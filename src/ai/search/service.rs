//! The search service: scoping, hybrid recall and result shaping.

use super::*;

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
        embedding_dimensions: usize,
    ) -> Result<Self> {
        Self::with_vector_index(
            db_path,
            embedding_provider,
            embedding_model,
            embedding_dimensions,
            Arc::new(SqliteVectorIndex),
        )
    }

    pub fn new_with_execution_identity(
        db_path: impl Into<PathBuf>,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        embedding_model: impl Into<String>,
        embedding_dimensions: usize,
        embedding_execution_identity: impl Into<String>,
    ) -> Result<Self> {
        let identity = embedding_execution_identity.into();
        if identity.trim().is_empty() || identity.trim() != identity {
            bail!("embedding execution identity is required");
        }
        let mut service = Self::new(
            db_path,
            embedding_provider,
            embedding_model,
            embedding_dimensions,
        )?;
        service.embedding_execution_identity = Some(identity);
        Ok(service)
    }

    pub fn with_vector_index(
        db_path: impl Into<PathBuf>,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        embedding_model: impl Into<String>,
        embedding_dimensions: usize,
        vector_index: Arc<dyn VectorIndex>,
    ) -> Result<Self> {
        let embedding_model = embedding_model.into();
        if embedding_model.trim().is_empty() {
            bail!("embedding model is required");
        }
        ensure!(
            embedding_dimensions > 0 && embedding_dimensions <= crate::ai::MAX_EMBEDDING_DIMENSIONS,
            "embedding dimensions must be between 1 and {}",
            crate::ai::MAX_EMBEDDING_DIMENSIONS
        );
        Ok(Self {
            db_path: db_path.into(),
            embedding_provider,
            embedding_model,
            embedding_dimensions,
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

    pub(crate) fn keyword_candidates(
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

    pub(crate) async fn semantic_candidates(
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

    pub(crate) async fn embed_query(&self, input: String) -> Result<Vec<f32>> {
        let response = self
            .embedding_provider
            .embeddings(EmbeddingRequest {
                model: self.embedding_model.clone(),
                input: vec![input],
                dimensions: Some(self.embedding_dimensions),
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
