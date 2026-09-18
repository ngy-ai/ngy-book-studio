//! Scoped keyword, semantic and hybrid document search.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::{FutureExt as _, future::BoxFuture};

pub use crate::ai::agent::{SearchMode, SearchRequest};
use crate::{
    ai::agent::{PassageRecord, SearchBackend},
    ai::{EmbeddingRequest, OpenAiCompatibleProvider},
    db::{connection, embeddings, search_chunks},
    document::{DocumentLocator, Revision, SourceLocator},
};

mod rank;

mod service;

mod vector;

pub use vector::*;

pub(crate) use rank::*;

#[cfg(test)]
mod tests;
pub(crate) const MAX_QUERY_BYTES: usize = 4 * 1024;
pub(crate) const MAX_RESULTS: usize = 256;
pub(crate) const MAX_ALLOWED_BOOKS: usize = 512;
pub(crate) const CANDIDATE_MULTIPLIER: usize = 4;
pub(crate) const RRF_K: f64 = 60.0;

#[derive(Clone)]
pub struct SearchService {
    pub(crate) db_path: PathBuf,
    pub(crate) embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) embedding_model: String,
    pub(crate) embedding_dimensions: usize,
    pub(crate) embedding_execution_identity: Option<String>,
    pub(crate) vector_index: Arc<dyn VectorIndex>,
}
