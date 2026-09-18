//! Shared fixtures for the search tests.

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

pub(crate) struct MockProvider {
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
pub(crate) struct FailingVectorIndex {
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

pub(crate) struct StaticVectorIndex {
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

pub(crate) struct Fixture {
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

pub(crate) fn insert_book(conn: &Connection, book_id: &str, title: &str, bodies: &[&str]) {
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

pub(crate) fn request(mode: SearchMode, book_ids: Vec<&str>) -> SearchRequest {
    SearchRequest {
        query: "common".to_string(),
        book_ids: book_ids.into_iter().map(str::to_string).collect(),
        mode,
        limit: 10,
    }
}

mod hybrid_test;
