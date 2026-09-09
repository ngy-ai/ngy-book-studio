//! Scoped keyword, semantic and hybrid document search.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use futures_util::{FutureExt as _, future::BoxFuture};

pub use crate::agent::{SearchMode, SearchRequest};
use crate::{
    agent::{PassageRecord, SearchBackend},
    ai::{EmbeddingRequest, OpenAiCompatibleProvider},
    db::{connection, embeddings, search_chunks},
    document::{DocumentLocator, Revision, SourceLocator},
};

const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_RESULTS: usize = 256;
const MAX_ALLOWED_BOOKS: usize = 512;
const CANDIDATE_MULTIPLIER: usize = 4;
const RRF_K: f64 = 60.0;

#[derive(Clone, Debug, PartialEq)]
pub struct ChunkEmbedding {
    pub id: String,
    pub search_chunk_id: String,
    pub model: String,
    pub vector: Vec<f32>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorHit {
    pub search_chunk_id: String,
    /// Cosine distance. Lower values rank first.
    pub distance: f64,
}

/// Storage and exact-query boundary for chunk embeddings.
///
/// Implementations must apply `allowed_book_ids` before ranking. SearchService
/// applies the same scope again while loading passage metadata so an incorrect
/// or remote implementation cannot widen the caller's authority.
pub trait VectorIndex: Send + Sync {
    fn upsert(&self, db_path: &Path, embedding: &ChunkEmbedding) -> Result<()>;

    fn get(
        &self,
        db_path: &Path,
        search_chunk_id: &str,
        model: &str,
    ) -> Result<Option<ChunkEmbedding>>;

    fn exact_search(
        &self,
        db_path: &Path,
        model: &str,
        query_vector: &[f32],
        allowed_book_ids: &[String],
        limit: usize,
    ) -> Result<Vec<VectorHit>>;

    /// Must enforce the model execution identity in the same snapshot as the
    /// vectors. Indices without that capability safely trigger FTS fallback.
    fn exact_search_for_execution(
        &self,
        _db_path: &Path,
        _model: &str,
        _execution_identity: &str,
        _query_vector: &[f32],
        _allowed_book_ids: &[String],
        _limit: usize,
    ) -> Result<Vec<VectorHit>> {
        bail!("vector index does not support execution identity checks")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SqliteVectorIndex;

impl VectorIndex for SqliteVectorIndex {
    fn upsert(&self, db_path: &Path, embedding: &ChunkEmbedding) -> Result<()> {
        let conn = connection::open_conn(db_path)?;
        embeddings::upsert_f32(
            &conn,
            &embedding.id,
            &embedding.search_chunk_id,
            &embedding.model,
            &embedding.vector,
            embedding.created_at,
        )?;
        Ok(())
    }

    fn get(
        &self,
        db_path: &Path,
        search_chunk_id: &str,
        model: &str,
    ) -> Result<Option<ChunkEmbedding>> {
        let conn = connection::open_conn(db_path)?;
        embeddings::get_f32_for_chunk_model(&conn, search_chunk_id, model).map(|stored| {
            stored.map(|(embedding, vector)| ChunkEmbedding {
                id: embedding.id,
                search_chunk_id: embedding.search_chunk_id,
                model: embedding.model,
                vector,
                created_at: embedding.created_at,
            })
        })
    }

    fn exact_search(
        &self,
        db_path: &Path,
        model: &str,
        query_vector: &[f32],
        allowed_book_ids: &[String],
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        let conn = connection::open_conn(db_path)?;
        embeddings::exact_knn_scoped(&conn, model, query_vector, allowed_book_ids, limit).map(
            |matches| {
                matches
                    .into_iter()
                    .map(|hit| VectorHit {
                        search_chunk_id: hit.search_chunk_id,
                        distance: hit.distance,
                    })
                    .collect()
            },
        )
    }

    fn exact_search_for_execution(
        &self,
        db_path: &Path,
        model: &str,
        execution_identity: &str,
        query_vector: &[f32],
        allowed_book_ids: &[String],
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        let conn = connection::open_conn(db_path)?;
        embeddings::exact_knn_scoped_for_execution(
            &conn,
            model,
            Some(execution_identity),
            query_vector,
            allowed_book_ids,
            limit,
        )
        .map(|matches| {
            matches
                .into_iter()
                .map(|hit| VectorHit {
                    search_chunk_id: hit.search_chunk_id,
                    distance: hit.distance,
                })
                .collect()
        })
    }
}

#[derive(Clone)]
pub struct SearchService {
    db_path: PathBuf,
    embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
    embedding_model: String,
    embedding_execution_identity: Option<String>,
    vector_index: Arc<dyn VectorIndex>,
}

impl std::fmt::Debug for SearchService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SearchService")
            .field("db_path", &self.db_path)
            .field("embedding_model", &self.embedding_model)
            .finish_non_exhaustive()
    }
}

impl SearchService {
    pub fn new(
        db_path: impl Into<PathBuf>,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        embedding_model: impl Into<String>,
    ) -> Result<Self> {
        Self::with_vector_index(
            db_path,
            embedding_provider,
            embedding_model,
            Arc::new(SqliteVectorIndex),
        )
    }

    pub fn new_with_execution_identity(
        db_path: impl Into<PathBuf>,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        embedding_model: impl Into<String>,
        embedding_execution_identity: impl Into<String>,
    ) -> Result<Self> {
        let identity = embedding_execution_identity.into();
        if identity.trim().is_empty() || identity.trim() != identity {
            bail!("embedding execution identity is required");
        }
        let mut service = Self::new(db_path, embedding_provider, embedding_model)?;
        service.embedding_execution_identity = Some(identity);
        Ok(service)
    }

    pub fn with_vector_index(
        db_path: impl Into<PathBuf>,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        embedding_model: impl Into<String>,
        vector_index: Arc<dyn VectorIndex>,
    ) -> Result<Self> {
        let embedding_model = embedding_model.into();
        if embedding_model.trim().is_empty() {
            bail!("embedding model is required");
        }
        Ok(Self {
            db_path: db_path.into(),
            embedding_provider,
            embedding_model,
            embedding_execution_identity: None,
            vector_index,
        })
    }

    pub fn embedding_model(&self) -> &str {
        &self.embedding_model
    }

    pub fn upsert_embedding(&self, embedding: &ChunkEmbedding) -> Result<()> {
        self.vector_index.upsert(&self.db_path, embedding)
    }

    pub fn embedding(&self, search_chunk_id: &str, model: &str) -> Result<Option<ChunkEmbedding>> {
        self.vector_index.get(&self.db_path, search_chunk_id, model)
    }

    pub async fn embed_and_upsert(
        &self,
        embedding_id: impl Into<String>,
        search_chunk_id: impl Into<String>,
        text: impl Into<String>,
        created_at: u64,
    ) -> Result<ChunkEmbedding> {
        let vector = self.embed_query(text.into()).await?;
        let embedding = ChunkEmbedding {
            id: embedding_id.into(),
            search_chunk_id: search_chunk_id.into(),
            model: self.embedding_model.clone(),
            vector,
            created_at,
        };
        self.upsert_embedding(&embedding)?;
        Ok(embedding)
    }

    pub async fn search(&self, request: SearchRequest) -> Result<Vec<PassageRecord>> {
        let allowed_book_ids = normalize_scope(request.book_ids)?;
        if allowed_book_ids.is_empty() {
            return Ok(Vec::new());
        }
        let query = request.query.trim();
        if query.is_empty() {
            bail!("search query must not be empty");
        }
        if query.len() > MAX_QUERY_BYTES {
            bail!("search query exceeds {MAX_QUERY_BYTES} bytes");
        }
        if request.limit == 0 || request.limit > MAX_RESULTS {
            bail!("search limit must be between 1 and {MAX_RESULTS}");
        }
        let candidate_limit = request
            .limit
            .saturating_mul(CANDIDATE_MULTIPLIER)
            .min(MAX_RESULTS);

        match request.mode {
            SearchMode::Keyword => Ok(self
                .keyword_candidates(query, &allowed_book_ids, candidate_limit)?
                .into_iter()
                .take(request.limit)
                .map(|candidate| candidate.record)
                .collect()),
            SearchMode::Semantic => {
                match self
                    .semantic_candidates(query, &allowed_book_ids, candidate_limit)
                    .await
                {
                    Ok(candidates) if !candidates.is_empty() => Ok(candidates
                        .into_iter()
                        .take(request.limit)
                        .map(|candidate| candidate.record)
                        .collect()),
                    Ok(_) => {
                        tracing::warn!(
                            model = %self.embedding_model,
                            book_count = allowed_book_ids.len(),
                            "语义索引没有可用结果，已降级为全文搜索"
                        );
                        Ok(self
                            .keyword_candidates(query, &allowed_book_ids, candidate_limit)?
                            .into_iter()
                            .take(request.limit)
                            .map(|candidate| candidate.record)
                            .collect())
                    }
                    Err(error) => {
                        tracing::warn!(
                            model = %self.embedding_model,
                            book_count = allowed_book_ids.len(),
                            %error,
                            "向量搜索不可用，已降级为全文搜索"
                        );
                        Ok(self
                            .keyword_candidates(query, &allowed_book_ids, candidate_limit)?
                            .into_iter()
                            .take(request.limit)
                            .map(|candidate| candidate.record)
                            .collect())
                    }
                }
            }
            SearchMode::Hybrid => {
                let keyword = self.keyword_candidates(query, &allowed_book_ids, candidate_limit)?;
                match self
                    .semantic_candidates(query, &allowed_book_ids, candidate_limit)
                    .await
                {
                    Ok(semantic) if !semantic.is_empty() => {
                        Ok(reciprocal_rank_fusion(keyword, semantic, request.limit))
                    }
                    Ok(_) => {
                        tracing::warn!(
                            model = %self.embedding_model,
                            book_count = allowed_book_ids.len(),
                            "语义索引没有可用结果，Hybrid 已降级为全文搜索"
                        );
                        Ok(keyword
                            .into_iter()
                            .take(request.limit)
                            .map(|candidate| candidate.record)
                            .collect())
                    }
                    Err(error) => {
                        tracing::warn!(
                            model = %self.embedding_model,
                            book_count = allowed_book_ids.len(),
                            %error,
                            "向量搜索不可用，Hybrid 已降级为全文搜索"
                        );
                        Ok(keyword
                            .into_iter()
                            .take(request.limit)
                            .map(|candidate| candidate.record)
                            .collect())
                    }
                }
            }
        }
    }

    fn keyword_candidates(
        &self,
        query: &str,
        allowed_book_ids: &[String],
        limit: usize,
    ) -> Result<Vec<RankedPassage>> {
        let conn = connection::open_conn(&self.db_path)?;
        search_chunks::search_fts_scoped(&conn, query, allowed_book_ids, limit)?
            .into_iter()
            .map(|hit| {
                // FTS5 BM25 is ordered ascending. Expose its negation so the
                // public relevance value follows a conventional higher-is-better scale.
                passage_from_document(hit.document, -hit.rank)
            })
            .collect()
    }

    async fn semantic_candidates(
        &self,
        query: &str,
        allowed_book_ids: &[String],
        limit: usize,
    ) -> Result<Vec<RankedPassage>> {
        let query_vector = self.embed_query(query.to_string()).await?;
        let hits = match self.embedding_execution_identity.as_deref() {
            Some(identity) => self.vector_index.exact_search_for_execution(
                &self.db_path,
                &self.embedding_model,
                identity,
                &query_vector,
                allowed_book_ids,
                limit,
            )?,
            None => self.vector_index.exact_search(
                &self.db_path,
                &self.embedding_model,
                &query_vector,
                allowed_book_ids,
                limit,
            )?,
        };
        if hits.is_empty() {
            return Ok(Vec::new());
        }

        let chunk_ids = hits
            .iter()
            .map(|hit| hit.search_chunk_id.clone())
            .collect::<Vec<_>>();
        let conn = connection::open_conn(&self.db_path)?;
        let documents = search_chunks::get_documents_scoped(&conn, &chunk_ids, allowed_book_ids)?;
        let mut documents = documents
            .into_iter()
            .map(|document| (document.chunk.id.clone(), document))
            .collect::<HashMap<_, _>>();

        let mut passages = Vec::with_capacity(hits.len());
        for hit in hits {
            if !hit.distance.is_finite() {
                bail!("vector index returned a non-finite distance");
            }
            if let Some(document) = documents.remove(&hit.search_chunk_id) {
                passages.push(passage_from_document(document, 1.0 - hit.distance)?);
            }
        }
        Ok(passages)
    }

    async fn embed_query(&self, input: String) -> Result<Vec<f32>> {
        let response = self
            .embedding_provider
            .embeddings(EmbeddingRequest {
                model: self.embedding_model.clone(),
                input: vec![input],
            })
            .await
            .context("embedding request failed")?;
        if response.vectors.len() != 1 {
            bail!("embedding response must contain exactly one vector");
        }
        let vector = response
            .vectors
            .into_iter()
            .next()
            .expect("length checked above");
        // Reuse the persistence validation so queries and stored vectors obey
        // identical finite/non-zero cosine-vector rules.
        embeddings::encode_f32(&vector)?;
        Ok(vector)
    }
}

impl SearchBackend for SearchService {
    fn search(&self, request: SearchRequest) -> BoxFuture<'_, Result<Vec<PassageRecord>>> {
        async move { SearchService::search(self, request).await }.boxed()
    }
}

#[derive(Clone, Debug)]
struct RankedPassage {
    record: PassageRecord,
}

fn passage_from_document(
    document: search_chunks::SearchChunkDocument,
    relevance: f64,
) -> Result<RankedPassage> {
    let locator = serde_json::from_str::<DocumentLocator>(&document.chunk.locator_json)
        .context("search chunk locator is invalid")?;
    if locator.book_id != document.chunk.book_id
        || locator.unit_id != document.chunk.content_unit_id
    {
        bail!("search chunk locator does not match its book and content unit");
    }
    if matches!(
        locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    ) {
        bail!("preview-only Office rendered pages cannot be returned by search");
    }
    Ok(RankedPassage {
        record: PassageRecord {
            passage_id: document.chunk.id,
            book_id: document.chunk.book_id,
            book_title: document.book_title,
            unit_id: document.chunk.content_unit_id,
            unit_title: document.unit_title,
            document_revision: Revision::new(document.document_revision),
            unit_revision: Revision::new(document.unit_revision),
            text: document.chunk.body,
            locator,
            relevance: Some(relevance),
        },
    })
}

fn reciprocal_rank_fusion(
    keyword: Vec<RankedPassage>,
    semantic: Vec<RankedPassage>,
    limit: usize,
) -> Vec<PassageRecord> {
    struct FusionEntry {
        record: PassageRecord,
        score: f64,
        best_rank: usize,
    }

    let mut entries = HashMap::<String, FusionEntry>::new();
    for candidates in [keyword, semantic] {
        for (index, candidate) in candidates.into_iter().enumerate() {
            let rank = index + 1;
            let entry = entries
                .entry(candidate.record.passage_id.clone())
                .or_insert_with(|| FusionEntry {
                    record: candidate.record,
                    score: 0.0,
                    best_rank: rank,
                });
            entry.score += 1.0 / (RRF_K + rank as f64);
            entry.best_rank = entry.best_rank.min(rank);
        }
    }

    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.best_rank.cmp(&right.best_rank))
            .then_with(|| left.record.passage_id.cmp(&right.record.passage_id))
    });
    entries
        .into_iter()
        .take(limit)
        .map(|mut entry| {
            entry.record.relevance = Some(entry.score);
            entry.record
        })
        .collect()
}

fn normalize_scope(book_ids: Vec<String>) -> Result<Vec<String>> {
    if book_ids.len() > MAX_ALLOWED_BOOKS {
        bail!("book scope exceeds {MAX_ALLOWED_BOOKS} ids");
    }
    let mut seen = HashSet::with_capacity(book_ids.len());
    Ok(book_ids
        .into_iter()
        .filter(|book_id| !book_id.is_empty() && seen.insert(book_id.clone()))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use anyhow::anyhow;
    use rusqlite::{Connection, params};

    use super::*;
    use crate::ai::{
        ChatEventStream, ChatRequest, EmbeddingBatch, ModelInfo, OpenAiCompatibleProvider,
    };

    struct MockProvider {
        calls: AtomicUsize,
        vector: Vec<f32>,
        response_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    }

    impl MockProvider {
        fn new(vector: Vec<f32>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                vector,
                response_gate: None,
            }
        }
    }

    impl OpenAiCompatibleProvider for MockProvider {
        fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
            async { Ok(Vec::new()) }.boxed()
        }

        fn chat_stream(&self, _request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
            async { Err(anyhow!("chat is not used by search tests")) }.boxed()
        }

        fn embeddings(&self, request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let vector = self.vector.clone();
            let response_gate = self.response_gate.clone();
            async move {
                if let Some((entered, release)) = response_gate {
                    entered.notify_one();
                    release.notified().await;
                }
                Ok(EmbeddingBatch {
                    model: request.model,
                    vectors: vec![vector],
                    usage: None,
                })
            }
            .boxed()
        }
    }

    #[derive(Default)]
    struct FailingVectorIndex {
        calls: AtomicUsize,
        scopes: Mutex<Vec<Vec<String>>>,
    }

    impl VectorIndex for FailingVectorIndex {
        fn upsert(&self, _db_path: &Path, _embedding: &ChunkEmbedding) -> Result<()> {
            Err(anyhow!("vector index unavailable"))
        }

        fn get(
            &self,
            _db_path: &Path,
            _search_chunk_id: &str,
            _model: &str,
        ) -> Result<Option<ChunkEmbedding>> {
            Err(anyhow!("vector index unavailable"))
        }

        fn exact_search(
            &self,
            _db_path: &Path,
            _model: &str,
            _query_vector: &[f32],
            allowed_book_ids: &[String],
            _limit: usize,
        ) -> Result<Vec<VectorHit>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.scopes.lock().unwrap().push(allowed_book_ids.to_vec());
            Err(anyhow!("vector index unavailable"))
        }
    }

    struct StaticVectorIndex {
        hits: Vec<VectorHit>,
    }

    impl VectorIndex for StaticVectorIndex {
        fn upsert(&self, _db_path: &Path, _embedding: &ChunkEmbedding) -> Result<()> {
            Ok(())
        }

        fn get(
            &self,
            _db_path: &Path,
            _search_chunk_id: &str,
            _model: &str,
        ) -> Result<Option<ChunkEmbedding>> {
            Ok(None)
        }

        fn exact_search(
            &self,
            _db_path: &Path,
            _model: &str,
            _query_vector: &[f32],
            _allowed_book_ids: &[String],
            limit: usize,
        ) -> Result<Vec<VectorHit>> {
            Ok(self.hits.iter().take(limit).cloned().collect())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(connection::DATABASE_FILE);
            let conn = connection::open_or_recreate(&path).unwrap();
            insert_book(
                &conn,
                "book-a",
                "Allowed book",
                &["common alpha", "common beta"],
            );
            insert_book(&conn, "book-b", "Blocked book", &["common alpha blocked"]);
            drop(conn);
            Self { _temp: temp, path }
        }

        fn set_embedding_execution(&self, identity: &str) {
            let mut conn = connection::open_conn(&self.path).unwrap();
            let source = crate::db::book_sources::get(&conn, "book-a-source")
                .unwrap()
                .unwrap();
            crate::db::transactions::reconcile_current_embedding_job(
                &mut conn,
                &source,
                "mock-model",
                identity,
                "vision-endpoint",
                10,
            )
            .unwrap();
        }

        fn put_semantic_only_hit(&self) {
            SqliteVectorIndex
                .upsert(
                    &self.path,
                    &ChunkEmbedding {
                        id: "semantic-hit".to_string(),
                        search_chunk_id: "book-a-chunk-1".to_string(),
                        model: "mock-model".to_string(),
                        vector: vec![1.0, 0.0],
                        created_at: 11,
                    },
                )
                .unwrap();
        }
    }

    fn insert_book(conn: &Connection, book_id: &str, title: &str, bodies: &[&str]) {
        let object_key = format!("{book_id}-source-object");
        let source_id = format!("{book_id}-source");
        let unit_id = format!("{book_id}-unit");
        conn.execute(
            "INSERT INTO blobs (object_key, media_type, byte_len, hash, created_at)
             VALUES (?1, 'application/epub+zip', 1, ?2, 1)",
            params![object_key, format!("{book_id}-source-hash")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO books
             (id, title, author, language, description, format, revision,
              source_object_key, cover_asset_id, added_at, updated_at, group_id)
             VALUES (?1, ?2, 'Author', NULL, NULL, 'epub', 1, ?3, NULL, 1, 1, NULL)",
            params![book_id, title, object_key],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO book_sources
             (id, book_id, revision, format, source_kind, object_key, source_name, created_at)
             VALUES (?1, ?2, 1, 'epub', 'imported', ?3, NULL, 1)",
            params![source_id, book_id, object_key],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO content_units
             (id, book_id, source_id, parent_id, ordinal, kind, href,
              source_locator_json, title, media_type, source_text, block_json,
              revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, NULL, 0, 'chapter', 'chapter.xhtml',
                     '{}', 'Chapter', 'text/html', '', '{}', 1, 1, 1)",
            params![unit_id, book_id, source_id],
        )
        .unwrap();
        for (ordinal, body) in bodies.iter().enumerate() {
            let chunk_id = format!("{book_id}-chunk-{ordinal}");
            let locator = serde_json::to_string(&DocumentLocator::unit(book_id, &unit_id)).unwrap();
            conn.execute(
                "INSERT INTO search_chunks
                 (id, book_id, source_id, content_unit_id, ordinal, heading, body,
                  token_count, content_hash, locator_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'Chapter', ?6, 2, ?7, ?8, 1)",
                params![
                    chunk_id,
                    book_id,
                    source_id,
                    unit_id,
                    ordinal as i64,
                    body,
                    format!("{book_id}-{ordinal}-hash"),
                    locator,
                ],
            )
            .unwrap();
        }
    }

    fn request(mode: SearchMode, book_ids: Vec<&str>) -> SearchRequest {
        SearchRequest {
            query: "common".to_string(),
            book_ids: book_ids.into_iter().map(str::to_string).collect(),
            mode,
            limit: 10,
        }
    }

    #[tokio::test]
    async fn endpoint_switch_during_query_embedding_falls_back_to_scoped_fts() {
        let fixture = Fixture::new();
        fixture.set_embedding_execution("endpoint-a:mock-model");
        fixture.put_semantic_only_hit();
        let mut query = request(SearchMode::Semantic, vec!["book-a"]);
        query.query = "alpha".to_string();
        let current = SearchService::new_with_execution_identity(
            &fixture.path,
            Arc::new(MockProvider::new(vec![1.0, 0.0])),
            "mock-model",
            "endpoint-a:mock-model",
        )
        .unwrap();
        let hits = current.search(query.clone()).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].passage_id, "book-a-chunk-1");

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut provider = MockProvider::new(vec![1.0, 0.0]);
        provider.response_gate = Some((entered.clone(), release.clone()));
        let snapshot = SearchService::new_with_execution_identity(
            &fixture.path,
            Arc::new(provider),
            "mock-model",
            "endpoint-a:mock-model",
        )
        .unwrap();
        let pending_query = query.clone();
        let pending = tokio::spawn(async move { snapshot.search(pending_query).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();

        // Same model name and dimensions, but the endpoint's vector space has
        // changed while the old snapshot's provider response was pending.
        fixture.set_embedding_execution("endpoint-b:mock-model");
        fixture.put_semantic_only_hit();
        release.notify_one();
        let hits = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].passage_id, "book-a-chunk-0");
        assert_eq!(hits[0].book_id, "book-a");

        let replacement = SearchService::new_with_execution_identity(
            &fixture.path,
            Arc::new(MockProvider::new(vec![1.0, 0.0])),
            "mock-model",
            "endpoint-b:mock-model",
        )
        .unwrap();
        let hits = replacement.search(query).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].passage_id, "book-a-chunk-1");
    }

    #[test]
    fn sqlite_execution_identity_filter_rejects_unbound_and_malformed_jobs() {
        let fixture = Fixture::new();
        fixture.put_semantic_only_hit();
        let lookup = || {
            SqliteVectorIndex
                .exact_search_for_execution(
                    &fixture.path,
                    "mock-model",
                    "endpoint-a:mock-model",
                    &[1.0, 0.0],
                    &["book-a".to_string()],
                    10,
                )
                .unwrap()
        };
        assert!(lookup().is_empty());
        fixture.set_embedding_execution("endpoint-a:mock-model");
        fixture.put_semantic_only_hit();
        assert_eq!(lookup().len(), 1);
        let conn = connection::open_conn(&fixture.path).unwrap();
        for cursor in ["null", "{"] {
            conn.execute(
                "UPDATE index_jobs SET cursor_json = ?1
                 WHERE id = 'embedding:book-a-source'",
                [cursor],
            )
            .unwrap();
            assert!(lookup().is_empty());
        }
    }

    #[tokio::test]
    async fn empty_scope_short_circuits_without_database_or_embedding_access() {
        let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
        let vector_index = Arc::new(FailingVectorIndex::default());
        let service = SearchService::with_vector_index(
            PathBuf::from("database-must-not-be-opened.db"),
            provider.clone(),
            "mock-model",
            vector_index.clone(),
        )
        .unwrap();

        let hits = service
            .search(request(SearchMode::Hybrid, Vec::new()))
            .await
            .unwrap();

        assert!(hits.is_empty());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(vector_index.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn hybrid_falls_back_to_scoped_fts_when_vector_search_fails() {
        let fixture = Fixture::new();
        let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
        let vector_index = Arc::new(FailingVectorIndex::default());
        let service = SearchService::with_vector_index(
            &fixture.path,
            provider.clone(),
            "mock-model",
            vector_index.clone(),
        )
        .unwrap();

        let hits = service
            .search(request(SearchMode::Hybrid, vec!["book-a"]))
            .await
            .unwrap();

        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.book_id == "book-a"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(vector_index.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            vector_index.scopes.lock().unwrap().as_slice(),
            &[vec!["book-a".to_string()]]
        );
    }

    #[tokio::test]
    async fn service_rechecks_scope_for_untrusted_vector_hits() {
        let fixture = Fixture::new();
        let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
        let vector_index = Arc::new(StaticVectorIndex {
            hits: vec![
                VectorHit {
                    search_chunk_id: "book-b-chunk-0".to_string(),
                    distance: 0.0,
                },
                VectorHit {
                    search_chunk_id: "book-a-chunk-0".to_string(),
                    distance: 0.1,
                },
            ],
        });
        let service =
            SearchService::with_vector_index(&fixture.path, provider, "mock-model", vector_index)
                .unwrap();

        let hits = service
            .search(request(SearchMode::Semantic, vec!["book-a"]))
            .await
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].passage_id, "book-a-chunk-0");
    }

    #[tokio::test]
    async fn preview_only_office_chunks_are_excluded_from_keyword_and_semantic_search() {
        let fixture = Fixture::new();
        let conn = connection::open_conn(&fixture.path).unwrap();
        let locator = serde_json::to_string(
            &DocumentLocator::unit("book-a", "book-a-unit")
                .with_source(SourceLocator::office_rendered_page(1)),
        )
        .unwrap();
        conn.execute(
            "UPDATE search_chunks SET locator_json = ?2 WHERE id = ?1",
            params!["book-a-chunk-0", locator],
        )
        .unwrap();
        drop(conn);

        let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
        let vector_index = Arc::new(StaticVectorIndex {
            hits: vec![
                VectorHit {
                    search_chunk_id: "book-a-chunk-0".to_string(),
                    distance: 0.0,
                },
                VectorHit {
                    search_chunk_id: "book-a-chunk-1".to_string(),
                    distance: 0.1,
                },
            ],
        });
        let service =
            SearchService::with_vector_index(&fixture.path, provider, "mock-model", vector_index)
                .unwrap();

        for mode in [SearchMode::Keyword, SearchMode::Semantic] {
            let hits = service.search(request(mode, vec!["book-a"])).await.unwrap();
            assert_eq!(
                hits.iter()
                    .map(|hit| hit.passage_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["book-a-chunk-1"]
            );
        }
    }

    #[test]
    fn sqlite_vector_index_upserts_reads_and_exactly_ranks_with_hard_scope() {
        let fixture = Fixture::new();
        let index = SqliteVectorIndex;
        for (id, chunk_id, vector) in [
            ("embedding-a-near", "book-a-chunk-0", vec![0.9, 0.1]),
            ("embedding-a-far", "book-a-chunk-1", vec![0.0, 1.0]),
            ("embedding-b-exact", "book-b-chunk-0", vec![1.0, 0.0]),
        ] {
            index
                .upsert(
                    &fixture.path,
                    &ChunkEmbedding {
                        id: id.to_string(),
                        search_chunk_id: chunk_id.to_string(),
                        model: "mock-model".to_string(),
                        vector,
                        created_at: 7,
                    },
                )
                .unwrap();
        }

        let stored = index
            .get(&fixture.path, "book-a-chunk-0", "mock-model")
            .unwrap()
            .unwrap();
        assert_eq!(stored.vector, vec![0.9, 0.1]);

        let hits = index
            .exact_search(
                &fixture.path,
                "mock-model",
                &[1.0, 0.0],
                &["book-a".to_string()],
                10,
            )
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| hit.search_chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["book-a-chunk-0", "book-a-chunk-1"]
        );
        assert!(hits[0].distance < hits[1].distance);
        assert!(
            hits.iter()
                .all(|hit| !hit.search_chunk_id.starts_with("book-b"))
        );
        assert!(
            index
                .exact_search(&fixture.path, "mock-model", &[1.0, 0.0], &[], 10,)
                .unwrap()
                .is_empty()
        );

        let conn = connection::open_conn(&fixture.path).unwrap();
        let locator = serde_json::to_string(
            &DocumentLocator::unit("book-a", "book-a-unit")
                .with_source(SourceLocator::office_rendered_page(1)),
        )
        .unwrap();
        conn.execute(
            "UPDATE search_chunks SET locator_json = ?2 WHERE id = ?1",
            params!["book-a-chunk-0", locator],
        )
        .unwrap();
        drop(conn);
        let hits = index
            .exact_search(
                &fixture.path,
                "mock-model",
                &[1.0, 0.0],
                &["book-a".to_string()],
                10,
            )
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| hit.search_chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["book-a-chunk-1"]
        );
    }

    #[test]
    fn reciprocal_rank_fusion_rewards_passages_found_by_both_rankers() {
        fn ranked(id: &str) -> RankedPassage {
            RankedPassage {
                record: PassageRecord {
                    passage_id: id.to_string(),
                    book_id: "book-a".to_string(),
                    book_title: "Book".to_string(),
                    unit_id: "unit".to_string(),
                    unit_title: "Unit".to_string(),
                    document_revision: Revision::new(1),
                    unit_revision: Revision::new(1),
                    text: id.to_string(),
                    locator: DocumentLocator::unit("book-a", "unit"),
                    relevance: None,
                },
            }
        }

        let fused = reciprocal_rank_fusion(
            vec![ranked("a"), ranked("b")],
            vec![ranked("b"), ranked("c")],
            3,
        );

        assert_eq!(fused[0].passage_id, "b");
        assert!(fused[0].relevance.unwrap() > fused[1].relevance.unwrap());
    }
}
