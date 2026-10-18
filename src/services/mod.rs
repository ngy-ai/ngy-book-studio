//! Process-level service composition.
//!
//! `AppServices` owns the durable store, parsers, model provider, search,
//! conversations and background workers. UI code receives cloneable handles
//! and schedules blocking library operations through [`AppServices::spawn_library`].

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

pub use crate::ai::indexing::{
    TranslationBlockDetail, TranslationBlockInfo, TranslationBlockList, TranslationBlockProbe,
    TranslationBlockRequest,
};
pub use crate::ai::job_diagnostics::{
    BACKGROUND_JOB_LOG_LIMIT, BackgroundJobLogEntry, BackgroundJobLogSnapshot, JobLogLevel,
};

use crate::djvu_renderer::{DJVU_RENDERER_NAME, DjvuPngRenderer};
#[cfg(target_os = "windows")]
use crate::office_visual::{OFFICE_ENHANCED_RENDERER_NAME, OfficeEnhancedRenderer};
#[cfg(target_os = "windows")]
use crate::windows_pdf_renderer::WindowsPdfRenderer;
use crate::{
    ai::agent::WebSearchBackend,
    ai::chat::ChatRepository,
    ai::credentials::{CredentialStore, SystemCredentialStore},
    ai::indexing::{IndexingCoordinator, IndexingJobStatus, IndexingModelConfig},
    ai::search::SearchService,
    ai::web_search::{HttpWebSearch, WebSearchConfig, normalize_web_endpoint},
    ai::{
        ChatGenerationSettings, DEFAULT_AI_REQUEST_TIMEOUT_SECS, DEFAULT_AI_USE_PROXY,
        DEFAULT_OLLAMA_OPENAI_BASE_URL, EndpointBudget, EndpointRateLimits, MAX_ENDPOINT_RPM,
        ModelInfo, OpenAiCompatibleProvider, OpenAiHttpProvider, ProviderConfig, RequestClass,
        TranslationGenerationSettings, default_endpoint_requests_per_minute,
        normalize_provider_base_url,
    },
    db,
    formats::FormatRegistry,
    library::{LibraryStore, reclaim_unreferenced_blobs},
    office_com::{OfficeComWorker, OfficeEnhancer},
    preview::{
        PreviewFuture, RenderProfile, RenderedVisualPage, RendererDescriptor, SqliteVisualJobStore,
        StructuralPngRenderer, VisualAssetPayload, VisualDocumentSource, VisualJobCoordinator,
        VisualJobState, VisualPageSink, VisualRenderer, VisualSourcePayload,
        office_preview_unit_id,
    },
    runtime::IoRuntime,
    storage::{BlobKey, BlobPublicationLock, BlobStore, LocalBlobStore},
    theme::Appearance,
};

mod background_jobs;

mod app;

mod helpers;

mod jobs;

mod mutation;

mod provider;

mod settings;

mod translation_service;

mod visual;

pub(crate) use helpers::*;

pub(crate) use jobs::*;

pub(crate) use mutation::*;

pub(crate) use settings::*;

// The public service surface, unchanged by the split.
pub use jobs::{
    BackgroundJobAction, BackgroundJobProgress, BackgroundJobSnapshot, BackgroundJobStatus,
    PublishedVisualPage, ReaderTocLabel, TranslatedBlock, VisualPageSourceKind,
};
pub use mutation::LibraryMutation;
pub use settings::{
    ApiKeyUpdate, DEFAULT_BACKGROUND_JOB_CONCURRENCY, DEFAULT_BACKGROUND_JOB_INTERVAL_MS,
    DEFAULT_CHAT_MODEL, DEFAULT_EMBEDDING_DIMENSIONS, DEFAULT_EMBEDDING_MODEL, DEFAULT_ENDPOINT_ID,
    DEFAULT_SELECTION_EXPLANATION_QUESTION, DEFAULT_TRANSLATION_MODEL, DEFAULT_VISION_MODEL,
    EndpointRoutingSettings, EndpointSettings, MAX_BACKGROUND_JOB_CONCURRENCY,
    MAX_BACKGROUND_JOB_INTERVAL_MS, MIN_BACKGROUND_JOB_CONCURRENCY, ModelRole, ProviderSettings,
    ReaderZoomSurface, TRANSLATION_LANGUAGES, TranslationDisplayMode, translation_language_label,
};

#[cfg(test)]
mod tests;
/// Shared application services. Construct once and pass an `Arc<AppServices>`
/// to windows. No method that performs SQLite or document work runs it on the
/// calling (GPUI) thread.
pub struct AppServices {
    data_dir: PathBuf,
    db_path: PathBuf,
    library: Arc<Mutex<LibraryStore>>,
    learning: Arc<crate::learning::LearningService>,
    library_generation: Arc<AtomicU64>,
    library_mutations: Arc<LibraryMutationQueue>,
    blobs: Arc<LocalBlobStore>,
    blob_publication: BlobPublicationLock,
    formats: Arc<FormatRegistry>,
    chat: ChatRepository,
    credentials: Arc<dyn CredentialStore>,
    ai: Arc<RwLock<AiServices>>,
    /// 各 Endpoint 的请求额度。它按 `base_url` 在进程里共享、活得比任何一批 provider
    /// 都久：设置保存会重建 provider，但已经记下的限速窗口不该跟着清零。
    endpoint_limits: Arc<EndpointRateLimits>,
    ai_configuration: Arc<tokio::sync::Mutex<()>>,
    office: Arc<dyn OfficeEnhancer>,
    indexing: Arc<IndexingCoordinator>,
    auto_run_background_jobs: Arc<AtomicBool>,
    visual_jobs: RwLock<Option<Arc<VisualJobCoordinator>>>,
    visual_renderer_descriptors: RwLock<Vec<RendererDescriptor>>,
    runtime: IoRuntime,
}

impl std::fmt::Debug for AppServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppServices")
            .field("data_dir", &self.data_dir)
            .field("db_path", &self.db_path)
            .field("formats", &self.formats)
            .field("ai", &self.ai)
            .field("indexing", &self.indexing)
            .field(
                "auto_run_background_jobs",
                &self.auto_run_background_jobs.load(Ordering::Acquire),
            )
            .field("has_visual_jobs", &self.visual_jobs().is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for AppServices {
    fn drop(&mut self) {
        if let Ok(slot) = self.visual_jobs.get_mut() {
            slot.take();
        }
    }
}
