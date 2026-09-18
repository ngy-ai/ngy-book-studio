use std::collections::{BTreeMap, HashSet};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "windows")]
use crate::windows_pdf_renderer::WindowsPdfRenderer;
use crate::{
    djvu_renderer::{DJVU_RENDERER_NAME, DjvuPngRenderer},
    document::{BlockDocument, DocumentLocator, Revision, SourceLocator, deterministic_id},
    preview::{
        RenderProfile, RendererDescriptor, StructuralPngRenderer, VisualJobSpec,
        decode_persisted_visual_job, encode_persisted_visual_job, office_preview_unit_id,
    },
};

use super::{
    asset_refs::{self, AssetRef},
    assets::{self, Asset},
    blobs::{self, BlobRecord},
    book_sources::{self, BookSource},
    books::{self, BookRecord},
    chat_citations::{self, ChatCitation},
    chat_messages::{self, ChatMessage},
    chat_threads,
    content_units::{self, ContentUnit},
    embeddings::{self, Embedding},
    groups::{self, BookGroup},
    index_jobs::{self, IndexJob, IndexJobStatus},
    office_enhancements,
    progress::{self, ReadingProgress},
    search_chunks::{self, SearchChunk},
    settings,
    toc_entries::{self, TocEntry},
    visual_page_staging,
    visual_pages::{self, VisualPage},
};

pub(crate) const MAX_STAGED_VISUAL_BYTES: u64 = 512 * 1024 * 1024;

/// The text was checked against immutable reader bytes by the caller. Recheck
/// the owning book and unit revisions under the write lock before publishing.
mod annotation;

mod chat;

mod document;

mod document_graph;

mod embedding_job;

mod group;

mod translation_job;

mod vision_job;

mod visual_page;

mod visual_render;

mod visual_search;

pub(crate) use annotation::*;

pub(crate) use chat::*;

pub(crate) use document::*;

pub(crate) use document_graph::*;

pub(crate) use embedding_job::*;

pub(crate) use group::*;

pub(crate) use translation_job::*;

pub(crate) use vision_job::*;

pub(crate) use visual_page::*;

pub(crate) use visual_render::*;

pub(crate) use visual_search::*;

#[cfg(test)]
mod tests;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisionJobReconciliation {
    Unchanged,
    Requeued,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PersistedIndexCursor {
    pub(crate) schema_version: u32,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) revision: u64,
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) execution_identity: Option<String>,
    #[serde(default)]
    pub(crate) input_execution_identity: Option<String>,
    #[serde(default)]
    pub(crate) next_ordinal: usize,
}

/// All SQLite metadata produced from one immutable source revision. Object
/// bytes must already have been durably written to `LocalBlobStore`; this value
/// only contains their metadata and object keys.
pub(crate) struct DocumentGraph<'a> {
    pub(crate) book: &'a BookRecord,
    pub(crate) source_blob: &'a BlobRecord,
    pub(crate) additional_blobs: &'a [BlobRecord],
    pub(crate) source: &'a BookSource,
    pub(crate) progress: Option<&'a ReadingProgress>,
    pub(crate) content_units: &'a [ContentUnit],
    pub(crate) toc_entries: &'a [TocEntry],
    pub(crate) assets: &'a [Asset],
    pub(crate) asset_refs: &'a [AssetRef],
    pub(crate) search_chunks: &'a [SearchChunk],
    pub(crate) visual_pages: &'a [VisualPage],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CurrentProgressWrite {
    Updated {
        source_revision: u64,
        spine_index: usize,
    },
    MissingBook,
    MissingUnit,
    UnitNotCurrent,
}

/// Rebuilds only derived visual work when the registered renderer or default
/// profile changes. Canonical content, original objects and text FTS remain
/// untouched; stale visual chunks (and their cascading embeddings) are
/// removed before vision is requeued from ordinal zero.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct VisualRenderReconciliation {
    pub(crate) changed_sources: usize,
    /// Visual page objects whose last database reference was removed by this
    /// transaction. Callers must still serialize with object publication and
    /// re-check the reference immediately before deleting the bytes.
    pub(crate) unreferenced_blobs: Vec<BlobRecord>,
}

#[derive(serde::Deserialize)]
pub(crate) struct PersistedChatScope {
    pub(crate) book_ids: Vec<String>,
}
