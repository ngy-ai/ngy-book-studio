//! The vector index the hybrid search reads through and the SQLite-backed
//! implementation of it.

use super::*;

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
