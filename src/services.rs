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

pub use crate::job_diagnostics::{
    BACKGROUND_JOB_LOG_LIMIT, BackgroundJobLogEntry, BackgroundJobLogSnapshot, JobLogLevel,
};

#[cfg(target_os = "windows")]
use crate::office_visual::{OFFICE_ENHANCED_RENDERER_NAME, OfficeEnhancedRenderer};
#[cfg(target_os = "windows")]
use crate::windows_pdf_renderer::WindowsPdfRenderer;
use crate::{
    agent::WebSearchBackend,
    ai::{
        ChatGenerationSettings, DEFAULT_AI_REQUEST_TIMEOUT_SECS, DEFAULT_OLLAMA_OPENAI_BASE_URL,
        ModelInfo, OpenAiCompatibleProvider, OpenAiHttpProvider, ProviderConfig,
        normalize_provider_base_url,
    },
    chat::ChatRepository,
    credentials::{CredentialStore, SystemCredentialStore},
    db,
    formats::FormatRegistry,
    indexing::{IndexingCoordinator, IndexingJobStatus, IndexingModelConfig},
    library::{LibraryStore, reclaim_unreferenced_blobs},
    office_com::{OfficeComWorker, OfficeEnhancer},
    preview::{
        PreviewFuture, RenderProfile, RenderedVisualPage, RendererDescriptor, SqliteVisualJobStore,
        StructuralPngRenderer, VisualAssetPayload, VisualDocumentSource, VisualJobCoordinator,
        VisualJobState, VisualPageSink, VisualRenderer, VisualSourcePayload,
        office_preview_unit_id,
    },
    runtime::IoRuntime,
    search::SearchService,
    storage::{BlobKey, BlobPublicationLock, BlobStore, LocalBlobStore},
    web_search::{HttpWebSearch, WebSearchConfig, normalize_web_endpoint},
};

const OBJECT_DIRECTORY: &str = "objects";
const PROVIDER_SETTINGS_KEY: &str = "ai.openai_compatible.provider.v1";
const CHAT_GENERATION_SETTINGS_KEY: &str = "ai.openai_compatible.chat_generation.v1";
const ENDPOINT_ROUTING_SETTINGS_KEY: &str = "ai.openai_compatible.endpoint_routing.v1";
const BACKGROUND_JOB_SETTINGS_KEY: &str = "background_jobs.preferences.v1";
const PDF_READER_SETTINGS_KEY: &str = "pdf.reader.preferences.v1";
const TRANSLATION_SETTINGS_KEY: &str = "translation.preferences.v1";
const WEB_SEARCH_CREDENTIAL_TARGET: &str = "ai.openai_compatible.web_search.v1";
const MAX_MODEL_NAME_CHARS: usize = 256;
#[cfg(target_os = "windows")]
const MAX_LOADED_OFFICE_PAGES: usize = 20_000;
#[cfg(target_os = "windows")]
const MAX_LOADED_OFFICE_PAGE_BYTES: u64 = 512 * 1024 * 1024;

pub const DEFAULT_CHAT_MODEL: &str = "qwen3.5:0.8b";
pub const DEFAULT_EMBEDDING_MODEL: &str = "qwen3-embedding:0.6b";
pub const DEFAULT_VISION_MODEL: &str = "qwen3.5:0.8b";
/// Default embedding vector dimension. When the configured value differs from
/// the previously saved one, all existing vector indices are invalidated and
/// must be regenerated.
pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 1024;
pub const DEFAULT_ENDPOINT_ID: &str = "default";

/// Preset target languages for the reading-time book translation, as
/// `(tag, label)` pairs. The tag is persisted verbatim and validated on save;
/// the label is only used by the AI settings window.
pub const TRANSLATION_LANGUAGES: [(&str, &str); 9] = [
    ("zh-Hans", "中文（简体）"),
    ("zh-Hant", "中文（繁體）"),
    ("en", "英语"),
    ("ja", "日语"),
    ("ko", "韩语"),
    ("fr", "法语"),
    ("de", "德语"),
    ("es", "西班牙语"),
    ("ru", "俄语"),
];

/// Human-readable label for one persisted language tag, or `None` when the tag
/// is not part of [`TRANSLATION_LANGUAGES`].
pub fn translation_language_label(tag: &str) -> Option<&'static str> {
    TRANSLATION_LANGUAGES
        .iter()
        .find(|(candidate, _)| *candidate == tag)
        .map(|(_, label)| *label)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRole {
    Chat,
    Embedding,
    Vision,
}

/// One independently authorized connection; credentials are stored by its
/// canonical URL in the operating-system credential store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointSettings {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub remote_content_confirmed: bool,
    pub allow_insecure_remote_http: bool,
    pub confirmed_remote_endpoint: String,
    pub request_timeout_secs: u64,
}

impl EndpointSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 128
                && self
                    .id
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_')),
            "Endpoint 标识无效"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.trim() == self.name
                && self.name.chars().count() <= 80
                && !self.name.chars().any(char::is_control),
            "Endpoint 名称须为 1 至 80 个字符，且不能包含首尾空白或控制字符"
        );
        self.provider_config(None)
            .validated_base_url()
            .with_context(|| format!("Endpoint「{}」配置无效", self.name))?;
        Ok(())
    }

    pub fn provider_config(&self, api_key: Option<String>) -> ProviderConfig {
        let confirmation_matches = normalize_provider_base_url(&self.base_url)
            .ok()
            .is_some_and(|url| self.confirmed_remote_endpoint == url.as_str());
        ProviderConfig {
            base_url: self.base_url.clone(),
            api_key,
            remote_content_confirmed: self.remote_content_confirmed && confirmation_matches,
            allow_insecure_remote_http: self.allow_insecure_remote_http && confirmation_matches,
            request_timeout_secs: self.request_timeout_secs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointRoutingSettings {
    pub default_endpoint_name: String,
    pub additional_endpoints: Vec<EndpointSettings>,
    pub chat_endpoint_id: String,
    pub embedding_endpoint_id: String,
    pub vision_endpoint_id: String,
}

impl Default for EndpointRoutingSettings {
    fn default() -> Self {
        Self {
            default_endpoint_name: "默认 Endpoint".into(),
            additional_endpoints: Vec::new(),
            chat_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
            embedding_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
            vision_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
        }
    }
}

/// New derivative jobs start paused until the user opts into automatic
/// execution in the AI settings window.
pub(crate) const DEFAULT_AUTO_RUN_BACKGROUND_JOBS: bool = false;

fn default_auto_run_background_jobs() -> bool {
    DEFAULT_AUTO_RUN_BACKGROUND_JOBS
}

/// Upper bound of background model jobs running at the same time. The indexing
/// worker spawns this many tasks once and parks the ones above the configured
/// concurrency, so raising or lowering the setting takes effect without a
/// restart.
pub const MAX_BACKGROUND_JOB_CONCURRENCY: usize = 8;
/// Fewer than one worker cannot make progress.
pub const MIN_BACKGROUND_JOB_CONCURRENCY: usize = 1;
/// One job at a time keeps memory and network pressure predictable; users on
/// capable machines can raise it deliberately.
pub const DEFAULT_BACKGROUND_JOB_CONCURRENCY: usize = 1;
/// Longest pause a worker waits between two background jobs.
pub const MAX_BACKGROUND_JOB_INTERVAL_MS: u64 = 60_000;
/// Default pause after a finished job. A short gap keeps a low-end machine from
/// being saturated by back-to-back model calls.
pub const DEFAULT_BACKGROUND_JOB_INTERVAL_MS: u64 = 10;

fn default_background_job_concurrency() -> usize {
    DEFAULT_BACKGROUND_JOB_CONCURRENCY
}

fn default_background_job_interval_ms() -> u64 {
    DEFAULT_BACKGROUND_JOB_INTERVAL_MS
}

fn default_embedding_dimensions() -> usize {
    DEFAULT_EMBEDDING_DIMENSIONS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedBackgroundJobSettings {
    auto_run: bool,
    /// `#[serde(default)]` keeps rows written before the scheduling options
    /// existed loadable.
    #[serde(default = "default_background_job_concurrency")]
    concurrency: usize,
    #[serde(default = "default_background_job_interval_ms")]
    interval_ms: u64,
}

impl Default for PersistedBackgroundJobSettings {
    fn default() -> Self {
        Self {
            auto_run: default_auto_run_background_jobs(),
            concurrency: default_background_job_concurrency(),
            interval_ms: default_background_job_interval_ms(),
        }
    }
}

/// Reading preferences of the PDF reader. Kept in their own settings key so the
/// established Provider JSON contract stays unchanged, exactly like the
/// background-job preference above.
pub(crate) const DEFAULT_PDF_COMPACT_READING: bool = false;

fn default_pdf_compact_reading() -> bool {
    DEFAULT_PDF_COMPACT_READING
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPdfReaderSettings {
    compact_reading: bool,
}

impl Default for PersistedPdfReaderSettings {
    fn default() -> Self {
        Self {
            compact_reading: default_pdf_compact_reading(),
        }
    }
}

/// Target language for the reading-time book translation. `None` keeps the
/// original text only; translation is opt-in through the AI settings window.
pub(crate) const DEFAULT_TRANSLATION_LANGUAGE: Option<&str> = None;

fn default_translation_language() -> Option<String> {
    DEFAULT_TRANSLATION_LANGUAGE.map(str::to_string)
}

/// How reading-time translations are presented relative to the original text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationDisplayMode {
    /// Translation text on top, original text below a divider.
    Bilingual,
    /// Only the translated text; the original is hidden until the reader toggles.
    TranslationOnly,
}

impl Default for TranslationDisplayMode {
    fn default() -> Self {
        DEFAULT_TRANSLATION_DISPLAY_MODE
    }
}

/// Reading-time translations hide the original text by default; the reader can
/// still toggle a paragraph back to bilingual. `Bilingual` restores the old
/// side-by-side view.
pub(crate) const DEFAULT_TRANSLATION_DISPLAY_MODE: TranslationDisplayMode =
    TranslationDisplayMode::TranslationOnly;

fn default_translation_display_mode() -> TranslationDisplayMode {
    DEFAULT_TRANSLATION_DISPLAY_MODE
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedTranslationSettings {
    #[serde(default)]
    default_language: Option<String>,
    #[serde(default)]
    display_mode: TranslationDisplayMode,
}

impl Default for PersistedTranslationSettings {
    fn default() -> Self {
        Self {
            default_language: default_translation_language(),
            display_mode: default_translation_display_mode(),
        }
    }
}

/// Persisted provider choices. Secrets intentionally cannot be represented by
/// this type; API keys live behind [`CredentialStore`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    pub base_url: String,
    /// Separate persistence keeps the established provider JSON unchanged.
    #[serde(skip)]
    pub endpoint_routing: EndpointRoutingSettings,
    pub chat_model: String,
    /// Stored under its own settings key and combined here for the UI/runtime.
    #[serde(skip)]
    pub chat_generation: ChatGenerationSettings,
    /// Stored under its own settings key so the established provider JSON
    /// contract remains unchanged.
    #[serde(skip, default = "default_auto_run_background_jobs")]
    pub auto_run_background_jobs: bool,
    /// How many background model jobs may run at the same time. Stored in the
    /// background-job settings row, like the auto-run preference above.
    #[serde(skip, default = "default_background_job_concurrency")]
    pub background_job_concurrency: usize,
    /// Pause in milliseconds one worker waits after finishing a job before it
    /// claims the next one. Stored in the background-job settings row.
    #[serde(skip, default = "default_background_job_interval_ms")]
    pub background_job_interval_ms: u64,
    /// Reading preference of the PDF reader: when enabled the continuous page
    /// column drops the gap between pages. Stored under its own settings key,
    /// like the background-job preference above.
    #[serde(skip, default = "default_pdf_compact_reading")]
    pub pdf_compact_reading: bool,
    /// Target language for the reading-time book translation. `None` keeps the
    /// original text. Stored under its own settings key so the established
    /// provider JSON contract remains unchanged, like the preferences above.
    #[serde(skip, default = "default_translation_language")]
    pub default_language: Option<String>,
    /// Reading-time translation display: bilingual by default off, only the
    /// translated text by default on. Stored under the translation settings key
    /// like the target language above.
    #[serde(skip, default = "default_translation_display_mode")]
    pub translation_display_mode: TranslationDisplayMode,
    pub embedding_model: String,
    /// Dimension override sent to the embedding provider. Changing this value
    /// invalidates all existing vector indices because stored vectors with the
    /// old dimension are incompatible with the new request/response size.
    #[serde(default = "default_embedding_dimensions")]
    pub embedding_dimensions: usize,
    pub vision_model: String,
    pub remote_content_confirmed: bool,
    pub allow_insecure_remote_http: bool,
    /// Canonical endpoint for which the content and transport acknowledgements
    /// were made. Changing the endpoint invalidates both booleans.
    pub confirmed_remote_endpoint: String,
    pub request_timeout_secs: u64,
    // --- Host-performed web search (off by default) ---
    /// Opt-in internet fallback used only when the authorized books cannot
    /// ground an answer. Disabled by default, so no request is ever issued
    /// unless the user enables and configures one.
    #[serde(default)]
    pub web_search_enabled: bool,
    #[serde(default)]
    pub web_search_url_template: String,
    #[serde(default = "default_web_search_method")]
    pub web_search_method: String,
    /// Optional JSON body template containing `{query}` for POST endpoints
    /// such as Tavily.
    #[serde(default)]
    pub web_search_body_template: Option<String>,
    /// Header name for the web-search key. `None` sends `Authorization: Bearer <key>`.
    #[serde(default)]
    pub web_search_key_header: Option<String>,
    #[serde(default)]
    pub web_search_remote_confirmed: bool,
    #[serde(default)]
    pub web_search_allow_insecure_http: bool,
    /// Canonical web endpoint for which the confirmations above were made.
    #[serde(default)]
    pub web_search_confirmed_remote_endpoint: String,
    #[serde(default = "default_web_search_timeout_secs")]
    pub web_search_timeout_secs: u64,
    #[serde(default = "default_web_search_max_results")]
    pub web_search_max_results: usize,
}

fn default_web_search_method() -> String {
    "GET".to_string()
}

fn default_web_search_timeout_secs() -> u64 {
    crate::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS
}

fn default_web_search_max_results() -> usize {
    crate::web_search::MAX_WEB_SEARCH_RESULTS
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_OPENAI_BASE_URL.to_string(),
            endpoint_routing: EndpointRoutingSettings::default(),
            chat_model: DEFAULT_CHAT_MODEL.to_string(),
            chat_generation: ChatGenerationSettings::default(),
            auto_run_background_jobs: default_auto_run_background_jobs(),
            background_job_concurrency: default_background_job_concurrency(),
            background_job_interval_ms: default_background_job_interval_ms(),
            pdf_compact_reading: default_pdf_compact_reading(),
            default_language: default_translation_language(),
            translation_display_mode: default_translation_display_mode(),
            embedding_model: DEFAULT_EMBEDDING_MODEL.to_string(),
            embedding_dimensions: DEFAULT_EMBEDDING_DIMENSIONS,
            vision_model: DEFAULT_VISION_MODEL.to_string(),
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: String::new(),
            request_timeout_secs: DEFAULT_AI_REQUEST_TIMEOUT_SECS,
            web_search_enabled: false,
            web_search_url_template: String::new(),
            web_search_method: default_web_search_method(),
            web_search_body_template: None,
            web_search_key_header: None,
            web_search_remote_confirmed: false,
            web_search_allow_insecure_http: false,
            web_search_confirmed_remote_endpoint: String::new(),
            web_search_timeout_secs: crate::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS,
            web_search_max_results: crate::web_search::MAX_WEB_SEARCH_RESULTS,
        }
    }
}

impl ProviderSettings {
    pub fn validate(&self) -> Result<()> {
        validate_model_name("chat", &self.chat_model)?;
        validate_model_name("embedding", &self.embedding_model)?;
        validate_model_name("vision", &self.vision_model)?;
        ensure!(
            self.embedding_dimensions > 0
                && self.embedding_dimensions <= crate::ai::MAX_EMBEDDING_DIMENSIONS,
            "embedding 维度必须在 1 到 {} 之间",
            crate::ai::MAX_EMBEDDING_DIMENSIONS
        );
        self.chat_generation.validate()?;
        if let Some(language) = &self.default_language {
            ensure!(
                TRANSLATION_LANGUAGES
                    .iter()
                    .any(|(tag, _)| *tag == language.as_str()),
                "默认显示语言无效"
            );
        }
        ensure!(
            (MIN_BACKGROUND_JOB_CONCURRENCY..=MAX_BACKGROUND_JOB_CONCURRENCY)
                .contains(&self.background_job_concurrency),
            "后台任务并发必须是 {MIN_BACKGROUND_JOB_CONCURRENCY} 到 {MAX_BACKGROUND_JOB_CONCURRENCY} 之间的整数"
        );
        ensure!(
            self.background_job_interval_ms <= MAX_BACKGROUND_JOB_INTERVAL_MS,
            "后台任务间隔必须是 0 到 {MAX_BACKGROUND_JOB_INTERVAL_MS} 之间的整数毫秒"
        );
        let mut ids = BTreeSet::new();
        let mut urls = BTreeSet::new();
        for endpoint in self.endpoints() {
            endpoint.validate()?;
            ensure!(ids.insert(endpoint.id.clone()), "Endpoint 标识重复");
            ensure!(
                urls.insert(normalize_provider_base_url(&endpoint.base_url)?.to_string()),
                "Endpoint 地址重复：请共用已有 Endpoint，并为各模型选择它"
            );
        }
        for role in [ModelRole::Chat, ModelRole::Embedding, ModelRole::Vision] {
            self.endpoint_for(role)?;
        }
        // Validate the web endpoint eagerly so a malformed configuration is
        // caught at save time instead of silently disabling the fallback.
        self.web_search_config(None)?;
        Ok(())
    }

    pub fn endpoints(&self) -> Vec<EndpointSettings> {
        let mut endpoints =
            Vec::with_capacity(self.endpoint_routing.additional_endpoints.len() + 1);
        endpoints.push(EndpointSettings {
            id: DEFAULT_ENDPOINT_ID.into(),
            name: self.endpoint_routing.default_endpoint_name.clone(),
            base_url: self.base_url.clone(),
            remote_content_confirmed: self.remote_content_confirmed,
            allow_insecure_remote_http: self.allow_insecure_remote_http,
            confirmed_remote_endpoint: self.confirmed_remote_endpoint.clone(),
            request_timeout_secs: self.request_timeout_secs,
        });
        endpoints.extend(self.endpoint_routing.additional_endpoints.iter().cloned());
        endpoints
    }

    pub fn endpoint_by_id(&self, id: &str) -> Result<EndpointSettings> {
        self.endpoints()
            .into_iter()
            .find(|endpoint| endpoint.id == id)
            .context("模型绑定的 Endpoint 不存在，请重新选择")
    }

    pub fn endpoint_for(&self, role: ModelRole) -> Result<EndpointSettings> {
        let id = match role {
            ModelRole::Chat => &self.endpoint_routing.chat_endpoint_id,
            ModelRole::Embedding => &self.endpoint_routing.embedding_endpoint_id,
            ModelRole::Vision => &self.endpoint_routing.vision_endpoint_id,
        };
        self.endpoint_by_id(id)
    }

    /// Builds the validated web-search configuration, or `None` when the
    /// fallback is disabled. The API key is supplied by the caller (it lives in
    /// the credential store, never in persisted JSON).
    pub fn web_search_config(&self, api_key: Option<String>) -> Result<Option<WebSearchConfig>> {
        if !self.web_search_enabled {
            return Ok(None);
        }
        let confirmation_matches = normalize_web_endpoint(&self.web_search_url_template)
            .is_some_and(|url| self.web_search_confirmed_remote_endpoint == url);
        let config = WebSearchConfig {
            enabled: true,
            url_template: self.web_search_url_template.clone(),
            method: self.web_search_method.clone(),
            body_template: self.web_search_body_template.clone(),
            api_key,
            api_key_header: self.web_search_key_header.clone(),
            remote_confirmed: self.web_search_remote_confirmed && confirmation_matches,
            allow_insecure_remote_http: self.web_search_allow_insecure_http && confirmation_matches,
            timeout_secs: self.web_search_timeout_secs,
            max_results: self.web_search_max_results,
        };
        config
            .validated_endpoint()
            .context("联网搜索端点配置无效")?;
        Ok(Some(config))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiKeyUpdate {
    Keep,
    Set(String),
    Delete,
}

/// Stable, UI-facing state for all persisted derivative work. Keeping this
/// type in the service layer prevents GPUI code from depending on database
/// rows or renderer-specific cursor JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundJobStatus {
    Queued,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundJobAction {
    Pause,
    Resume,
    Retry,
    Cancel,
    /// Re-runs a whole-book translation from the first block, discarding the
    /// persisted译文 of that book and language.
    Retranslate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackgroundJobProgress {
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundJobSnapshot {
    pub id: String,
    pub book_id: String,
    /// Foreign key into `book_sources`. `None` for legacy tasks created before
    /// sources became first-class. The details UI uses this to look up the
    /// owning source kind without exposing raw identifiers beyond the service
    /// layer.
    pub source_id: Option<String>,
    pub kind: String,
    pub status: BackgroundJobStatus,
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub attempts: u32,
    pub progress: BackgroundJobProgress,
    pub error: Option<String>,
    /// Unix seconds (UTC) — populated by the service layer so the UI can
    /// render a stable local timestamp without re-querying the database.
    pub created_at: u64,
    pub updated_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    /// Raw renderer/provider cursor JSON. Only kept for `Failed` jobs and
    /// for `visual_render` jobs so users can see which renderer profile and
    /// units were used; otherwise the cursor can be very large and noisy.
    pub cursor_json: Option<String>,
}

/// One current translation of a text block, keyed for the reader's DOM
/// matching. `source` is the canonical source text used as a matching hint when
/// the chapter HTML carries no block identifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslatedBlock {
    pub key: String,
    pub source: String,
    pub segments: Vec<crate::translation::TranslationSegment>,
}

/// Whether a book's declared language already satisfies a target tag. Compares
/// only the primary subtag so `en-US` matches `en` and `zh` matches `zh-Hans`.
pub fn language_is_target(source: Option<&str>, target: &str) -> bool {
    fn primary(tag: &str) -> String {
        tag.trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
    match source {
        Some(source) => {
            let source = primary(source);
            !source.is_empty() && source == primary(target)
        }
        None => false,
    }
}

/// A verified Office-enhanced page loaded from the managed object store.
///
/// Object keys and filesystem paths deliberately stay private to the service
/// layer; callers receive only stable document coordinates and owned bytes.
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfficeEnhancedPage {
    pub file_name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub content_unit_id: Option<String>,
    pub locator: crate::document::DocumentLocator,
}

struct AiServices {
    settings: ProviderSettings,
    provider: Arc<dyn OpenAiCompatibleProvider>,
    embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
    vision_provider: Arc<dyn OpenAiCompatibleProvider>,
    search: Arc<SearchService>,
}

/// Immutable request view, captured before asynchronous selection or history
/// preparation so saving settings cannot mix model, endpoint and search roles.
#[derive(Clone)]
pub(crate) struct AiRequestSnapshot {
    pub settings: ProviderSettings,
    pub provider: Arc<dyn OpenAiCompatibleProvider>,
    pub search: Arc<SearchService>,
}

impl std::fmt::Debug for AiServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiServices")
            .field("settings", &self.settings)
            .field("search", &self.search)
            .finish_non_exhaustive()
    }
}

/// Result of a shared library mutation together with the exact in-memory
/// projection published by that mutation.
#[derive(Debug)]
pub struct LibraryMutation<T> {
    pub value: T,
    pub snapshot: LibraryStore,
    pub generation: u64,
}

/// FIFO admission for every mutation of the process-level library snapshot.
///
/// `spawn_blocking` does not promise that jobs begin in submission order. A
/// plain mutex therefore lets a later UI request commit before an earlier one
/// and then be overwritten by it. Tickets are assigned and queued
/// synchronously at the API boundary. A single asynchronous dispatcher grants
/// exactly one turn at a time, so requests waiting for their turn consume
/// neither Tokio core threads nor blocking-pool threads.
#[derive(Debug)]
struct LibraryMutationQueue {
    next_ticket: AtomicU64,
    requests: tokio::sync::mpsc::UnboundedSender<LibraryMutationRequest>,
}

impl LibraryMutationQueue {
    fn new(runtime: &tokio::runtime::Handle) -> Self {
        let (requests, receiver) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(run_library_mutation_turnstile(receiver));
        Self {
            next_ticket: AtomicU64::new(0),
            requests,
        }
    }

    fn reserve(&self) -> LibraryMutationReservation {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let (ready, turn) = tokio::sync::oneshot::channel();
        if let Err(stopped) = self.requests.send(LibraryMutationRequest { ticket, ready }) {
            // Closing the returned sender makes `enter` report a useful error
            // instead of leaving an unresolvable reservation behind.
            drop(stopped.0.ready);
        }
        LibraryMutationReservation { ticket, turn }
    }
}

#[derive(Debug)]
struct LibraryMutationRequest {
    ticket: u64,
    ready: tokio::sync::oneshot::Sender<LibraryMutationTurn>,
}

#[derive(Debug)]
struct LibraryMutationReservation {
    ticket: u64,
    turn: tokio::sync::oneshot::Receiver<LibraryMutationTurn>,
}

impl LibraryMutationReservation {
    async fn enter(self) -> Result<LibraryMutationTurn> {
        self.turn.await.with_context(|| {
            format!(
                "library mutation turnstile stopped before ticket {} was admitted",
                self.ticket
            )
        })
    }
}

#[derive(Debug)]
struct LibraryMutationTurn {
    completed: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for LibraryMutationTurn {
    fn drop(&mut self) {
        if let Some(completed) = self.completed.take() {
            let _ = completed.send(());
        }
    }
}

async fn run_library_mutation_turnstile(
    mut requests: tokio::sync::mpsc::UnboundedReceiver<LibraryMutationRequest>,
) {
    let mut next_ticket = 0_u64;
    let mut pending = BTreeMap::new();

    while let Some(request) = requests.recv().await {
        let replaced = pending.insert(request.ticket, request.ready);
        debug_assert!(replaced.is_none(), "library mutation ticket was reused");

        while let Some(ready) = pending.remove(&next_ticket) {
            next_ticket = next_ticket.wrapping_add(1);
            let (completed, completion) = tokio::sync::oneshot::channel();
            let turn = LibraryMutationTurn {
                completed: Some(completed),
            };

            // A request may be explicitly aborted while queued. In that case
            // sending the turn returns it to us; dropping it releases the
            // dispatcher immediately and does not leave a ticket-sized hole.
            if ready.send(turn).is_ok() {
                let _ = completion.await;
            }
        }
    }
}

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

impl AppServices {
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_credentials(data_dir, Arc::new(SystemCredentialStore))
    }

    pub fn open_with_credentials(
        data_dir: impl Into<PathBuf>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self> {
        let requested_data_dir = data_dir.into();
        let runtime = IoRuntime::default();
        let library =
            LibraryStore::load_from_with_runtime(requested_data_dir.clone(), runtime.clone())?;
        let data_dir = fs::canonicalize(&requested_data_dir).with_context(|| {
            format!(
                "failed to resolve application data directory {}",
                requested_data_dir.display()
            )
        })?;
        let db_path = data_dir.join(db::DATABASE_FILE);
        let learning = Arc::new(crate::learning::LearningService::new(
            data_dir.join("learning"),
            runtime.clone(),
        ));
        let blobs = Arc::new(LocalBlobStore::new(data_dir.join(OBJECT_DIRECTORY))?);
        let formats = Arc::new(FormatRegistry::with_builtin_importers());
        let blob_publication = library.blob_publication_lock();
        let auto_run_background_jobs = library.background_job_auto_run_flag();
        let library = Arc::new(Mutex::new(library));
        let library_generation = Arc::new(AtomicU64::new(0));
        let library_mutations = Arc::new(LibraryMutationQueue::new(&runtime.handle()));
        let chat = ChatRepository::new(db_path.clone(), runtime.clone());
        let settings = load_provider_settings(&db_path)?;
        auto_run_background_jobs.store(settings.auto_run_background_jobs, Ordering::Release);
        let api_keys = load_endpoint_api_keys(&settings, credentials.as_ref())?;
        let ai = build_ai_services(&db_path, settings, &api_keys)?;
        let office: Arc<dyn OfficeEnhancer> = Arc::new(OfficeComWorker::start()?);
        let mut visual_renderers: Vec<Arc<dyn VisualRenderer>> =
            vec![Arc::new(StructuralPngRenderer)];
        #[cfg(target_os = "windows")]
        visual_renderers.push(Arc::new(WindowsPdfRenderer));
        #[cfg(target_os = "windows")]
        visual_renderers.push(Arc::new(OfficeEnhancedRenderer::new(Arc::clone(&office))));
        let visual_job_store = Arc::new(SqliteVisualJobStore::new(&db_path));
        let renderer_descriptors = visual_renderers
            .iter()
            .map(|renderer| renderer.descriptor())
            .collect::<Vec<_>>();
        // Reconcile before starting the model worker so stale SVG pages can
        // never race a requeued vision job during startup. The same gate used
        // by every object publisher spans the reference-removal transaction;
        // stale page bytes are then rechecked and reclaimed before any worker
        // can publish or consume a replacement.
        let reconciliation = {
            let publication_guard = runtime.block_on(blob_publication.acquire());
            let reconciliation = visual_job_store
                .reconcile_registered_renderers(&renderer_descriptors, unix_timestamp()?)?;
            drop(publication_guard);
            reconciliation
        };
        if reconciliation.changed_sources != 0 {
            tracing::info!(
                changed_sources = reconciliation.changed_sources,
                "已按当前 renderer/profile 重建视觉派生任务"
            );
        }
        runtime.block_on(reclaim_unreferenced_blobs(
            &db_path,
            &blobs,
            &blob_publication,
            reconciliation.unreferenced_blobs,
            "过期视觉页面对象",
        ));
        let indexing_blobs: Arc<dyn BlobStore> = blobs.clone();
        let indexing_models = indexing_model_config(&ai.settings)?;
        let indexing = IndexingCoordinator::start(
            runtime.handle(),
            &db_path,
            indexing_blobs,
            Arc::clone(&ai.embedding_provider),
            Arc::clone(&ai.vision_provider),
            indexing_models,
        )?;
        indexing.configure_scheduling(
            ai.settings.background_job_concurrency,
            Duration::from_millis(ai.settings.background_job_interval_ms),
        );
        runtime.block_on(indexing.configure_translation(
            Arc::clone(&ai.provider),
            ai.settings.chat_model.clone(),
            translation_execution_identity(&ai.settings)?,
            ai.settings.default_language.clone(),
            ai.settings.auto_run_background_jobs,
        ))?;

        let source: Arc<dyn VisualDocumentSource> = Arc::new(LibraryVisualDocumentSource {
            library: Arc::clone(&library),
        });
        let sink: Arc<dyn VisualPageSink> = Arc::new(LocalVisualPageSink {
            db_path: db_path.clone(),
            blobs: Arc::clone(&blobs),
            blob_publication: blob_publication.clone(),
            indexing: Arc::clone(&indexing),
        });
        let visual_jobs = VisualJobCoordinator::new(
            runtime.handle(),
            visual_job_store,
            source,
            sink,
            visual_renderers,
        )?;

        Ok(Self {
            data_dir,
            db_path,
            library,
            learning,
            library_generation,
            library_mutations,
            blobs,
            blob_publication,
            formats,
            chat,
            credentials,
            ai: Arc::new(RwLock::new(ai)),
            ai_configuration: Arc::new(tokio::sync::Mutex::new(())),
            office,
            indexing,
            auto_run_background_jobs,
            visual_jobs: RwLock::new(Some(Arc::new(visual_jobs))),
            visual_renderer_descriptors: RwLock::new(renderer_descriptors),
            runtime,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn learning(&self) -> Arc<crate::learning::LearningService> {
        Arc::clone(&self.learning)
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn runtime(&self) -> IoRuntime {
        self.runtime.clone()
    }

    pub fn blob_store(&self) -> Arc<LocalBlobStore> {
        Arc::clone(&self.blobs)
    }

    pub fn format_registry(&self) -> Arc<FormatRegistry> {
        Arc::clone(&self.formats)
    }

    pub fn chat(&self) -> ChatRepository {
        self.chat.clone()
    }

    /// Returns a current library snapshot for constructing a window. Window
    /// code should still use the service methods for long-running mutations;
    /// this clone is only the existing UI's initial, local view model.
    pub fn library_snapshot(&self) -> Result<LibraryStore> {
        self.library
            .lock()
            .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))
            .map(|library| library.clone())
    }

    /// Monotonic sequence assigned to successful mutations of the shared
    /// in-memory library projection. It lets windows discard a delayed older
    /// completion without ever taking the library mutex on the GPUI thread.
    pub fn library_generation(&self) -> u64 {
        self.library_generation.load(Ordering::Acquire)
    }

    pub fn provider_settings(&self) -> Result<ProviderSettings> {
        Ok(self
            .ai
            .read()
            .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
            .settings
            .clone())
    }

    /// Returns the current, revision-matched translations for one content unit
    /// in the configured default display language. Returns an empty vector when
    /// translation is disabled, the book already uses the target language, the
    /// unit belongs to another book, or no up-to-date translations exist yet.
    pub async fn translation_blocks_for_unit(
        &self,
        book_id: String,
        content_unit_id: String,
    ) -> Result<Vec<TranslatedBlock>> {
        let settings = self.provider_settings()?;
        let Some(target_language) = settings.default_language.clone() else {
            return Ok(Vec::new());
        };
        let chat_model = settings.chat_model.clone();
        let execution_identity = translation_execution_identity(&settings)?;
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let Some(book) = db::books::get(&conn, &book_id)? else {
                    return Ok(Vec::new());
                };
                if book
                    .language
                    .as_deref()
                    .is_some_and(|source| language_is_target(Some(source), &target_language))
                {
                    return Ok(Vec::new());
                }
                let Some(unit) = db::content_units::get(&conn, &content_unit_id)? else {
                    return Ok(Vec::new());
                };
                if unit.book_id != book_id {
                    return Ok(Vec::new());
                }
                let rows =
                    db::translations::list_for_unit(&conn, &content_unit_id, &target_language)?;
                Ok(rows
                    .into_iter()
                    .filter(|row| {
                        row.document_revision == book.revision
                            && row.unit_revision == unit.revision
                            && row.model == chat_model
                    })
                    .filter_map(|row| {
                        let translation = serde_json::from_str::<
                            crate::translation::StoredTranslation,
                        >(&row.translated_text)
                        .ok()?;
                        if translation.execution_identity != execution_identity {
                            return None;
                        }
                        Some(TranslatedBlock {
                            key: row.block_id,
                            source: row.source_text,
                            segments: translation.segments,
                        })
                    })
                    .collect())
            })
            .await
            .context("译文查询线程异常退出")?
    }

    pub fn provider(&self) -> Result<Arc<dyn OpenAiCompatibleProvider>> {
        Ok(Arc::clone(
            &self
                .ai
                .read()
                .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
                .provider,
        ))
    }

    pub(crate) fn ai_request_snapshot(&self) -> Result<AiRequestSnapshot> {
        let ai = self
            .ai
            .read()
            .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?;
        Ok(AiRequestSnapshot {
            settings: ai.settings.clone(),
            provider: Arc::clone(&ai.provider),
            search: Arc::clone(&ai.search),
        })
    }

    /// Builds the host web-search backend from the persisted configuration, or
    /// `None` when the fallback is disabled. The API key is read from the
    /// credential store, never from persisted JSON.
    pub fn web_search_backend(&self) -> Result<Option<Arc<dyn WebSearchBackend>>> {
        let settings = self.provider_settings()?;
        let api_key = if settings.web_search_enabled {
            self.credentials.api_key(WEB_SEARCH_CREDENTIAL_TARGET)?
        } else {
            None
        };
        match settings.web_search_config(api_key)? {
            Some(config) => Ok(Some(Arc::new(HttpWebSearch::new(config)?))),
            None => Ok(None),
        }
    }

    pub fn web_search_api_key(&self) -> Result<Option<String>> {
        self.credentials.api_key(WEB_SEARCH_CREDENTIAL_TARGET)
    }

    pub fn set_web_search_api_key(&self, key: &str) -> Result<()> {
        self.credentials
            .set_api_key(WEB_SEARCH_CREDENTIAL_TARGET, key)
    }

    pub fn delete_web_search_api_key(&self) -> Result<()> {
        self.credentials
            .delete_api_key(WEB_SEARCH_CREDENTIAL_TARGET)
    }

    pub fn search(&self) -> Result<Arc<SearchService>> {
        Ok(Arc::clone(
            &self
                .ai
                .read()
                .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
                .search,
        ))
    }

    pub fn office_enhancer(&self) -> Arc<dyn OfficeEnhancer> {
        Arc::clone(&self.office)
    }

    pub fn indexing(&self) -> Arc<IndexingCoordinator> {
        Arc::clone(&self.indexing)
    }

    pub fn visual_jobs(&self) -> Option<Arc<VisualJobCoordinator>> {
        self.visual_jobs
            .read()
            .ok()
            .and_then(|coordinator| coordinator.as_ref().map(Arc::clone))
    }

    /// Runs synchronous LibraryStore work away from GPUI and Tokio core
    /// workers. The returned handle belongs to the dedicated application
    /// runtime and may be awaited from a GPUI task.
    pub fn spawn_library<T, F>(&self, operation: F) -> tokio::task::JoinHandle<Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(&mut LibraryStore) -> Result<T> + Send + 'static,
    {
        let library = Arc::clone(&self.library);
        let library_generation = Arc::clone(&self.library_generation);
        let mutation_turn = self.library_mutations.reserve();
        let indexing = Arc::clone(&self.indexing);
        self.runtime.handle().spawn(async move {
            let turn = mutation_turn.enter().await?;
            tokio::task::spawn_blocking(move || {
                let _turn = turn;
                let mut library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let result = operation(&mut library);
                if result.is_ok() {
                    library_generation.fetch_add(1, Ordering::AcqRel);
                    indexing.wake();
                }
                result
            })
            .await
            .context("library mutation worker stopped")?
        })
    }

    /// Runs a mutation and captures the resulting projection while the same
    /// mutex guard is still held. The generation reflects commit order, so a
    /// UI can merge only monotonically newer completions even when async tasks
    /// finish their GPUI callbacks out of order.
    pub fn spawn_library_projected<T, F>(
        &self,
        operation: F,
    ) -> tokio::task::JoinHandle<Result<LibraryMutation<T>>>
    where
        T: Send + 'static,
        F: FnOnce(&mut LibraryStore) -> Result<T> + Send + 'static,
    {
        let library = Arc::clone(&self.library);
        let library_generation = Arc::clone(&self.library_generation);
        let mutation_turn = self.library_mutations.reserve();
        let indexing = Arc::clone(&self.indexing);
        self.runtime.handle().spawn(async move {
            let turn = mutation_turn.enter().await?;
            tokio::task::spawn_blocking(move || {
                let _turn = turn;
                let mut library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let value = operation(&mut library)?;
                let generation = library_generation.fetch_add(1, Ordering::AcqRel) + 1;
                let snapshot = library.clone();
                drop(library);
                indexing.wake();
                Ok(LibraryMutation {
                    value,
                    snapshot,
                    generation,
                })
            })
            .await
            .context("projected library mutation worker stopped")?
        })
    }

    /// Runs a read-only LibraryStore operation away from GPUI and Tokio core
    /// workers. The in-memory projection is cloned before the operation so a
    /// long media read cannot hold the mutation lock. Unlike
    /// [`Self::spawn_library`], successful reads do not wake the indexing
    /// coordinator; WebView range requests therefore cannot turn playback
    /// into an indexing wake storm.
    pub fn spawn_library_read<T, F>(&self, operation: F) -> tokio::task::JoinHandle<Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(&LibraryStore) -> Result<T> + Send + 'static,
    {
        let library = Arc::clone(&self.library);
        self.runtime.handle().spawn_blocking(move || {
            let library = library
                .lock()
                .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?
                .clone();
            operation(&library)
        })
    }

    /// Updates the default Endpoint key while preserving every other key.
    pub async fn configure_provider(
        &self,
        settings: ProviderSettings,
        api_key: ApiKeyUpdate,
    ) -> Result<()> {
        self.configure_providers(
            settings,
            BTreeMap::from([(DEFAULT_ENDPOINT_ID.into(), api_key)]),
        )
        .await
    }

    /// Saves all Endpoint and role choices as one serialized transition. Once
    /// accepted, the runtime completes or rolls back even if the UI is closed.
    pub async fn configure_providers(
        &self,
        settings: ProviderSettings,
        api_keys: BTreeMap<String, ApiKeyUpdate>,
    ) -> Result<()> {
        settings.validate()?;
        for (id, update) in &api_keys {
            settings.endpoint_by_id(id)?;
            if let ApiKeyUpdate::Set(value) = update {
                ensure!(!value.is_empty(), "API key cannot be empty");
            }
        }
        let indexing_models = indexing_model_config(&settings)?;
        let translation_identity = translation_execution_identity(&settings)?;
        let db_path = self.db_path.clone();
        let credentials = Arc::clone(&self.credentials);
        let ai = Arc::clone(&self.ai);
        let configuration = Arc::clone(&self.ai_configuration);
        let indexing = Arc::clone(&self.indexing);
        let auto_run = Arc::clone(&self.auto_run_background_jobs);
        self.runtime
            .spawn(async move {
                let _transition = configuration.lock().await;
                let previous_settings = ai
                    .read()
                    .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
                    .settings
                    .clone();
                let worker_db = db_path.clone();
                let worker_credentials = Arc::clone(&credentials);
                let (next, previous_rows, previous_keys) = tokio::task::spawn_blocking(move || {
                    let previous_rows = snapshot_ai_settings(&worker_db)?;
                    let mut next_keys =
                        load_endpoint_api_keys(&settings, worker_credentials.as_ref())?;
                    let mut changes = BTreeMap::new();
                    for endpoint in settings.endpoints() {
                        let target = normalize_provider_base_url(&endpoint.base_url)?.to_string();
                        let update = api_keys.get(&endpoint.id).unwrap_or(&ApiKeyUpdate::Keep);
                        match update {
                            ApiKeyUpdate::Keep => {}
                            ApiKeyUpdate::Set(value) => {
                                next_keys.insert(endpoint.id, Some(value.clone()));
                                changes.insert(target, Some(value.clone()));
                            }
                            ApiKeyUpdate::Delete => {
                                next_keys.insert(endpoint.id, None);
                                changes.insert(target, None);
                            }
                        }
                    }
                    let next_urls = settings
                        .endpoints()
                        .into_iter()
                        .map(|endpoint| {
                            normalize_provider_base_url(&endpoint.base_url)
                                .map(|url| url.to_string())
                        })
                        .collect::<Result<BTreeSet<_>>>()?;
                    for endpoint in previous_settings.endpoints() {
                        let target = normalize_provider_base_url(&endpoint.base_url)?.to_string();
                        if !next_urls.contains(&target) {
                            changes.insert(target, None);
                        }
                    }
                    // Construct every role before mutating durable settings or keys.
                    let services = build_ai_services(&worker_db, settings.clone(), &next_keys)?;
                    let previous_keys = changes
                        .keys()
                        .map(|target| {
                            worker_credentials
                                .api_key(target)
                                .map(|key| (target.clone(), key))
                        })
                        .collect::<Result<BTreeMap<_, _>>>()?;
                    let mutation = (|| {
                        restore_endpoint_keys(worker_credentials.as_ref(), &changes)?;
                        save_provider_settings(&worker_db, &settings)
                    })();
                    if let Err(error) = mutation {
                        if let Err(rollback_error) =
                            restore_endpoint_keys(worker_credentials.as_ref(), &previous_keys)
                        {
                            return Err(error.context(format!(
                                "credential rollback also failed: {rollback_error:#}"
                            )));
                        }
                        return Err(error);
                    }
                    Ok((services, previous_rows, previous_keys))
                })
                .await
                .context("AI settings worker stopped")??;

                let reconfigure_result = match indexing
                    .reconfigure(
                        Arc::clone(&next.embedding_provider),
                        Arc::clone(&next.vision_provider),
                        indexing_models,
                    )
                    .await
                    .context("failed to reconfigure derived indexing")
                {
                    Ok(()) => indexing
                        .configure_translation(
                            Arc::clone(&next.provider),
                            next.settings.chat_model.clone(),
                            translation_identity,
                            next.settings.default_language.clone(),
                            next.settings.auto_run_background_jobs,
                        )
                        .await
                        .context("failed to reconfigure book translation"),
                    Err(error) => Err(error),
                };
                if let Err(error) = reconfigure_result {
                    let rollback = tokio::task::spawn_blocking(move || {
                        let settings_result = restore_ai_settings(&db_path, &previous_rows);
                        let credential_result =
                            restore_endpoint_keys(credentials.as_ref(), &previous_keys);
                        match (settings_result, credential_result) {
                            (Ok(()), Ok(())) => Ok(()),
                            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                            (Err(settings_error), Err(credential_error)) => Err(settings_error
                                .context(format!(
                                    "credential rollback also failed: {credential_error:#}"
                                ))),
                        }
                    })
                    .await
                    .context("AI settings rollback worker stopped")?;
                    if let Err(rollback_error) = rollback {
                        return Err(error.context(format!(
                            "persisted AI settings rollback also failed: {rollback_error:#}"
                        )));
                    }
                    return Err(error);
                }

                auto_run.store(next.settings.auto_run_background_jobs, Ordering::Release);
                indexing.configure_scheduling(
                    next.settings.background_job_concurrency,
                    Duration::from_millis(next.settings.background_job_interval_ms),
                );
                *ai.write().unwrap_or_else(|error| error.into_inner()) = next;
                ai.clear_poison();
                Ok(())
            })
            .await
            .context("AI configuration task stopped")?
    }

    /// Probes only the default Endpoint, independently of model or web drafts.
    pub async fn probe_provider_models(
        &self,
        settings: ProviderSettings,
        api_key: ApiKeyUpdate,
    ) -> Result<Vec<ModelInfo>> {
        self.probe_endpoint_models(settings.endpoint_by_id(DEFAULT_ENDPOINT_ID)?, api_key)
            .await
    }

    /// Queries an existing or unsaved Endpoint without persisting it. Other
    /// Endpoint, model and web-search drafts do not affect this operation.
    pub async fn probe_endpoint_models(
        &self,
        endpoint: EndpointSettings,
        api_key: ApiKeyUpdate,
    ) -> Result<Vec<ModelInfo>> {
        let credentials = Arc::clone(&self.credentials);
        let provider = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                endpoint.validate()?;
                let target = normalize_provider_base_url(&endpoint.base_url)?;
                let api_key = match api_key {
                    ApiKeyUpdate::Keep => credentials.api_key(target.as_str())?,
                    ApiKeyUpdate::Set(value) => {
                        ensure!(!value.is_empty(), "API key cannot be empty");
                        Some(value)
                    }
                    ApiKeyUpdate::Delete => None,
                };
                OpenAiHttpProvider::new(endpoint.provider_config(api_key))
            })
            .await
            .context("AI provider probe setup worker stopped")??;
        self.runtime
            .spawn(async move { provider.models().await })
            .await
            .context("AI model probe worker stopped")?
    }

    /// Replaces the structural renderer coordinator with an explicitly
    /// supplied renderer set (for example a PDF.js or Office-enhanced worker).
    /// Existing in-flight jobs must be allowed to finish or be cancelled first.
    pub fn replace_visual_renderers(
        &self,
        source: Arc<dyn VisualDocumentSource>,
        sink: Arc<dyn VisualPageSink>,
        renderers: Vec<Arc<dyn VisualRenderer>>,
    ) -> Result<Arc<VisualJobCoordinator>> {
        let descriptors = renderers
            .iter()
            .map(|renderer| renderer.descriptor())
            .collect::<Vec<_>>();
        let coordinator = Arc::new(VisualJobCoordinator::new(
            self.runtime.handle(),
            Arc::new(SqliteVisualJobStore::new(&self.db_path)),
            source,
            sink,
            renderers,
        )?);
        let mut current = self
            .visual_jobs
            .write()
            .map_err(|_| anyhow::anyhow!("visual job service lock is poisoned"))?;
        if let Some(existing) = current.as_ref() {
            ensure!(
                Arc::strong_count(existing) == 1,
                "visual coordinator is in use; cancel or release its jobs before replacing it"
            );
        }
        *current = Some(Arc::clone(&coordinator));
        *self
            .visual_renderer_descriptors
            .write()
            .map_err(|_| anyhow::anyhow!("visual renderer descriptor lock is poisoned"))? =
            descriptors;
        Ok(coordinator)
    }
}

impl AppServices {
    /// Returns the durable per-book opt-in. Absence is deliberately equivalent
    /// to disabled so importing an Office document never starts COM silently.
    #[cfg(target_os = "windows")]
    pub async fn office_enhancement_enabled(&self, book_id: String) -> Result<bool> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                db::office_enhancements::is_enabled(&db::open_conn(&db_path)?, &book_id)
            })
            .await
            .context("Office 增强预览设置查询线程异常退出")?
    }

    /// Enables or disables Office COM enhancement for the current source,
    /// replacing only visual-derived pages/chunks/jobs. Canonical content,
    /// full-text chunks and the byte-exact original remain untouched.
    #[cfg(target_os = "windows")]
    pub async fn set_office_enhancement_enabled(
        &self,
        book_id: String,
        enabled: bool,
    ) -> Result<String> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        // Serialize the setting and source lookup with canonical document
        // mutations. Otherwise an edit could publish a normalized source
        // between preflight and the derived-job replacement.
        let _mutation_turn = self.library_mutations.reserve().enter().await?;
        let db_path = self.db_path.clone();
        let lookup_book_id = book_id.clone();
        let source_id = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let book = db::books::get(&conn, &lookup_book_id)?.context("图书不存在")?;
                let source = db::book_sources::get_revision(&conn, &lookup_book_id, book.revision)?
                    .context("当前图书来源不存在")?;
                if enabled {
                    ensure!(
                        source.source_kind == "original"
                            && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx"),
                        "只有尚未规范化编辑的 Office 原件可以启用增强预览"
                    );
                }
                Ok(source.id)
            })
            .await
            .context("Office 增强预览来源查询线程异常退出")??;

        let coordinator = self.visual_jobs().context("视觉任务协调器不可用")?;
        let job_id = format!("visual-render:{source_id}");
        stop_visual_job_before_replacement(&coordinator, &job_id).await?;
        stop_indexing_jobs_before_visual_replacement(&self.indexing, &book_id, &source_id).await?;

        let descriptors = self
            .visual_renderer_descriptors
            .read()
            .map_err(|_| anyhow::anyhow!("visual renderer descriptor lock is poisoned"))?
            .clone();
        let profile = RenderProfile::default();
        let db_path = self.db_path.clone();
        let publication_guard = self.blob_publication.acquire().await;
        let (committed_source_id, reconciliation, committed_spec) = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                let mut conn = db::open_conn(&db_path)?;
                let (source_id, reconciliation) = db::transactions::set_office_enhancement(
                    &mut conn,
                    &book_id,
                    enabled,
                    &descriptors,
                    &profile,
                    unix_timestamp()?,
                )?;
                // The mutation turn prevents another source/settings replacement;
                // the background renderer may only advance this committed job.
                // Freeze its full identity before object reclamation yields.
                let job_id = format!("visual-render:{source_id}");
                let job = db::index_jobs::get(&conn, &job_id)?
                    .context("Office 增强视觉任务提交后不存在")?;
                let (spec, _) = crate::preview::decode_persisted_visual_job(&job.cursor_json)?;
                ensure!(
                    spec.id == job_id && spec.book_id == book_id && spec.source_id == source_id,
                    "Office 增强视觉任务与本次提交的来源不一致"
                );
                Ok((source_id, reconciliation, spec))
            })
            .await
            .context("Office 增强预览设置线程异常退出")??;
        ensure!(
            committed_source_id == source_id,
            "图书来源在 Office 增强预览设置期间发生变化"
        );
        drop(publication_guard);
        reclaim_unreferenced_blobs(
            &self.db_path,
            &self.blobs,
            &self.blob_publication,
            reconciliation.unreferenced_blobs,
            "Office 增强替换的视觉页面对象",
        )
        .await;
        coordinator
            .schedule_committed(&committed_spec)
            .await
            .context("无法确认 Office 增强视觉任务已交给后台处理")?;
        self.indexing.wake();
        Ok(job_id)
    }

    /// Loads the complete, successfully published Office-enhanced page set
    /// for the book's current source revision.
    ///
    /// The database graph is validated while the object publication gate is
    /// held, then every object is rechecked against its stored length, BLAKE3
    /// digest and image media type before bytes leave the service layer.
    #[cfg(target_os = "windows")]
    pub async fn load_office_enhanced_pages(
        &self,
        book_id: String,
    ) -> Result<Vec<OfficeEnhancedPage>> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        // Canonical edits and Office enable/disable operations use this same
        // turnstile. Holding the turn until object reads finish ensures the
        // result still belongs to the current source when it is returned.
        let _mutation_turn = self.library_mutations.reserve().enter().await?;
        let publication_guard = self.blob_publication.acquire().await;
        let db_path = self.db_path.clone();
        let rows = self
            .runtime
            .handle()
            .spawn_blocking(move || load_office_enhanced_page_rows(&db_path, &book_id))
            .await
            .context("Office 增强页面查询线程异常退出")??;

        let mut pages = Vec::with_capacity(rows.len());
        let mut total_bytes = 0_u64;
        for (page_index, row) in rows.into_iter().enumerate() {
            let key = BlobKey::parse(&row.object_key).context("Office 增强页面对象键无效")?;
            let bytes = self
                .blobs
                .get(&key)
                .await
                .context("无法读取 Office 增强页面对象")?;
            ensure!(
                bytes.len() as u64 == row.byte_len,
                "Office 增强页面对象长度与数据库元数据不一致"
            );
            total_bytes = total_bytes
                .checked_add(row.byte_len)
                .context("Office 增强页面总大小溢出")?;
            ensure!(
                total_bytes <= MAX_LOADED_OFFICE_PAGE_BYTES,
                "Office 增强页面总大小超过支持上限"
            );
            let digest = blake3::hash(&bytes).to_hex().to_string();
            ensure!(
                digest == row.hash && BlobKey::from_bytes(&bytes) == key,
                "Office 增强页面对象摘要与数据库元数据不一致"
            );
            let expected_format = office_enhanced_image_format(&row.media_type)?;
            ensure!(
                image::guess_format(&bytes).context("无法识别 Office 增强页面图片格式")?
                    == expected_format,
                "Office 增强页面图片格式与媒体类型不一致"
            );
            pages.push(OfficeEnhancedPage {
                file_name: office_enhanced_page_file_name(page_index, &row.media_type)?,
                media_type: row.media_type,
                bytes,
                content_unit_id: row.content_unit_id,
                locator: row.locator,
            });
        }
        drop(publication_guard);
        Ok(pages)
    }

    /// Loads the complete persisted task history for the requested books on
    /// the application runtime. The scope is explicit and an empty scope never
    /// turns into an unbounded query.
    pub async fn background_jobs_for_books(
        &self,
        mut book_ids: Vec<String>,
    ) -> Result<Vec<BackgroundJobSnapshot>> {
        book_ids.retain(|book_id| !book_id.trim().is_empty());
        book_ids.sort();
        book_ids.dedup();
        if book_ids.is_empty() {
            return Ok(Vec::new());
        }

        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let mut snapshots = Vec::new();
                for book_id in &book_ids {
                    for job in db::index_jobs::list_for_book(&conn, book_id)? {
                        snapshots.push(background_job_snapshot(&conn, job)?);
                    }
                }
                snapshots.sort_by(|left, right| {
                    left.book_id
                        .cmp(&right.book_id)
                        .then_with(|| {
                            background_kind_order(&left.kind)
                                .cmp(&background_kind_order(&right.kind))
                        })
                        .then_with(|| left.id.cmp(&right.id))
                });
                Ok(snapshots)
            })
            .await
            .context("后台任务查询线程异常退出")?
    }

    /// Reads the most recent 500 diagnostic entries after checking the same
    /// explicit book scope used by the task list. An empty scope grants no
    /// access, and deleted jobs cannot expose leftover diagnostic history.
    pub async fn background_job_logs(
        &self,
        job_id: String,
        book_ids: Vec<String>,
    ) -> Result<BackgroundJobLogSnapshot> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let job = db::index_jobs::get(&conn, &job_id)?.context("后台任务已不存在")?;
                ensure!(
                    book_ids.iter().any(|book_id| book_id == &job.book_id),
                    "后台任务不在当前图书范围内"
                );
                crate::job_diagnostics::JobDiagnosticStore::for_database(&db_path)?.read(&job_id)
            })
            .await
            .context("后台任务日志查询线程异常退出")?
    }

    /// Applies one state transition using the coordinator that owns the job
    /// kind. In particular, a running visual renderer must receive its in-memory
    /// control signal in addition to the durable SQLite flag.
    pub async fn control_background_job(
        &self,
        job_id: String,
        action: BackgroundJobAction,
    ) -> Result<bool> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let indexing = Arc::clone(&self.indexing);
        let visual_jobs = self.visual_jobs();
        self.runtime
            .spawn(async move {
                let job = indexing
                    .job(&job_id)
                    .await?
                    .with_context(|| format!("后台任务已不存在：{job_id}"))?;
                match job.kind.as_str() {
                    "visual_render" => {
                        let coordinator = visual_jobs.context("视觉任务协调器不可用")?;
                        match action {
                            BackgroundJobAction::Pause => coordinator.pause(&job_id).await,
                            BackgroundJobAction::Resume => coordinator.resume(&job_id).await,
                            BackgroundJobAction::Retry => coordinator.retry(&job_id).await,
                            BackgroundJobAction::Cancel => coordinator.cancel(&job_id).await,
                            // Only translation has a re-run; a page render is
                            // rebuilt by re-importing a revision.
                            BackgroundJobAction::Retranslate => Ok(false),
                        }
                    }
                    "translation" => match action {
                        BackgroundJobAction::Pause => indexing.pause(&job_id).await,
                        BackgroundJobAction::Resume => indexing.resume(&job_id).await,
                        BackgroundJobAction::Retry => indexing.retry(&job_id).await,
                        BackgroundJobAction::Cancel => indexing.cancel(&job_id).await,
                        BackgroundJobAction::Retranslate => indexing.retranslate(&job_id).await,
                    },
                    "embedding" | "vision" => match action {
                        BackgroundJobAction::Pause => indexing.pause(&job_id).await,
                        BackgroundJobAction::Resume => indexing.resume(&job_id).await,
                        BackgroundJobAction::Retry => indexing.retry(&job_id).await,
                        BackgroundJobAction::Cancel => indexing.cancel(&job_id).await,
                        BackgroundJobAction::Retranslate => Ok(false),
                    },
                    other => anyhow::bail!("不支持控制后台任务类型：{other}"),
                }
            })
            .await
            .context("后台任务控制线程异常退出")?
    }
}

impl Drop for AppServices {
    fn drop(&mut self) {
        if let Ok(slot) = self.visual_jobs.get_mut() {
            slot.take();
        }
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct OfficeEnhancedPageRow {
    object_key: String,
    media_type: String,
    byte_len: u64,
    hash: String,
    content_unit_id: Option<String>,
    locator: crate::document::DocumentLocator,
}

#[cfg(target_os = "windows")]
fn load_office_enhanced_page_rows(
    db_path: &Path,
    book_id: &str,
) -> Result<Vec<OfficeEnhancedPageRow>> {
    let conn = db::open_conn(db_path)?;
    let book = db::books::get(&conn, book_id)?.context("图书不存在")?;
    let source = db::book_sources::get_revision(&conn, book_id, book.revision)?
        .context("当前图书来源不存在")?;
    ensure!(
        db::office_enhancements::is_enabled(&conn, book_id)?,
        "当前图书尚未启用 Office 增强预览"
    );
    ensure!(
        source.source_kind == "original"
            && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx"),
        "当前图书来源不再是可增强的 Office 原件"
    );

    let job_id = format!("visual-render:{}", source.id);
    let job = db::index_jobs::get(&conn, &job_id)?.context("Office 增强视觉任务不存在")?;
    ensure!(
        job.book_id == book.id
            && job.source_id.as_deref() == Some(source.id.as_str())
            && job.kind == "visual_render",
        "Office 增强视觉任务不属于当前图书来源"
    );
    ensure!(
        job.status == db::index_jobs::IndexJobStatus::Succeeded,
        "Office 增强视觉任务尚未成功完成"
    );
    let (spec, completed_pages) = crate::preview::decode_persisted_visual_job(&job.cursor_json)
        .context("Office 增强视觉任务游标无效")?;
    ensure!(
        spec.id == job_id
            && spec.book_id == book.id
            && spec.source_id == source.id
            && spec.document_revision.get() == book.revision
            && spec.renderer == OFFICE_ENHANCED_RENDERER_NAME
            && spec.fidelity == crate::preview::RenderFidelity::OfficeEnhanced,
        "Office 增强视觉任务不是当前图书版本的增强产物"
    );
    let units = db::content_units::list_for_source(&conn, &source.id)?
        .into_iter()
        .map(|unit| (unit.id, unit.revision))
        .collect::<BTreeMap<_, _>>();
    ensure!(!units.is_empty(), "当前 Office 来源没有内容单元");
    ensure!(
        spec.unit_ids.is_empty()
            || (spec.unit_ids.len() == units.len()
                && spec
                    .unit_ids
                    .iter()
                    .all(|unit_id| units.contains_key(unit_id))),
        "Office 增强视觉任务只包含部分内容单元"
    );
    let pages = db::visual_pages::list_for_source(&conn, &source.id)?;
    ensure!(
        !pages.is_empty() && pages.len() <= MAX_LOADED_OFFICE_PAGES,
        "Office 增强页面数量无效或超过支持上限"
    );
    ensure!(
        completed_pages == pages.len(),
        "Office 增强视觉任务完成进度与已发布页面不一致"
    );

    let profile_id = spec.profile.stable_id();
    let mut total_bytes = 0_u64;
    let mut rows = Vec::with_capacity(pages.len());
    for (expected_index, page) in pages.into_iter().enumerate() {
        ensure!(
            page.page_index == expected_index,
            "Office 增强页面序号不连续"
        );
        ensure!(
            page.book_id == book.id
                && page.source_id == source.id
                && page.document_revision == book.revision
                && page.renderer == spec.renderer
                && page.renderer_version == spec.renderer_version
                && page.profile_id == profile_id
                && page.render_scale == spec.profile.scale()
                && page.fidelity == "office_enhanced",
            "Office 增强页面元数据与当前视觉任务不一致"
        );
        ensure!(
            page.width > 0 && page.height > 0 && page.render_scale.is_finite(),
            "Office 增强页面尺寸或缩放无效"
        );
        let locator = serde_json::from_str::<crate::document::DocumentLocator>(&page.locator_json)
            .context("Office 增强页面定位信息无效")?;
        locator
            .validate()
            .context("Office 增强页面定位信息不符合统一模型约束")?;
        ensure!(
            locator.book_id == book.id,
            "Office 增强页面定位不属于当前图书"
        );
        match locator.source.as_ref() {
            Some(crate::document::SourceLocator::Slide { .. }) => {
                let content_unit_id = page
                    .content_unit_id
                    .as_deref()
                    .context("PowerPoint 增强页面缺少内容单元 ID")?;
                let current_unit_revision = units.get(content_unit_id).with_context(|| {
                    format!("Office 增强页面引用了不存在的内容单元：{content_unit_id}")
                })?;
                ensure!(
                    locator.unit_id == content_unit_id
                        && page.unit_revision == *current_unit_revision,
                    "PowerPoint 增强页面内容单元映射或版本已过期"
                );
            }
            Some(crate::document::SourceLocator::OfficeRenderedPage { page: source_page }) => {
                let expected_page = u32::try_from(expected_index)
                    .ok()
                    .and_then(|index| index.checked_add(1));
                ensure!(
                    page.content_unit_id.is_none()
                        && matches!(source.format.as_str(), "doc" | "docx" | "xlsx")
                        && page.unit_revision == crate::document::Revision::INITIAL.get()
                        && locator.unit_id == office_preview_unit_id(&book.id, &source.id)
                        && locator.block_id.is_none()
                        && locator.text_range.is_none()
                        && locator.region.is_none()
                        && expected_page == Some(*source_page),
                    "Word/Excel 增强页面错误关联了内容单元或页面定位无效"
                );
            }
            _ => anyhow::bail!("Office 增强页面缺少可验证的幻灯片或预览页定位"),
        }
        let blob =
            db::blobs::get(&conn, &page.object_key)?.context("Office 增强页面对象元数据不存在")?;
        office_enhanced_image_format(&blob.media_type)?;
        BlobKey::parse(&blob.object_key).context("Office 增强页面对象键无效")?;
        total_bytes = total_bytes
            .checked_add(blob.byte_len)
            .context("Office 增强页面总大小溢出")?;
        ensure!(
            total_bytes <= MAX_LOADED_OFFICE_PAGE_BYTES,
            "Office 增强页面总大小超过支持上限"
        );
        rows.push(OfficeEnhancedPageRow {
            object_key: blob.object_key,
            media_type: blob.media_type,
            byte_len: blob.byte_len,
            hash: blob.hash,
            content_unit_id: page.content_unit_id,
            locator,
        });
    }
    Ok(rows)
}

#[cfg(target_os = "windows")]
fn office_enhanced_image_format(media_type: &str) -> Result<image::ImageFormat> {
    match media_type {
        "image/png" => Ok(image::ImageFormat::Png),
        "image/jpeg" => Ok(image::ImageFormat::Jpeg),
        _ => anyhow::bail!("Office 增强页面使用了不支持的图片媒体类型：{media_type}"),
    }
}

#[cfg(target_os = "windows")]
fn office_enhanced_page_file_name(page_index: usize, media_type: &str) -> Result<String> {
    let extension = match office_enhanced_image_format(media_type)? {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        _ => unreachable!("Office enhanced media types are exhaustively checked"),
    };
    Ok(format!("Page{:05}.{extension}", page_index + 1))
}

fn background_job_snapshot(
    conn: &rusqlite::Connection,
    job: db::index_jobs::IndexJob,
) -> Result<BackgroundJobSnapshot> {
    let cursor = serde_json::from_str::<serde_json::Value>(&job.cursor_json).ok();
    let completed = match job.kind.as_str() {
        "visual_render" => cursor
            .as_ref()
            .and_then(|value| json_usize(value.get("completed_pages")))
            .unwrap_or_default(),
        "embedding" | "vision" | "translation" => cursor
            .as_ref()
            .and_then(|value| json_usize(value.get("next_ordinal")))
            .unwrap_or_default(),
        _ => 0,
    };
    let total = match (job.kind.as_str(), job.source_id.as_deref()) {
        ("embedding", Some(source_id)) => {
            Some(db::search_chunks::count_for_source(conn, source_id)?)
        }
        ("vision", Some(source_id)) => Some(db::visual_pages::count_for_source(conn, source_id)?),
        // A content unit can render to several pages. The final page count
        // is known only after publication succeeds; unit_ids is not a page total.
        ("visual_render", _) if job.status == db::index_jobs::IndexJobStatus::Succeeded => {
            Some(completed)
        }
        ("visual_render", _) => None,
        _ => None,
    };
    let kept_cursor = snapshot_cursor_json(&job.kind, job.status, &job.cursor_json);
    Ok(BackgroundJobSnapshot {
        id: job.id,
        book_id: job.book_id,
        source_id: job.source_id,
        kind: job.kind,
        status: match job.status {
            db::index_jobs::IndexJobStatus::Queued => BackgroundJobStatus::Queued,
            db::index_jobs::IndexJobStatus::Running => BackgroundJobStatus::Running,
            db::index_jobs::IndexJobStatus::Paused => BackgroundJobStatus::Paused,
            db::index_jobs::IndexJobStatus::Succeeded => BackgroundJobStatus::Succeeded,
            db::index_jobs::IndexJobStatus::Failed => BackgroundJobStatus::Failed,
            db::index_jobs::IndexJobStatus::Cancelled => BackgroundJobStatus::Cancelled,
        },
        pause_requested: job.pause_requested,
        cancel_requested: job.cancel_requested,
        attempts: job.attempts,
        progress: BackgroundJobProgress { completed, total },
        error: job.error,
        created_at: job.created_at,
        updated_at: job.updated_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        cursor_json: kept_cursor,
    })
}

/// Decide whether to expose the raw cursor JSON to the UI. Visual render jobs
/// keep their cursor so users can inspect the renderer profile and unit list;
/// failed jobs of any kind keep theirs because the cursor often contains the
/// last successful provider response or page index. Other jobs return `None`
/// because the cursor is just an internal offset.
fn snapshot_cursor_json(
    kind: &str,
    status: db::index_jobs::IndexJobStatus,
    cursor: &str,
) -> Option<String> {
    let keep = kind == "visual_render" || matches!(status, db::index_jobs::IndexJobStatus::Failed);
    if keep && !cursor.is_empty() {
        Some(cursor.to_string())
    } else {
        None
    }
}

fn json_usize(value: Option<&serde_json::Value>) -> Option<usize> {
    value
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn background_kind_order(kind: &str) -> u8 {
    match kind {
        "visual_render" => 0,
        "vision" => 1,
        "embedding" => 2,
        "translation" => 3,
        _ => 4,
    }
}

fn build_ai_services(
    db_path: &Path,
    settings: ProviderSettings,
    api_keys: &BTreeMap<String, Option<String>>,
) -> Result<AiServices> {
    settings.validate()?;
    let build_role = |role| -> Result<Arc<dyn OpenAiCompatibleProvider>> {
        let endpoint = settings.endpoint_for(role)?;
        let api_key = api_keys.get(&endpoint.id).cloned().flatten();
        Ok(Arc::new(OpenAiHttpProvider::new(
            endpoint.provider_config(api_key),
        )?))
    };
    let provider = build_role(ModelRole::Chat)?;
    let embedding_provider = build_role(ModelRole::Embedding)?;
    let vision_provider = build_role(ModelRole::Vision)?;
    let search = Arc::new(SearchService::new_with_execution_identity(
        db_path,
        Arc::clone(&embedding_provider),
        &settings.embedding_model,
        settings.embedding_dimensions,
        embedding_execution_identity(&settings)?,
    )?);
    Ok(AiServices {
        settings,
        provider,
        embedding_provider,
        vision_provider,
        search,
    })
}

fn load_endpoint_api_keys(
    settings: &ProviderSettings,
    credentials: &dyn CredentialStore,
) -> Result<BTreeMap<String, Option<String>>> {
    settings
        .endpoints()
        .into_iter()
        .map(|endpoint| {
            let target = normalize_provider_base_url(&endpoint.base_url)?;
            Ok((endpoint.id, credentials.api_key(target.as_str())?))
        })
        .collect()
}

/// Attempt every entry even when one credential operation fails, so rollback
/// does not abandon independent keys after a single failure.
fn restore_endpoint_keys(
    credentials: &dyn CredentialStore,
    values: &BTreeMap<String, Option<String>>,
) -> Result<()> {
    let mut failure = None;
    for (target, value) in values {
        let result = match value {
            Some(value) => credentials.set_api_key(target, value),
            None => credentials.delete_api_key(target),
        };
        if let Err(error) = result {
            failure = Some(match failure {
                Some(previous) => {
                    error.context(format!("another credential operation failed: {previous:#}"))
                }
                None => error,
            });
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

const AI_SETTINGS_KEYS: [&str; 6] = [
    PROVIDER_SETTINGS_KEY,
    CHAT_GENERATION_SETTINGS_KEY,
    ENDPOINT_ROUTING_SETTINGS_KEY,
    BACKGROUND_JOB_SETTINGS_KEY,
    PDF_READER_SETTINGS_KEY,
    TRANSLATION_SETTINGS_KEY,
];

fn snapshot_ai_settings(db_path: &Path) -> Result<Vec<Option<db::settings::Setting>>> {
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法读取 AI 设置备份")?;
    let rows = AI_SETTINGS_KEYS
        .iter()
        .map(|key| db::settings::get(&tx, key))
        .collect::<Result<_>>()?;
    tx.commit().context("无法完成 AI 设置备份")?;
    Ok(rows)
}

fn restore_ai_settings(db_path: &Path, rows: &[Option<db::settings::Setting>]) -> Result<()> {
    ensure!(rows.len() == AI_SETTINGS_KEYS.len(), "AI 设置备份不完整");
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法开始回滚 AI 设置")?;
    for (key, row) in AI_SETTINGS_KEYS.iter().zip(rows) {
        match row {
            Some(row) => {
                db::settings::upsert(&tx, row)?;
            }
            None => {
                db::settings::delete(&tx, key)?;
            }
        }
    }
    tx.commit().context("无法提交 AI 设置回滚")?;
    Ok(())
}

fn indexing_model_config(settings: &ProviderSettings) -> Result<IndexingModelConfig> {
    IndexingModelConfig::new(
        &settings.embedding_model,
        settings.embedding_dimensions,
        embedding_execution_identity(settings)?,
        &settings.vision_model,
        vision_execution_identity(settings)?,
    )
}

/// Non-secret identity for deciding whether persisted whole-book translations
/// are stale. A changed chat endpoint or model restarts every translation job
/// from the first block; the target language is part of each job's identity.
fn translation_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint = normalize_provider_base_url(&settings.endpoint_for(ModelRole::Chat)?.base_url)?;
    Ok(format!(
        "translation-v2:{}",
        blake3::hash(format!("{}\0{}", endpoint.as_str(), settings.chat_model).as_bytes()).to_hex()
    ))
}

fn vision_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint =
        normalize_provider_base_url(&settings.endpoint_for(ModelRole::Vision)?.base_url)?;
    Ok(format!(
        "vision-v1:{}",
        blake3::hash(format!("{}\0{}", endpoint.as_str(), settings.vision_model).as_bytes())
            .to_hex()
    ))
}

/// Non-secret identity for deciding whether persisted vector work is stale.
/// Deliberately excludes chat/vision choices, request policy and credentials.
fn embedding_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint =
        normalize_provider_base_url(&settings.endpoint_for(ModelRole::Embedding)?.base_url)?;
    Ok(format!(
        "embedding-v1:{}",
        blake3::hash(
            format!(
                "{}\0{}\0{}",
                endpoint.as_str(),
                settings.embedding_model,
                settings.embedding_dimensions
            )
            .as_bytes()
        )
        .to_hex()
    ))
}

fn load_provider_settings(db_path: &Path) -> Result<ProviderSettings> {
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法读取 AI 设置快照")?;
    let mut settings = match db::settings::get(&tx, PROVIDER_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<ProviderSettings>(&row.value_json)
            .context("stored AI provider settings are invalid")?,
        None => ProviderSettings::default(),
    };
    settings.chat_generation = match db::settings::get(&tx, CHAT_GENERATION_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<ChatGenerationSettings>(&row.value_json)
            .context("保存的对话模型参数无效")?,
        None => ChatGenerationSettings::default(),
    };
    settings.endpoint_routing = match db::settings::get(&tx, ENDPOINT_ROUTING_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<EndpointRoutingSettings>(&row.value_json)
            .context("保存的 Endpoint 与模型绑定设置无效")?,
        None => EndpointRoutingSettings::default(),
    };
    let background_jobs = match db::settings::get(&tx, BACKGROUND_JOB_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<PersistedBackgroundJobSettings>(&row.value_json)
            .context("保存的后台任务设置无效")?,
        None => PersistedBackgroundJobSettings::default(),
    };
    settings.auto_run_background_jobs = background_jobs.auto_run;
    // Clamp instead of failing: a hand-edited or truncated row must not prevent
    // the library from opening.
    settings.background_job_concurrency = background_jobs.concurrency.clamp(
        MIN_BACKGROUND_JOB_CONCURRENCY,
        MAX_BACKGROUND_JOB_CONCURRENCY,
    );
    settings.background_job_interval_ms = background_jobs
        .interval_ms
        .min(MAX_BACKGROUND_JOB_INTERVAL_MS);
    settings.pdf_compact_reading = match db::settings::get(&tx, PDF_READER_SETTINGS_KEY)? {
        Some(row) => {
            serde_json::from_str::<PersistedPdfReaderSettings>(&row.value_json)
                .context("保存的 PDF 阅读设置无效")?
                .compact_reading
        }
        None => default_pdf_compact_reading(),
    };
    let translation_settings = match db::settings::get(&tx, TRANSLATION_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<PersistedTranslationSettings>(&row.value_json)
            .context("保存的翻译设置无效")?,
        None => PersistedTranslationSettings::default(),
    };
    settings.default_language = translation_settings.default_language;
    settings.translation_display_mode = translation_settings.display_mode;
    settings.validate()?;
    tx.commit().context("无法完成 AI 设置快照读取")?;
    Ok(settings)
}

fn save_provider_settings(db_path: &Path, settings: &ProviderSettings) -> Result<()> {
    settings.validate()?;
    let value_json = serde_json::to_string(settings).context("failed to serialize AI settings")?;
    // A structural assertion guards future accidental additions to the
    // persisted type as well as today's explicit absence of an API-key field.
    let object = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&value_json)
        .context("AI settings did not serialize to an object")?;
    ensure!(
        !object.keys().any(|key| {
            let key = key.to_ascii_lowercase();
            key.contains("api_key") || key.contains("password") || key.contains("secret")
        }),
        "refusing to persist a secret-bearing AI setting"
    );
    let updated_at = unix_timestamp()?;
    let setting = db::settings::Setting {
        key: PROVIDER_SETTINGS_KEY.to_string(),
        value_json,
        updated_at,
    };
    let generation = db::settings::Setting {
        key: CHAT_GENERATION_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&settings.chat_generation)
            .context("无法序列化对话模型参数")?,
        updated_at,
    };
    let background_jobs = db::settings::Setting {
        key: BACKGROUND_JOB_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedBackgroundJobSettings {
            auto_run: settings.auto_run_background_jobs,
            concurrency: settings.background_job_concurrency,
            interval_ms: settings.background_job_interval_ms,
        })
        .context("无法序列化后台任务设置")?,
        updated_at,
    };
    let pdf_reader = db::settings::Setting {
        key: PDF_READER_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedPdfReaderSettings {
            compact_reading: settings.pdf_compact_reading,
        })
        .context("无法序列化 PDF 阅读设置")?,
        updated_at,
    };
    let translation = db::settings::Setting {
        key: TRANSLATION_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedTranslationSettings {
            default_language: settings.default_language.clone(),
            display_mode: settings.translation_display_mode,
        })
        .context("无法序列化翻译设置")?,
        updated_at,
    };
    let routing = db::settings::Setting {
        key: ENDPOINT_ROUTING_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&settings.endpoint_routing)
            .context("无法序列化 Endpoint 与模型绑定设置")?,
        updated_at,
    };
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法开始保存 AI 设置")?;
    ensure!(
        db::settings::upsert(&tx, &setting)? == 1,
        "AI provider settings were not stored"
    );
    ensure!(
        db::settings::upsert(&tx, &generation)? == 1,
        "对话模型参数未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &background_jobs)? == 1,
        "后台任务设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &pdf_reader)? == 1,
        "PDF 阅读设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &translation)? == 1,
        "翻译设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &routing)? == 1,
        "Endpoint 与模型绑定设置未能保存"
    );
    tx.commit().context("无法提交 AI 设置")?;
    Ok(())
}

fn validate_model_name(kind: &str, model: &str) -> Result<()> {
    ensure!(
        !model.trim().is_empty()
            && model.trim() == model
            && model.chars().count() <= MAX_MODEL_NAME_CHARS
            && !model.chars().any(char::is_control),
        "{kind} model is invalid"
    );
    Ok(())
}

fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_secs())
}

#[cfg(target_os = "windows")]
async fn stop_visual_job_before_replacement(
    coordinator: &VisualJobCoordinator,
    job_id: &str,
) -> Result<()> {
    let Some(record) = coordinator.status(job_id).await? else {
        return Ok(());
    };
    if matches!(
        record.state,
        VisualJobState::Queued | VisualJobState::Running | VisualJobState::Paused
    ) {
        let _ = coordinator.cancel(job_id).await?;
    } else {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(125);
    loop {
        let Some(record) = coordinator.status(job_id).await? else {
            return Ok(());
        };
        if matches!(
            record.state,
            VisualJobState::Succeeded | VisualJobState::Failed | VisualJobState::Cancelled
        ) {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待现有视觉任务停止超时"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(target_os = "windows")]
async fn stop_indexing_jobs_before_visual_replacement(
    indexing: &IndexingCoordinator,
    book_id: &str,
    source_id: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let active = indexing
            .jobs_for_book(book_id)
            .await?
            .into_iter()
            .filter(|job| {
                job.source_id.as_deref() == Some(source_id)
                    && matches!(job.kind.as_str(), "embedding" | "vision")
                    && matches!(
                        job.status,
                        IndexingJobStatus::Queued
                            | IndexingJobStatus::Running
                            | IndexingJobStatus::Paused
                    )
            })
            .collect::<Vec<_>>();
        if active.is_empty() {
            return Ok(());
        }
        for job in active {
            if job.status == IndexingJobStatus::Paused {
                // Paused jobs are not scanned by the worker. Put them back in
                // the queue before setting cancellation so they can publish a
                // durable terminal state.
                let _ = indexing.resume(&job.id).await?;
            }
            let _ = indexing.cancel(&job.id).await?;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待旧视觉理解或向量任务停止超时"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[derive(Clone)]
struct LibraryVisualDocumentSource {
    library: Arc<Mutex<LibraryStore>>,
}

impl VisualDocumentSource for LibraryVisualDocumentSource {
    fn load_document(
        &self,
        book_id: String,
        revision: crate::document::Revision,
    ) -> PreviewFuture<'_, crate::document::BookDocument> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let document = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?
                    .document(&book_id)?;
                ensure!(
                    document.revision == revision,
                    "visual request revision is no longer current"
                );
                Ok(document)
            })
            .await
            .context("visual document loader stopped")?
        })
    }

    fn load_asset(
        &self,
        book_id: String,
        asset_id: String,
    ) -> PreviewFuture<'_, VisualAssetPayload> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let (media_type, expected_len) = library.asset_metadata(&book_id, &asset_id)?;
                ensure!(
                    media_type.starts_with("image/"),
                    "视觉任务只能读取图书所属图片"
                );
                let bytes = library.asset_bytes(&book_id, &asset_id)?;
                ensure!(
                    bytes.len() as u64 == expected_len,
                    "视觉图片对象长度与数据库元数据不一致"
                );
                Ok(VisualAssetPayload { media_type, bytes })
            })
            .await
            .context("visual asset loader stopped")?
        })
    }

    fn load_source(
        &self,
        book_id: String,
        source_id: String,
        revision: crate::document::Revision,
    ) -> PreviewFuture<'_, VisualSourcePayload> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let (format, source_kind, media_type, bytes) =
                    library.visual_source_bytes(&book_id, &source_id, revision)?;
                Ok(VisualSourcePayload {
                    format,
                    source_kind,
                    media_type,
                    bytes,
                })
            })
            .await
            .context("visual source loader stopped")?
        })
    }
}

#[derive(Clone)]
struct LocalVisualPageSink {
    db_path: PathBuf,
    blobs: Arc<LocalBlobStore>,
    blob_publication: BlobPublicationLock,
    indexing: Arc<IndexingCoordinator>,
}

impl VisualPageSink for LocalVisualPageSink {
    fn reset_staging(&self, spec: crate::preview::VisualJobSpec) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let reset_db_path = db_path.clone();
            let unreferenced = tokio::task::spawn_blocking(move || {
                db::transactions::clear_visual_page_staging(
                    &mut db::open_conn(&reset_db_path)?,
                    &spec,
                )
            })
            .await
            .context("visual page staging reset worker stopped")??;
            drop(publication_guard);
            reclaim_unreferenced_blobs(
                &db_path,
                &blobs,
                &blob_publication,
                unreferenced,
                "视觉页面断点对象",
            )
            .await;
            Ok(())
        })
    }

    fn checkpoint_page(
        &self,
        spec: crate::preview::VisualJobSpec,
        page: RenderedVisualPage,
        completed_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let now = unix_timestamp()?;
            let key = blobs.put(&page.bytes).await?;
            ensure!(
                key == crate::storage::BlobKey::from_bytes(&page.bytes),
                "object store returned a non-content-addressed key"
            );
            let blob_row = db::blobs::BlobRecord {
                object_key: key.to_string(),
                media_type: page.media_type.clone(),
                byte_len: page.bytes.len() as u64,
                hash: blake3::hash(&page.bytes).to_hex().to_string(),
                created_at: now,
            };
            let page_row = visual_page_row(&spec, page, key.to_string(), now)?;
            let commit_db_path = db_path.clone();
            tokio::task::spawn_blocking(move || {
                db::transactions::checkpoint_visual_page(
                    &mut db::open_conn(&commit_db_path)?,
                    &spec,
                    &blob_row,
                    &page_row,
                    completed_pages,
                    now,
                )
            })
            .await
            .context("visual page checkpoint worker stopped")??;
            drop(publication_guard);
            Ok(())
        })
    }

    fn commit_pages(
        &self,
        spec: crate::preview::VisualJobSpec,
        total_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        let indexing = Arc::clone(&self.indexing);
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let source_id = spec.source_id.clone();
            let commit_db_path = db_path.clone();
            let now = unix_timestamp()?;
            let unreferenced = tokio::task::spawn_blocking(move || {
                db::transactions::publish_staged_visual_pages(
                    &mut db::open_conn(&commit_db_path)?,
                    &spec,
                    total_pages,
                    now,
                )
            })
            .await
            .context("visual page publication worker stopped")??;
            drop(publication_guard);
            reclaim_unreferenced_blobs(
                &db_path,
                &blobs,
                &blob_publication,
                unreferenced,
                "已替换视觉页面对象",
            )
            .await;
            if let Err(error) = indexing.visual_pages_ready(&source_id).await {
                // Final pages and the Succeeded job state were already
                // committed atomically. Vision jobs are durable and will be
                // discovered by the background scanner, so a wake-up failure
                // must not rewrite the completed render as Failed.
                tracing::warn!(source_id, %error, "视觉页面已发布，但视觉索引即时唤醒失败");
            }
            Ok(())
        })
    }
}

fn visual_page_row(
    spec: &crate::preview::VisualJobSpec,
    page: RenderedVisualPage,
    object_key: String,
    created_at: u64,
) -> Result<db::visual_pages::VisualPage> {
    let fidelity = match page.metadata.fidelity {
        crate::preview::RenderFidelity::Normalized => "normalized",
        crate::preview::RenderFidelity::Structural => "structural",
        crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
    }
    .to_string();
    Ok(db::visual_pages::VisualPage {
        id: page.id,
        book_id: spec.book_id.clone(),
        source_id: spec.source_id.clone(),
        content_unit_id: page.content_unit_id,
        page_index: page.page_index,
        object_key,
        width: page.width,
        height: page.height,
        render_scale: spec.profile.scale(),
        renderer: page.metadata.renderer,
        renderer_version: page.metadata.renderer_version,
        document_revision: page.metadata.document_revision.get(),
        unit_revision: page.metadata.unit_revision.get(),
        profile_id: page.metadata.profile_id,
        fidelity,
        locator_json: serde_json::to_string(&page.locator)
            .context("failed to serialize visual page locator")?,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ai::DEFAULT_OLLAMA_OPENAI_BASE_URL, credentials::MemoryCredentialStore,
        library::ImportOutcome, preview::VisualJobState,
    };
    #[cfg(target_os = "windows")]
    use crate::{
        office_com::{
            OfficeEnhanceOutput, OfficeEnhanceRequest, OfficeEnhancementKind, OfficeFuture,
        },
        office_visual::OfficeEnhancedRenderer,
    };
    #[cfg(target_os = "windows")]
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };
    use std::{
        future::Future,
        io::Cursor,
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
        task::{Context as TaskContext, Poll, Wake, Waker},
        thread,
        time::Duration,
    };

    struct ThreadWake(thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    /// Polls a future on this ordinary test thread without entering any Tokio
    /// runtime. This mirrors GPUI's foreground executor closely enough to catch
    /// service methods that accidentally depend on an ambient Tokio handle.
    fn block_on_without_tokio<F: Future>(future: F) -> F::Output {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
        let mut context = TaskContext::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => thread::park(),
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[derive(Clone, Copy)]
    struct FakeOfficeSlides;

    #[cfg(target_os = "windows")]
    impl OfficeEnhancer for FakeOfficeSlides {
        fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
            Box::pin(async move {
                ensure!(
                    request.kind == OfficeEnhancementKind::PowerPointImages,
                    "test expected PowerPoint enhancement"
                );
                let mut paths = Vec::new();
                for index in 1..=2 {
                    let path = request.target.join(format!("Slide{index}.png"));
                    std::fs::write(&path, tiny_office_slide_png(index as u8))?;
                    paths.push(path);
                }
                Ok(OfficeEnhanceOutput::Images(paths))
            })
        }
    }

    #[cfg(target_os = "windows")]
    struct GatedFakeOfficeSlides {
        started: async_channel::Sender<()>,
        release: async_channel::Receiver<()>,
    }

    #[cfg(target_os = "windows")]
    impl OfficeEnhancer for GatedFakeOfficeSlides {
        fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
            Box::pin(async move {
                self.started.send(()).await?;
                self.release.recv().await?;
                FakeOfficeSlides.enhance(request).await
            })
        }
    }

    #[cfg(target_os = "windows")]
    fn tiny_office_slide_png(channel: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(4, 3, image::Rgba([channel, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn async_library_mutation_turnstile_is_fifo_without_blocking_pool_starvation() {
        const REQUESTS: usize = 128;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let queue = LibraryMutationQueue::new(runtime.handle());
        let reservations = (0..REQUESTS).map(|_| queue.reserve()).collect::<Vec<_>>();
        let order = Arc::new(StdMutex::new(Vec::with_capacity(REQUESTS)));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        runtime.block_on(async move {
            let mut handles = Vec::with_capacity(REQUESTS);
            // Polling the consumers in reverse order must not affect the API
            // boundary ticket order established above.
            for reservation in reservations.into_iter().rev() {
                let ticket = reservation.ticket;
                let order = Arc::clone(&order);
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                handles.push(tokio::spawn(async move {
                    let turn = reservation.enter().await.unwrap();
                    tokio::task::spawn_blocking(move || {
                        let now_active = active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                        max_active.fetch_max(now_active, AtomicOrdering::SeqCst);
                        order.lock().unwrap().push(ticket);
                        std::thread::yield_now();
                        active.fetch_sub(1, AtomicOrdering::SeqCst);
                        drop(turn);
                    })
                    .await
                    .unwrap();
                }));
            }

            tokio::time::timeout(Duration::from_secs(5), async {
                for handle in handles {
                    handle.await.unwrap();
                }
            })
            .await
            .expect("queued mutations starved a one-thread blocking pool");

            assert_eq!(max_active.load(AtomicOrdering::SeqCst), 1);
            assert_eq!(
                *order.lock().unwrap(),
                (0..REQUESTS as u64).collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn composes_default_local_services_in_an_isolated_directory() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services = AppServices::open_with_credentials(temp.path(), credentials).unwrap();

        let provider_settings = services.provider_settings().unwrap();
        assert_eq!(provider_settings.base_url, DEFAULT_OLLAMA_OPENAI_BASE_URL);
        assert_eq!(provider_settings.chat_model, "qwen3.5:0.8b");
        assert_eq!(provider_settings.embedding_model, "qwen3-embedding:0.6b");
        assert_eq!(provider_settings.vision_model, "qwen3.5:0.8b");
        assert_eq!(
            services.search().unwrap().embedding_model(),
            DEFAULT_EMBEDDING_MODEL
        );
        assert!(services.blob_store().root().starts_with(temp.path()));
        assert!(services.visual_jobs().is_some());

        let title = services
            .runtime()
            .block_on(async {
                services
                    .spawn_library(|library| {
                        Ok(library.create_book("Service book", "Author")?.title)
                    })
                    .await
                    .context("library test worker stopped")?
            })
            .unwrap();
        assert_eq!(title, "Service book");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn office_opt_in_replaces_persisted_visual_pages_through_app_services() {
        use office_oxide::{DocumentFormat, create::create_from_markdown_to_writer};

        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let source: Arc<dyn VisualDocumentSource> = Arc::new(LibraryVisualDocumentSource {
            library: Arc::clone(&services.library),
        });
        let sink: Arc<dyn VisualPageSink> = Arc::new(LocalVisualPageSink {
            db_path: services.db_path.clone(),
            blobs: Arc::clone(&services.blobs),
            blob_publication: services.blob_publication.clone(),
            indexing: Arc::clone(&services.indexing),
        });
        let (started_tx, started_rx) = async_channel::bounded(1);
        let (release_tx, release_rx) = async_channel::bounded(1);
        services
            .replace_visual_renderers(
                source,
                sink,
                vec![
                    Arc::new(StructuralPngRenderer),
                    Arc::new(OfficeEnhancedRenderer::new(Arc::new(
                        GatedFakeOfficeSlides {
                            started: started_tx,
                            release: release_rx,
                        },
                    ))),
                ],
            )
            .unwrap();

        let mut pptx = Cursor::new(Vec::new());
        create_from_markdown_to_writer(
            "# First\n\nOfficeEnhancedOne\n\n---\n\n# Second\n\nOfficeEnhancedTwo",
            DocumentFormat::Pptx,
            &mut pptx,
        )
        .unwrap();
        let source_path = temp.path().join("enhanced.pptx");
        std::fs::write(&source_path, pptx.into_inner()).unwrap();
        let runtime = services.runtime();
        let record = runtime
            .block_on(async {
                services
                    .spawn_library(move |library| match library.import(&source_path)? {
                        ImportOutcome::Added(record) | ImportOutcome::AlreadyExists(record) => {
                            Ok(record)
                        }
                    })
                    .await
                    .context("Office test import worker stopped")?
            })
            .unwrap();
        let source_id = db::book_sources::get_revision(
            &db::open_conn(&services.db_path).unwrap(),
            &record.id,
            record.revision,
        )
        .unwrap()
        .unwrap()
        .id;
        let job_id = format!("visual-render:{source_id}");

        let submitted_job_id = runtime
            .block_on(services.set_office_enhancement_enabled(record.id.clone(), true))
            .unwrap();
        assert_eq!(submitted_job_id, job_id);
        assert!(
            runtime
                .block_on(services.office_enhancement_enabled(record.id.clone()))
                .unwrap()
        );
        let coordinator = services.visual_jobs().unwrap();
        let running = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), started_rx.recv()).await??;
                let running = coordinator.status(&job_id).await?.unwrap();
                assert_eq!(running.state, VisualJobState::Running);
                assert!(!coordinator.recover(&job_id).await?);
                // Deterministically place the caller's post-commit notification
                // after worker takeover, without a timing-based sleep.
                coordinator.schedule_committed(&running.spec).await?;
                release_tx.send(()).await?;
                Ok::<_, anyhow::Error>(running)
            })
            .unwrap();
        let succeeded = runtime
            .block_on(services.visual_jobs().unwrap().wait_for_state(
                &job_id,
                VisualJobState::Succeeded,
                Duration::from_secs(5),
            ))
            .unwrap();
        assert_eq!(succeeded.spec, running.spec);
        assert_eq!(succeeded.attempts, 1);
        assert!(!runtime.block_on(coordinator.recover(&job_id)).unwrap());
        runtime
            .block_on(coordinator.schedule_committed(&running.spec))
            .expect("a completed committed Office job is still successfully submitted");
        assert_eq!(
            runtime
                .block_on(coordinator.status(&job_id))
                .unwrap()
                .unwrap(),
            succeeded,
            "late confirmation must not reset the completed job"
        );
        let pages = db::visual_pages::list_for_source(
            &db::open_conn(&services.db_path).unwrap(),
            &source_id,
        )
        .unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages.iter().all(|page| {
            page.renderer == "moye-office-com-enhanced"
                && page.fidelity == "office_enhanced"
                && serde_json::from_str::<crate::document::DocumentLocator>(&page.locator_json)
                    .unwrap()
                    .source
                    .is_some()
        }));
        let loaded_pages = runtime
            .block_on(services.load_office_enhanced_pages(record.id.clone()))
            .unwrap();
        assert_eq!(loaded_pages.len(), 2);
        assert_eq!(loaded_pages[0].file_name, "Page00001.png");
        assert_eq!(loaded_pages[1].file_name, "Page00002.png");
        assert!(loaded_pages.iter().all(|page| {
            page.media_type == "image/png"
                && !page.bytes.is_empty()
                && page
                    .content_unit_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty())
                && page.locator.source.is_some()
        }));

        let submitted_job_id = runtime
            .block_on(services.set_office_enhancement_enabled(record.id.clone(), false))
            .unwrap();
        assert_eq!(submitted_job_id, job_id);
        assert!(
            !runtime
                .block_on(services.office_enhancement_enabled(record.id.clone()))
                .unwrap()
        );
        runtime
            .block_on(services.visual_jobs().unwrap().wait_for_state(
                &job_id,
                VisualJobState::Succeeded,
                Duration::from_secs(5),
            ))
            .unwrap();
        let pages = db::visual_pages::list_for_source(
            &db::open_conn(&services.db_path).unwrap(),
            &source_id,
        )
        .unwrap();
        assert!(!pages.is_empty());
        assert!(pages.iter().all(|page| page.fidelity == "structural"));
        assert!(
            runtime
                .block_on(services.load_office_enhanced_pages(record.id))
                .is_err()
        );
    }

    #[test]
    fn projected_mutations_follow_commit_order_and_reads_do_not_advance_generation() {
        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let runtime = services.runtime();
        assert_eq!(services.library_generation(), 0);

        let initial_count = runtime
            .block_on(async {
                services
                    .spawn_library_read(|library| Ok(library.books().len()))
                    .await
                    .context("library read worker stopped")?
            })
            .unwrap();
        assert_eq!(initial_count, 0);
        assert_eq!(services.library_generation(), 0);

        let first = runtime
            .block_on(async {
                services
                    .spawn_library_projected(|library| {
                        library.create_book("Projected one", "Author")
                    })
                    .await
                    .context("first projected mutation stopped")?
            })
            .unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(first.snapshot.books().len(), 1);
        assert_eq!(services.library_generation(), 1);

        let error = runtime
            .block_on(async {
                services
                    .spawn_library_projected::<(), _>(|_| anyhow::bail!("expected failure"))
                    .await
                    .context("failed projected mutation stopped")?
            })
            .unwrap_err();
        assert!(error.to_string().contains("expected failure"));
        assert_eq!(services.library_generation(), 1);

        let second = runtime
            .block_on(async {
                services
                    .spawn_library_projected(|library| {
                        library.create_book("Projected two", "Author")
                    })
                    .await
                    .context("second projected mutation stopped")?
            })
            .unwrap();
        assert_eq!(second.generation, 2);
        assert_eq!(second.snapshot.books().len(), 2);
        assert_eq!(services.library_generation(), 2);
    }

    #[test]
    fn provider_settings_survive_restart_without_persisting_the_api_key() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let settings = ProviderSettings {
            base_url: "https://models.example.test/v1".to_string(),
            chat_model: "chat-test".to_string(),
            chat_generation: ChatGenerationSettings {
                temperature: None,
                top_p: Some(0.8),
                max_output_tokens: 512,
                presence_penalty: Some(0.3),
                frequency_penalty: Some(-0.4),
            },
            auto_run_background_jobs: false,
            background_job_concurrency: 3,
            background_job_interval_ms: 250,
            pdf_compact_reading: true,
            embedding_model: "embed-test".to_string(),
            vision_model: "vision-test".to_string(),
            remote_content_confirmed: true,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: "https://models.example.test/v1/".to_string(),
            request_timeout_secs: 30,
            ..Default::default()
        };
        block_on_without_tokio(services.configure_provider(
            settings.clone(),
            ApiKeyUpdate::Set("credential-only-secret".to_string()),
        ))
        .unwrap();
        assert_eq!(
            credentials
                .api_key(
                    normalize_provider_base_url(&settings.base_url)
                        .unwrap()
                        .as_str()
                )
                .unwrap()
                .as_deref(),
            Some("credential-only-secret")
        );

        let row = db::settings::get(
            &db::open_conn(services.database_path()).unwrap(),
            PROVIDER_SETTINGS_KEY,
        )
        .unwrap()
        .unwrap();
        assert!(!row.value_json.contains("credential-only-secret"));
        assert!(!row.value_json.to_ascii_lowercase().contains("api_key"));
        assert!(!row.value_json.contains("chat_generation"));
        assert!(!row.value_json.contains("auto_run_background_jobs"));
        assert!(!row.value_json.contains("pdf_compact_reading"));
        assert!(!row.value_json.contains("endpoint_routing"));
        let generation_row = db::settings::get(
            &db::open_conn(services.database_path()).unwrap(),
            CHAT_GENERATION_SETTINGS_KEY,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<ChatGenerationSettings>(&generation_row.value_json).unwrap(),
            settings.chat_generation
        );
        let background_job_row = db::settings::get(
            &db::open_conn(services.database_path()).unwrap(),
            BACKGROUND_JOB_SETTINGS_KEY,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<PersistedBackgroundJobSettings>(&background_job_row.value_json)
                .unwrap(),
            PersistedBackgroundJobSettings {
                auto_run: false,
                concurrency: 3,
                interval_ms: 250,
            }
        );
        let pdf_reader_row = db::settings::get(
            &db::open_conn(services.database_path()).unwrap(),
            PDF_READER_SETTINGS_KEY,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<PersistedPdfReaderSettings>(&pdf_reader_row.value_json).unwrap(),
            PersistedPdfReaderSettings {
                compact_reading: true
            }
        );
        drop(services);

        let reopened = AppServices::open_with_credentials(temp.path(), credentials).unwrap();
        assert_eq!(reopened.provider_settings().unwrap(), settings);
        assert_eq!(reopened.search().unwrap().embedding_model(), "embed-test");
    }

    fn endpoint_fixture(id: &str, port: u16) -> EndpointSettings {
        EndpointSettings {
            id: id.into(),
            name: format!("Endpoint {id}"),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: String::new(),
            request_timeout_secs: 30,
        }
    }

    fn routed_settings() -> ProviderSettings {
        let mut settings = ProviderSettings::default();
        settings.endpoint_routing.additional_endpoints = vec![
            endpoint_fixture("embed", 21435),
            endpoint_fixture("vision", 21436),
        ];
        settings.endpoint_routing.embedding_endpoint_id = "embed".into();
        settings.endpoint_routing.vision_endpoint_id = "vision".into();
        settings
    }

    #[test]
    fn endpoint_routing_rejects_missing_duplicate_and_unconfirmed_endpoints() {
        let settings = routed_settings();
        settings.validate().unwrap();
        assert_eq!(
            settings.endpoint_for(ModelRole::Chat).unwrap().id,
            DEFAULT_ENDPOINT_ID
        );
        assert_eq!(
            settings.endpoint_for(ModelRole::Embedding).unwrap().id,
            "embed"
        );
        assert_eq!(
            settings.endpoint_for(ModelRole::Vision).unwrap().id,
            "vision"
        );

        let mut dangling = settings.clone();
        dangling.endpoint_routing.embedding_endpoint_id = "removed".into();
        assert!(dangling.validate().is_err());
        let mut duplicate_id = settings.clone();
        duplicate_id.endpoint_routing.additional_endpoints[0].id = DEFAULT_ENDPOINT_ID.into();
        assert!(duplicate_id.validate().is_err());
        let mut duplicate_url = settings.clone();
        duplicate_url.endpoint_routing.additional_endpoints[0].base_url =
            settings.base_url.trim_end_matches('/').into();
        assert!(
            duplicate_url
                .validate()
                .unwrap_err()
                .to_string()
                .contains("地址重复")
        );
        let mut remote = settings.clone();
        let endpoint = &mut remote.endpoint_routing.additional_endpoints[0];
        endpoint.base_url = "https://models.example.test/v1".into();
        endpoint.remote_content_confirmed = true;
        endpoint.confirmed_remote_endpoint = "https://previous.example.test/v1/".into();
        assert!(remote.validate().is_err());
        remote.endpoint_routing.additional_endpoints[0].confirmed_remote_endpoint =
            "https://models.example.test/v1/".into();
        remote.validate().unwrap();
    }

    #[test]
    fn routed_endpoint_identity_ignores_unrelated_chat_and_tracks_its_own_url() {
        let settings = routed_settings();
        let embedding = embedding_execution_identity(&settings).unwrap();
        let vision = vision_execution_identity(&settings).unwrap();
        let mut changed = settings.clone();
        changed.base_url = "http://127.0.0.1:21437/v1/".into();
        changed.chat_model = "changed-chat".into();
        changed.endpoint_routing.default_endpoint_name = "Renamed".into();
        assert_eq!(embedding_execution_identity(&changed).unwrap(), embedding);
        assert_eq!(vision_execution_identity(&changed).unwrap(), vision);
        changed.endpoint_routing.additional_endpoints[0].base_url =
            "http://127.0.0.1:21438/v1".into();
        assert_ne!(embedding_execution_identity(&changed).unwrap(), embedding);
        assert_eq!(vision_execution_identity(&changed).unwrap(), vision);
        changed.endpoint_routing.additional_endpoints[1].base_url =
            "http://127.0.0.1:21439/v1".into();
        assert_ne!(vision_execution_identity(&changed).unwrap(), vision);
    }

    #[test]
    fn routing_and_independent_keys_survive_restart_without_copying_changed_urls() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let settings = routed_settings();
        let updates = BTreeMap::from([
            (
                DEFAULT_ENDPOINT_ID.into(),
                ApiKeyUpdate::Set("default-secret".into()),
            ),
            ("embed".into(), ApiKeyUpdate::Set("embed-secret".into())),
            ("vision".into(), ApiKeyUpdate::Set("vision-secret".into())),
        ]);
        block_on_without_tokio(services.configure_providers(settings.clone(), updates)).unwrap();
        for (id, expected) in [
            (DEFAULT_ENDPOINT_ID, "default-secret"),
            ("embed", "embed-secret"),
            ("vision", "vision-secret"),
        ] {
            let endpoint = settings.endpoint_by_id(id).unwrap();
            let target = normalize_provider_base_url(&endpoint.base_url).unwrap();
            assert_eq!(
                credentials.api_key(target.as_str()).unwrap().as_deref(),
                Some(expected)
            );
        }
        let rows = db::settings::list(&db::open_conn(services.database_path()).unwrap()).unwrap();
        assert!(rows.iter().all(|row| !row.value_json.contains("-secret")));
        assert!(
            rows.iter()
                .any(|row| row.key == ENDPOINT_ROUTING_SETTINGS_KEY)
        );
        drop(services);
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        assert_eq!(services.provider_settings().unwrap(), settings);
        let mut changed = settings.clone();
        changed.endpoint_routing.additional_endpoints[0].base_url =
            "http://127.0.0.1:21438/v1".into();
        block_on_without_tokio(services.configure_providers(changed.clone(), BTreeMap::new()))
            .unwrap();
        assert!(
            credentials
                .api_key("http://127.0.0.1:21435/v1/")
                .unwrap()
                .is_none()
        );
        assert!(
            credentials
                .api_key("http://127.0.0.1:21438/v1/")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            credentials
                .api_key("http://127.0.0.1:21436/v1/")
                .unwrap()
                .as_deref(),
            Some("vision-secret")
        );
        assert_eq!(
            load_provider_settings(services.database_path()).unwrap(),
            changed
        );
    }

    #[test]
    fn routing_save_failure_rolls_back_all_credentials_and_setting_rows() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let previous = services.provider_settings().unwrap();
        let conn = db::open_conn(services.database_path()).unwrap();
        let previous_rows = db::settings::list(&conn).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER test_endpoint_routing_save_failure
             BEFORE INSERT ON settings
             WHEN NEW.key = 'ai.openai_compatible.endpoint_routing.v1'
             BEGIN SELECT RAISE(ABORT, 'injected endpoint routing save failure'); END;",
        )
        .unwrap();
        let next = routed_settings();
        let updates = BTreeMap::from([
            (
                DEFAULT_ENDPOINT_ID.into(),
                ApiKeyUpdate::Set("default-new-secret".into()),
            ),
            ("embed".into(), ApiKeyUpdate::Set("embed-new-secret".into())),
            (
                "vision".into(),
                ApiKeyUpdate::Set("vision-new-secret".into()),
            ),
        ]);
        assert!(
            block_on_without_tokio(services.configure_providers(next.clone(), updates)).is_err()
        );
        assert_eq!(services.provider_settings().unwrap(), previous);
        assert_eq!(db::settings::list(&conn).unwrap(), previous_rows);
        for endpoint in next.endpoints() {
            let target = normalize_provider_base_url(&endpoint.base_url).unwrap();
            assert!(credentials.api_key(target.as_str()).unwrap().is_none());
        }
        conn.execute_batch("DROP TRIGGER test_endpoint_routing_save_failure")
            .unwrap();
    }

    #[derive(Default)]
    struct FailOnceCredentialStore {
        memory: MemoryCredentialStore,
        fail_next_write: AtomicBool,
    }

    impl CredentialStore for FailOnceCredentialStore {
        fn set_api_key(&self, target: &str, value: &str) -> Result<()> {
            if target == "http://127.0.0.1:21435/v1/"
                && self.fail_next_write.swap(false, Ordering::SeqCst)
            {
                anyhow::bail!("injected credential backend failure");
            }
            self.memory.set_api_key(target, value)
        }

        fn api_key(&self, target: &str) -> Result<Option<String>> {
            self.memory.api_key(target)
        }

        fn delete_api_key(&self, target: &str) -> Result<()> {
            self.memory.delete_api_key(target)
        }
    }

    #[test]
    fn partial_credential_failure_restores_every_endpoint_and_keeps_settings() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(FailOnceCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let settings = routed_settings();
        let updates = BTreeMap::from([
            (
                DEFAULT_ENDPOINT_ID.into(),
                ApiKeyUpdate::Set("default-old-secret".into()),
            ),
            ("embed".into(), ApiKeyUpdate::Set("embed-old-secret".into())),
            (
                "vision".into(),
                ApiKeyUpdate::Set("vision-old-secret".into()),
            ),
        ]);
        block_on_without_tokio(services.configure_providers(settings.clone(), updates)).unwrap();
        let before_rows =
            db::settings::list(&db::open_conn(services.database_path()).unwrap()).unwrap();
        credentials.fail_next_write.store(true, Ordering::SeqCst);
        let mut changed = settings.clone();
        changed.chat_model = "must-not-publish".into();
        let updates = BTreeMap::from([
            (
                DEFAULT_ENDPOINT_ID.into(),
                ApiKeyUpdate::Set("default-new-secret".into()),
            ),
            ("embed".into(), ApiKeyUpdate::Set("embed-new-secret".into())),
            ("vision".into(), ApiKeyUpdate::Delete),
        ]);
        assert!(block_on_without_tokio(services.configure_providers(changed, updates)).is_err());
        assert_eq!(services.provider_settings().unwrap(), settings);
        assert_eq!(
            db::settings::list(&db::open_conn(services.database_path()).unwrap()).unwrap(),
            before_rows
        );
        for (id, expected) in [
            (DEFAULT_ENDPOINT_ID, "default-old-secret"),
            ("embed", "embed-old-secret"),
            ("vision", "vision-old-secret"),
        ] {
            let target =
                normalize_provider_base_url(&settings.endpoint_by_id(id).unwrap().base_url)
                    .unwrap();
            assert_eq!(
                credentials.api_key(target.as_str()).unwrap().as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn invalid_persisted_routing_is_preserved_and_reported() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_or_recreate(&db_path).unwrap();
        let dangling = EndpointRoutingSettings {
            vision_endpoint_id: "missing".into(),
            ..EndpointRoutingSettings::default()
        };
        let mut unknown = serde_json::to_value(EndpointRoutingSettings::default()).unwrap();
        unknown["extra"] = serde_json::json!(true);
        for value_json in [
            "{".into(),
            "{}".into(),
            serde_json::to_string(&dangling).unwrap(),
            unknown.to_string(),
        ] {
            let row = db::settings::Setting {
                key: ENDPOINT_ROUTING_SETTINGS_KEY.into(),
                value_json,
                updated_at: 1,
            };
            db::settings::upsert(&conn, &row).unwrap();
            assert!(load_provider_settings(&db_path).is_err());
            assert_eq!(
                db::settings::get(&conn, ENDPOINT_ROUTING_SETTINGS_KEY).unwrap(),
                Some(row)
            );
        }
    }

    #[test]
    fn background_job_logs_enforce_book_scope_and_job_existence() {
        use crate::job_diagnostics::{JobLogEvent, JobLogMetrics, record_for_database};

        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let created = block_on_without_tokio(async {
            services
                .spawn_library_projected(|library| library.create_book("日志范围", "作者"))
                .await
                .context("library mutation worker stopped")?
        })
        .unwrap();
        let conn = db::open_conn(services.database_path()).unwrap();
        let jobs = db::index_jobs::list_for_book(&conn, &created.value.id).unwrap();
        let job_id = jobs[0].id.clone();
        record_for_database(
            services.database_path(),
            &job_id,
            JobLogEvent::RunStarted,
            JobLogMetrics::default(),
        );
        let logs = block_on_without_tokio(
            services.background_job_logs(job_id.clone(), vec![created.value.id.clone()]),
        )
        .unwrap();
        assert_eq!(logs.entries.len(), 1);
        for scope in [Vec::new(), vec!["another-book".into()]] {
            assert!(
                block_on_without_tokio(services.background_job_logs(job_id.clone(), scope))
                    .is_err()
            );
        }
        db::index_jobs::delete(&conn, &job_id).unwrap();
        assert!(
            block_on_without_tokio(services.background_job_logs(job_id, vec![created.value.id]))
                .is_err()
        );
    }

    #[test]
    fn configured_background_job_policy_reaches_service_library_mutations() {
        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let mut settings = services.provider_settings().unwrap();
        settings.auto_run_background_jobs = false;
        block_on_without_tokio(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
            .unwrap();

        let created = block_on_without_tokio(async {
            services
                .spawn_library_projected(|library| library.create_book("暂停服务派生任务", "作者"))
                .await
                .context("library mutation worker stopped")?
        })
        .unwrap();
        let jobs = db::index_jobs::list_for_book(
            &db::open_conn(services.database_path()).unwrap(),
            &created.value.id,
        )
        .unwrap();
        assert_eq!(jobs.len(), 3);
        assert!(jobs.iter().all(|job| {
            job.status == db::index_jobs::IndexJobStatus::Paused
                && job.attempts == 0
                && job.started_at.is_none()
                && job.finished_at.is_none()
        }));
        assert_eq!(services.provider_settings().unwrap(), settings);

        settings.auto_run_background_jobs = true;
        block_on_without_tokio(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
            .unwrap();
        let existing_jobs = db::index_jobs::list_for_book(
            &db::open_conn(services.database_path()).unwrap(),
            &created.value.id,
        )
        .unwrap();
        assert_eq!(existing_jobs.len(), 3);
        assert!(existing_jobs.iter().all(|job| {
            job.status == db::index_jobs::IndexJobStatus::Paused
                && job.attempts == 0
                && job.started_at.is_none()
                && job.finished_at.is_none()
        }));
        assert_eq!(services.provider_settings().unwrap(), settings);
    }

    #[test]
    fn existing_provider_settings_start_with_unconfigured_generation_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_or_recreate(&db_path).unwrap();
        let settings = ProviderSettings {
            chat_model: "existing-chat-choice".into(),
            ..Default::default()
        };
        let provider_row = db::settings::Setting {
            key: PROVIDER_SETTINGS_KEY.into(),
            value_json: serde_json::to_string(&settings).unwrap(),
            updated_at: 1,
        };
        db::settings::upsert(&conn, &provider_row).unwrap();
        assert!(
            db::settings::get(&conn, CHAT_GENERATION_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
        assert!(
            db::settings::get(&conn, BACKGROUND_JOB_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
        assert!(
            db::settings::get(&conn, PDF_READER_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
        drop(conn);

        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        assert_eq!(services.provider_settings().unwrap(), settings);
        assert!(
            !services
                .provider_settings()
                .unwrap()
                .auto_run_background_jobs
        );
        // A book-less install defaults to the ordinary page gap and does not
        // write the reading preference until the user saves it.
        assert!(!services.provider_settings().unwrap().pdf_compact_reading);
        let conn = db::open_conn(services.database_path()).unwrap();
        assert_eq!(
            db::settings::get(&conn, PROVIDER_SETTINGS_KEY).unwrap(),
            Some(provider_row)
        );
        assert!(
            db::settings::get(&conn, CHAT_GENERATION_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
        assert!(
            db::settings::get(&conn, BACKGROUND_JOB_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
        assert!(
            db::settings::get(&conn, PDF_READER_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn translation_language_defaults_off_and_round_trips_through_its_own_key() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_or_recreate(&db_path).unwrap();
        assert!(
            db::settings::get(&conn, TRANSLATION_SETTINGS_KEY)
                .unwrap()
                .is_none()
        );
        drop(conn);

        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        assert_eq!(services.provider_settings().unwrap().default_language, None);

        let mut settings = services.provider_settings().unwrap();
        settings.default_language = Some("ja".to_string());
        block_on_without_tokio(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
            .unwrap();
        assert_eq!(
            services
                .provider_settings()
                .unwrap()
                .default_language
                .as_deref(),
            Some("ja")
        );

        let invalid = ProviderSettings {
            default_language: Some("not-a-preset".to_string()),
            ..Default::default()
        };
        assert!(invalid.validate().is_err());

        let conn = db::open_conn(services.database_path()).unwrap();
        let stored = db::settings::get(&conn, TRANSLATION_SETTINGS_KEY)
            .unwrap()
            .expect("translation settings must be persisted under their own key");
        assert_eq!(
            serde_json::from_str::<PersistedTranslationSettings>(&stored.value_json)
                .unwrap()
                .default_language
                .as_deref(),
            Some("ja")
        );
        let provider = db::settings::get(&conn, PROVIDER_SETTINGS_KEY)
            .unwrap()
            .expect("provider settings must stay persisted");
        assert!(
            !provider.value_json.contains("default_language"),
            "the translation language must not leak into the provider JSON contract",
        );
    }

    #[test]
    fn malformed_or_invalid_saved_generation_is_reported_and_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_or_recreate(&db_path).unwrap();
        let mut out_of_bounds = serde_json::to_value(ChatGenerationSettings::default()).unwrap();
        out_of_bounds["max_output_tokens"] = serde_json::json!(0);
        let mut unknown_field = serde_json::to_value(ChatGenerationSettings::default()).unwrap();
        unknown_field["extra"] = serde_json::json!(true);
        for value_json in [
            "{".to_string(),
            "{}".to_string(),
            out_of_bounds.to_string(),
            unknown_field.to_string(),
        ] {
            let row = db::settings::Setting {
                key: CHAT_GENERATION_SETTINGS_KEY.into(),
                value_json,
                updated_at: 1,
            };
            db::settings::upsert(&conn, &row).unwrap();
            assert!(load_provider_settings(&db_path).is_err());
            assert_eq!(
                db::settings::get(&conn, CHAT_GENERATION_SETTINGS_KEY).unwrap(),
                Some(row)
            );
        }
    }

    #[test]
    fn malformed_saved_background_job_setting_is_reported_and_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_or_recreate(&db_path).unwrap();
        for value_json in [
            "{".to_string(),
            "{}".to_string(),
            serde_json::json!({ "auto_run": "yes" }).to_string(),
            serde_json::json!({ "auto_run": true, "extra": false }).to_string(),
        ] {
            let row = db::settings::Setting {
                key: BACKGROUND_JOB_SETTINGS_KEY.into(),
                value_json,
                updated_at: 1,
            };
            db::settings::upsert(&conn, &row).unwrap();
            assert!(load_provider_settings(&db_path).is_err());
            assert_eq!(
                db::settings::get(&conn, BACKGROUND_JOB_SETTINGS_KEY).unwrap(),
                Some(row)
            );
        }
    }

    #[test]
    fn background_job_scheduling_setting_is_validated_and_clamped_on_load() {
        for settings in [
            ProviderSettings {
                background_job_concurrency: 0,
                ..Default::default()
            },
            ProviderSettings {
                background_job_concurrency: MAX_BACKGROUND_JOB_CONCURRENCY + 1,
                ..Default::default()
            },
            ProviderSettings {
                background_job_interval_ms: MAX_BACKGROUND_JOB_INTERVAL_MS + 1,
                ..Default::default()
            },
        ] {
            assert!(settings.validate().is_err());
        }

        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_or_recreate(&db_path).unwrap();
        let out_of_range = db::settings::Setting {
            key: BACKGROUND_JOB_SETTINGS_KEY.into(),
            value_json: serde_json::json!({
                "auto_run": true,
                "concurrency": 0,
                "interval_ms": u64::MAX,
            })
            .to_string(),
            updated_at: 1,
        };
        db::settings::upsert(&conn, &out_of_range).unwrap();
        // A damaged value must not make the library unopenable.
        let settings = load_provider_settings(&db_path).unwrap();
        assert!(settings.auto_run_background_jobs);
        assert_eq!(
            settings.background_job_concurrency,
            MIN_BACKGROUND_JOB_CONCURRENCY
        );
        assert_eq!(
            settings.background_job_interval_ms,
            MAX_BACKGROUND_JOB_INTERVAL_MS
        );

        // A row written before the scheduling options existed still loads with
        // the documented defaults.
        let legacy = db::settings::Setting {
            key: BACKGROUND_JOB_SETTINGS_KEY.into(),
            value_json: serde_json::json!({ "auto_run": false }).to_string(),
            updated_at: 2,
        };
        db::settings::upsert(&conn, &legacy).unwrap();
        let settings = load_provider_settings(&db_path).unwrap();
        assert_eq!(
            settings.background_job_concurrency,
            DEFAULT_BACKGROUND_JOB_CONCURRENCY
        );
        assert_eq!(
            settings.background_job_interval_ms,
            DEFAULT_BACKGROUND_JOB_INTERVAL_MS
        );
    }

    #[test]
    fn generation_save_failure_rolls_back_provider_generation_and_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let previous = services.provider_settings().unwrap();
        block_on_without_tokio(services.configure_provider(previous.clone(), ApiKeyUpdate::Keep))
            .unwrap();
        let conn = db::open_conn(services.database_path()).unwrap();
        let previous_rows = db::settings::list(&conn).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER test_chat_generation_save_failure
             BEFORE INSERT ON settings
             WHEN NEW.key = 'ai.openai_compatible.chat_generation.v1'
             BEGIN
                 SELECT RAISE(ABORT, 'injected chat generation save failure');
             END;",
        )
        .unwrap();
        let mut next = previous.clone();
        next.chat_model = "must-not-persist".into();
        next.chat_generation.max_output_tokens = 128;
        assert!(
            block_on_without_tokio(services.configure_provider(
                next,
                ApiKeyUpdate::Set("must-rollback-generation-test".into())
            ))
            .is_err()
        );
        assert_eq!(services.provider_settings().unwrap(), previous);
        assert_eq!(
            load_provider_settings(services.database_path()).unwrap(),
            previous
        );
        assert_eq!(db::settings::list(&conn).unwrap(), previous_rows);
        assert!(credentials.api_key(&previous.base_url).unwrap().is_none());
        conn.execute_batch("DROP TRIGGER test_chat_generation_save_failure")
            .unwrap();
    }

    #[test]
    fn embedding_identity_uses_only_canonical_endpoint_and_embedding_model() {
        let base = ProviderSettings::default();
        let expected = embedding_execution_identity(&base).unwrap();
        let mut unrelated = base.clone();
        unrelated.base_url = format!("{}/", unrelated.base_url.trim_end_matches('/'));
        unrelated.chat_model = "chat-other".to_string();
        unrelated.chat_generation.max_output_tokens = 128;
        unrelated.chat_generation.temperature = None;
        unrelated.vision_model = "vision-other".to_string();
        unrelated.request_timeout_secs = 1;
        unrelated.remote_content_confirmed = true;
        unrelated.allow_insecure_remote_http = true;
        unrelated.confirmed_remote_endpoint = "ignored-for-identity".to_string();
        assert_eq!(embedding_execution_identity(&unrelated).unwrap(), expected);

        let mut different_model = base.clone();
        different_model.embedding_model = "embed-other".to_string();
        assert_ne!(
            embedding_execution_identity(&different_model).unwrap(),
            expected
        );
        let mut different_endpoint = base;
        different_endpoint.base_url = "http://127.0.0.1:11435/v1".to_string();
        assert_ne!(
            embedding_execution_identity(&different_endpoint).unwrap(),
            expected
        );
    }

    #[test]
    fn embedding_identity_changes_when_dimensions_change() {
        let base = ProviderSettings::default();
        let expected = embedding_execution_identity(&base).unwrap();
        let mut different_dimensions = base.clone();
        different_dimensions.embedding_dimensions = 768;
        assert_ne!(
            embedding_execution_identity(&different_dimensions).unwrap(),
            expected
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn chat_timeout_and_key_changes_do_not_requeue_embedding_work() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let book = block_on_without_tokio(async {
            services
                .spawn_library(|library| library.create_book("Embedding identity", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();
        let conn = db::open_conn(services.database_path()).unwrap();
        let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap();
        drop(conn);
        services
            .runtime()
            .block_on(stop_indexing_jobs_before_visual_replacement(
                &services.indexing,
                &book.id,
                &source.id,
            ))
            .unwrap();
        let before = db::index_jobs::list_for_source_kind(
            &db::open_conn(services.database_path()).unwrap(),
            &source.id,
            "embedding",
        )
        .unwrap();
        assert!(!before.is_empty());

        let mut next = services.provider_settings().unwrap();
        next.chat_model = "chat-only-change".to_string();
        next.chat_generation.max_output_tokens = 512;
        next.chat_generation.top_p = Some(0.75);
        next.request_timeout_secs = (next.request_timeout_secs + 1).min(600);
        block_on_without_tokio(
            services.configure_provider(next, ApiKeyUpdate::Set("key-only-change".to_string())),
        )
        .unwrap();

        let after = db::index_jobs::list_for_source_kind(
            &db::open_conn(services.database_path()).unwrap(),
            &source.id,
            "embedding",
        )
        .unwrap();
        assert_eq!(after, before);
        assert_eq!(
            credentials
                .api_key(DEFAULT_OLLAMA_OPENAI_BASE_URL)
                .unwrap()
                .as_deref(),
            Some("key-only-change")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn provider_configuration_rolls_back_when_index_reconfigure_fails() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        let book = block_on_without_tokio(async {
            services
                .spawn_library(|library| library.create_book("Provider rollback", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();
        let conn = db::open_conn(services.database_path()).unwrap();
        let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap();
        services
            .runtime()
            .block_on(stop_indexing_jobs_before_visual_replacement(
                &services.indexing,
                &book.id,
                &source.id,
            ))
            .unwrap();
        let vision_id = format!("vision:{}", source.id);
        let before_job = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
        let previous = services.provider_settings().unwrap();
        let previous_rows = db::settings::list(&conn).unwrap();
        let previous_snapshot = services.ai_request_snapshot().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER test_provider_reconfigure_failure
             BEFORE INSERT ON index_jobs
             WHEN NEW.kind = 'embedding'
              AND instr(NEW.cursor_json, '\"model\":\"embed-rollback-test\"') > 0
             BEGIN
                 SELECT RAISE(ABORT, 'injected provider reconfigure failure');
             END;",
        )
        .unwrap();

        let mut next = routed_settings();
        next.chat_model = "chat-rollback-test".to_string();
        next.chat_generation.max_output_tokens = 128;
        next.chat_generation.temperature = None;
        next.embedding_model = "embed-rollback-test".to_string();
        next.vision_model = "vision-rollback-test".to_string();
        let updates = BTreeMap::from([
            (
                DEFAULT_ENDPOINT_ID.into(),
                ApiKeyUpdate::Set("default-rollback-secret".into()),
            ),
            (
                "embed".into(),
                ApiKeyUpdate::Set("embed-rollback-secret".into()),
            ),
            (
                "vision".into(),
                ApiKeyUpdate::Set("vision-rollback-secret".into()),
            ),
        ]);
        let result = block_on_without_tokio(services.configure_providers(next.clone(), updates));
        assert!(result.is_err());
        assert_eq!(services.provider_settings().unwrap(), previous);
        let persisted = load_provider_settings(services.database_path()).unwrap();
        assert_eq!(persisted, previous);
        assert_eq!(db::settings::list(&conn).unwrap(), previous_rows);
        let restored_snapshot = services.ai_request_snapshot().unwrap();
        assert!(Arc::ptr_eq(
            &restored_snapshot.provider,
            &previous_snapshot.provider
        ));
        assert!(Arc::ptr_eq(
            &restored_snapshot.search,
            &previous_snapshot.search
        ));
        for endpoint in next.endpoints() {
            let target = normalize_provider_base_url(&endpoint.base_url).unwrap();
            assert!(credentials.api_key(target.as_str()).unwrap().is_none());
        }
        assert_eq!(
            db::index_jobs::get(&conn, &vision_id).unwrap().unwrap(),
            before_job
        );

        conn.execute_batch("DROP TRIGGER test_provider_reconfigure_failure")
            .unwrap();
    }

    #[test]
    fn web_search_stays_off_until_enabled_and_confirmed() {
        let mut settings = ProviderSettings::default();
        assert!(settings.web_search_config(None).unwrap().is_none());

        // A loopback endpoint needs no acknowledgement.
        settings.web_search_enabled = true;
        settings.web_search_url_template =
            "http://127.0.0.1:8080/search?q={query}&format=json".to_string();
        assert!(settings.validate().is_ok());

        // The key comes from the caller (credential store) and is not persisted.
        let config = settings
            .web_search_config(Some("credential-only".to_string()))
            .unwrap()
            .expect("loopback web search config");
        assert_eq!(config.api_key.as_deref(), Some("credential-only"));

        // A remote endpoint is rejected until it is explicitly confirmed.
        settings.web_search_url_template = "https://api.example.test/search?q={query}".to_string();
        assert!(settings.validate().is_err());

        settings.web_search_confirmed_remote_endpoint =
            normalize_web_endpoint(&settings.web_search_url_template).unwrap();
        settings.web_search_remote_confirmed = true;
        assert!(settings.validate().is_ok());

        // Changing the endpoint invalidates the acknowledgement again.
        settings.web_search_url_template =
            "https://other.example.test/search?q={query}".to_string();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn remote_confirmation_is_bound_to_the_exact_canonical_endpoint() {
        let mut settings = ProviderSettings {
            base_url: "https://models-a.example.test/v1".to_string(),
            chat_model: "chat-test".to_string(),
            embedding_model: "embed-test".to_string(),
            vision_model: "vision-test".to_string(),
            remote_content_confirmed: true,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: "https://models-a.example.test/v1/".to_string(),
            request_timeout_secs: 30,
            ..Default::default()
        };
        settings.validate().unwrap();

        settings.base_url = "https://models-b.example.test/v1".to_string();
        assert!(settings.validate().is_err());

        settings.confirmed_remote_endpoint = "https://models-b.example.test/v1/".to_string();
        settings.validate().unwrap();
    }

    #[test]
    fn persisted_provider_contract_does_not_accept_legacy_unbound_confirmation() {
        let legacy = serde_json::json!({
            "base_url": "https://legacy-remote.example.test/v1",
            "chat_model": "chat-test",
            "embedding_model": "embed-test",
            "vision_model": "vision-test",
            "remote_content_confirmed": true,
            "allow_insecure_remote_http": false,
            "request_timeout_secs": 30
        });
        assert!(serde_json::from_value::<ProviderSettings>(legacy).is_err());
    }

    #[test]
    fn background_job_snapshots_are_scoped_deduplicated_and_include_progress() {
        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let runtime = services.runtime();
        let book = runtime
            .block_on(async {
                services
                    .spawn_library(|library| library.create_book("Tasks", "Author"))
                    .await
                    .context("library test worker stopped")?
            })
            .unwrap();

        let jobs = runtime
            .block_on(services.background_jobs_for_books(vec![book.id.clone(), book.id.clone()]))
            .unwrap();
        assert_eq!(jobs.len(), 3);
        assert_eq!(
            jobs.iter().map(|job| job.kind.as_str()).collect::<Vec<_>>(),
            vec!["visual_render", "vision", "embedding"]
        );
        let visual = jobs.iter().find(|job| job.kind == "visual_render").unwrap();
        assert_eq!(visual.progress.total, None);
        assert!(
            runtime
                .block_on(services.background_jobs_for_books(Vec::new()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn background_job_snapshots_use_actual_rendered_pages_instead_of_content_unit_count() {
        use db::index_jobs::{IndexJob, IndexJobStatus};

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for status in [
            IndexJobStatus::Queued,
            IndexJobStatus::Running,
            IndexJobStatus::Paused,
            IndexJobStatus::Failed,
            IndexJobStatus::Cancelled,
            IndexJobStatus::Succeeded,
        ] {
            let job = IndexJob {
                id: "visual-render:three-chapters".into(),
                book_id: "book".into(),
                source_id: Some("source".into()),
                kind: "visual_render".into(),
                status,
                pause_requested: false,
                cancel_requested: false,
                attempts: 1,
                cursor_json: serde_json::json!({
                    "completed_pages": 7,
                    "spec": { "unit_ids": ["chapter-1", "chapter-2", "chapter-3"] }
                })
                .to_string(),
                error: None,
                created_at: 1,
                updated_at: 2,
                started_at: Some(1),
                finished_at: (status == IndexJobStatus::Succeeded).then_some(2),
            };
            let snapshot = background_job_snapshot(&conn, job).unwrap();
            assert_eq!(snapshot.progress.completed, 7, "{status:?}");
            assert_eq!(
                snapshot.progress.total,
                (status == IndexJobStatus::Succeeded).then_some(7),
                "{status:?} must not treat three chapters as three pages"
            );
        }
    }

    #[test]
    fn configuring_a_default_language_enqueues_translation_for_existing_books() {
        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let runtime = services.runtime();
        let book = runtime
            .block_on(async {
                services
                    .spawn_library(|library| library.create_book("Translated", "Author"))
                    .await
                    .context("library test worker stopped")?
            })
            .unwrap();

        // Translation is opt-in: nothing is queued while it is disabled.
        let jobs = runtime
            .block_on(services.background_jobs_for_books(vec![book.id.clone()]))
            .unwrap();
        assert!(jobs.iter().all(|job| job.kind != "translation"));

        let mut settings = services.provider_settings().unwrap();
        settings.default_language = Some("zh-Hans".to_string());
        runtime
            .block_on(services.configure_provider(settings, ApiKeyUpdate::Keep))
            .unwrap();

        let jobs = runtime
            .block_on(services.background_jobs_for_books(vec![book.id.clone()]))
            .unwrap();
        let translation = jobs
            .iter()
            .find(|job| job.kind == "translation")
            .expect("saving a default language must enqueue a translation job");
        assert_eq!(
            translation.status,
            BackgroundJobStatus::Paused,
            "auto-run is disabled by default, so the new task starts paused"
        );

        // The durable job survives a restart and is reconciled again on startup.
        drop(services);
        let reopened = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let jobs = reopened
            .runtime()
            .block_on(reopened.background_jobs_for_books(vec![book.id.clone()]))
            .unwrap();
        assert!(
            jobs.iter().any(|job| job.kind == "translation"),
            "startup reconciliation keeps one translation job per book and language"
        );
    }

    #[test]
    fn structural_visual_jobs_use_the_shared_library_and_object_store() {
        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        // This test exercises the durable visual queue, so new derivative jobs
        // must be created queued instead of using the product default.
        services
            .auto_run_background_jobs
            .store(true, Ordering::Release);
        // This deliberately bypasses AppServices::spawn_library, matching the
        // existing UI clones. The durable visual queue must still be noticed.
        let (book, document) = {
            let mut library = services.library_snapshot().unwrap();
            let book = library.create_book("Visual", "Author").unwrap();
            let document = library.document(&book.id).unwrap();
            (book, document)
        };
        let source = db::book_sources::get_revision(
            &db::open_conn(services.database_path()).unwrap(),
            &book.id,
            book.revision,
        )
        .unwrap()
        .unwrap();
        let visual_job_id = format!("visual-render:{}", source.id);
        let coordinator = services.visual_jobs().unwrap();
        let completed = services.runtime().block_on(coordinator.wait_for_state(
            &visual_job_id,
            VisualJobState::Succeeded,
            std::time::Duration::from_secs(5),
        ));
        assert_eq!(completed.unwrap().completed_pages, document.units.len());
        let pages = db::visual_pages::list_for_source(
            &db::open_conn(services.database_path()).unwrap(),
            &source.id,
        )
        .unwrap();
        assert_eq!(pages.len(), document.units.len());
        assert!(pages.iter().all(|page| {
            page.renderer == "moye-structural-png"
                && page.document_revision == book.revision
                && page.unit_revision == book.revision
                && page.profile_id.starts_with("render-profile-")
                && page.fidelity == "structural"
        }));
        for page in &pages {
            let blob = db::blobs::get(
                &db::open_conn(services.database_path()).unwrap(),
                &page.object_key,
            )
            .unwrap()
            .expect("persisted structural page blob");
            assert_eq!(blob.media_type, "image/png");
            let key = BlobKey::parse(&page.object_key).unwrap();
            let bytes = services
                .runtime()
                .block_on(services.blob_store().get(&key))
                .unwrap();
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
    }

    #[test]
    fn startup_requeues_legacy_renderer_and_reclaims_its_visual_object() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
        let book = library.create_book("Legacy visual", "Author").unwrap();
        let document = library.document(&book.id).unwrap();
        let unit = document.units.first().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let source = db::book_sources::get_revision(
            &db::open_conn(&db_path).unwrap(),
            &book.id,
            book.revision,
        )
        .unwrap()
        .unwrap();
        let runtime = library.io_runtime();
        let store = LocalBlobStore::new(temp.path().join(OBJECT_DIRECTORY)).unwrap();
        let legacy_bytes = b"<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'/>";
        let legacy_key = runtime.block_on(store.put(legacy_bytes)).unwrap();
        let legacy_blob = db::blobs::BlobRecord {
            object_key: legacy_key.to_string(),
            media_type: "image/svg+xml".to_string(),
            byte_len: legacy_bytes.len() as u64,
            hash: blake3::hash(legacy_bytes).to_hex().to_string(),
            created_at: unix_timestamp().unwrap(),
        };
        let legacy_page = db::visual_pages::VisualPage {
            id: "legacy-svg-page".to_string(),
            book_id: book.id.clone(),
            source_id: source.id.clone(),
            content_unit_id: Some(unit.id.clone()),
            page_index: 0,
            object_key: legacy_key.to_string(),
            width: 8,
            height: 8,
            render_scale: 1.0,
            renderer: "moye-structural-svg".to_string(),
            renderer_version: "0.0.1".to_string(),
            document_revision: book.revision,
            unit_revision: unit.revision.get(),
            profile_id: crate::preview::RenderProfile::default().stable_id(),
            fidelity: "structural".to_string(),
            locator_json: serde_json::to_string(&crate::document::DocumentLocator::unit(
                &book.id, &unit.id,
            ))
            .unwrap(),
            created_at: unix_timestamp().unwrap(),
        };
        db::transactions::replace_visual_pages(
            &mut db::open_conn(&db_path).unwrap(),
            &book.id,
            &source.id,
            book.revision,
            &[legacy_blob],
            &[legacy_page],
        )
        .unwrap();
        let job_id = format!("visual-render:{}", source.id);
        let legacy_spec = crate::preview::VisualJobSpec {
            id: job_id.clone(),
            book_id: book.id.clone(),
            source_id: source.id.clone(),
            document_revision: crate::document::Revision::new(book.revision),
            renderer: "moye-structural-svg".to_string(),
            renderer_version: "0.0.1".to_string(),
            fidelity: crate::preview::RenderFidelity::Structural,
            unit_ids: document.units.iter().map(|unit| unit.id.clone()).collect(),
            profile: crate::preview::RenderProfile::default(),
        };
        db::index_jobs::update_state(
            &db::open_conn(&db_path).unwrap(),
            &job_id,
            db::index_jobs::IndexJobStatus::Succeeded,
            &crate::preview::encode_persisted_visual_job(legacy_spec, 1).unwrap(),
            None,
            unix_timestamp().unwrap(),
            Some(unix_timestamp().unwrap()),
            Some(unix_timestamp().unwrap()),
        )
        .unwrap();
        drop(library);

        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        assert!(!runtime.block_on(store.exists(&legacy_key)).unwrap());
        assert!(
            db::blobs::get(&db::open_conn(&db_path).unwrap(), legacy_key.as_str())
                .unwrap()
                .is_none()
        );
        let completed = runtime
            .block_on(services.visual_jobs().unwrap().wait_for_state(
                &job_id,
                VisualJobState::Succeeded,
                std::time::Duration::from_secs(5),
            ))
            .unwrap();
        assert_eq!(completed.spec.renderer, "moye-structural-png");
        let pages =
            db::visual_pages::list_for_source(&db::open_conn(&db_path).unwrap(), &source.id)
                .unwrap();
        assert!(!pages.is_empty());
        assert!(
            pages
                .iter()
                .all(|page| page.renderer == "moye-structural-png")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn imported_pdf_visual_job_persists_native_png_pages() {
        let temp = tempfile::tempdir().unwrap();
        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let source_path = temp.path().join("native-visual.pdf");
        fs::write(&source_path, native_pdf_fixture()).unwrap();
        // The native renderer only runs on a queued job, not on the default
        // paused one.
        services
            .auto_run_background_jobs
            .store(true, Ordering::Release);
        let mut library = services.library_snapshot().unwrap();
        let book = match library.import(&source_path).unwrap() {
            ImportOutcome::Added(book) => book,
            ImportOutcome::AlreadyExists(_) => panic!("new PDF unexpectedly deduplicated"),
        };
        let source = db::book_sources::get_revision(
            &db::open_conn(services.database_path()).unwrap(),
            &book.id,
            book.revision,
        )
        .unwrap()
        .unwrap();
        let job_id = format!("visual-render:{}", source.id);
        let completed = services
            .runtime()
            .block_on(services.visual_jobs().unwrap().wait_for_state(
                &job_id,
                VisualJobState::Succeeded,
                std::time::Duration::from_secs(10),
            ))
            .unwrap();
        assert_eq!(completed.completed_pages, 1);
        let pages = db::visual_pages::list_for_source(
            &db::open_conn(services.database_path()).unwrap(),
            &source.id,
        )
        .unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].renderer, "moye-windows-pdf-png");
        let key = BlobKey::parse(&pages[0].object_key).unwrap();
        let png = services
            .runtime()
            .block_on(services.blob_store().get(&key))
            .unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

        let unit_id = library.document(&book.id).unwrap().units[0].id.clone();
        let updated = library
            .update_content_unit_source(
                &book.id,
                &unit_id,
                "<h1>Normalized</h1><p>Edited PDF content</p>",
            )
            .unwrap();
        let normalized_source = db::book_sources::get_revision(
            &db::open_conn(services.database_path()).unwrap(),
            &book.id,
            updated.revision,
        )
        .unwrap()
        .unwrap();
        assert_eq!(normalized_source.source_kind, "normalized");
        assert_eq!(normalized_source.format, "epub");
        let normalized_job_id = format!("visual-render:{}", normalized_source.id);
        services
            .runtime()
            .block_on(services.visual_jobs().unwrap().wait_for_state(
                &normalized_job_id,
                VisualJobState::Succeeded,
                std::time::Duration::from_secs(10),
            ))
            .unwrap();
        let normalized_pages = db::visual_pages::list_for_source(
            &db::open_conn(services.database_path()).unwrap(),
            &normalized_source.id,
        )
        .unwrap();
        assert!(!normalized_pages.is_empty());
        assert!(
            normalized_pages
                .iter()
                .all(|page| page.renderer == "moye-structural-png")
        );
    }

    #[test]
    fn structural_visual_queue_recovers_a_running_job_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
        let book = library.create_book("Restart visual", "Author").unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let source = db::book_sources::get_revision(
            &db::open_conn(&db_path).unwrap(),
            &book.id,
            book.revision,
        )
        .unwrap()
        .unwrap();
        let job_id = format!("visual-render:{}", source.id);
        let conn = db::open_conn(&db_path).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Running,
            &job.cursor_json,
            None,
            unix_timestamp().unwrap(),
            Some(unix_timestamp().unwrap()),
            None,
        )
        .unwrap();
        drop(conn);
        drop(library);

        let services = AppServices::open_with_credentials(
            temp.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        let completed =
            services
                .runtime()
                .block_on(services.visual_jobs().unwrap().wait_for_state(
                    &job_id,
                    VisualJobState::Succeeded,
                    std::time::Duration::from_secs(5),
                ));
        assert!(completed.unwrap().attempts >= 2);
    }

    #[test]
    fn replacing_visual_pages_reclaims_only_superseded_objects() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
        let book = library.create_book("Visual replacement", "Author").unwrap();
        let document = library.document(&book.id).unwrap();
        let unit = document.units.first().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let source = db::book_sources::get_revision(
            &db::open_conn(&db_path).unwrap(),
            &book.id,
            book.revision,
        )
        .unwrap()
        .unwrap();
        drop(library);

        let runtime = IoRuntime::default();
        let store = LocalBlobStore::new(temp.path().join(OBJECT_DIRECTORY)).unwrap();
        let old_bytes = b"old visual page";
        let reused_bytes = b"reused visual page";
        let shared_bytes = b"shared visual page";
        let replacement_bytes = b"replacement visual page";
        let old_key = runtime.block_on(store.put(old_bytes)).unwrap();
        let reused_key = runtime.block_on(store.put(reused_bytes)).unwrap();
        let shared_key = runtime.block_on(store.put(shared_bytes)).unwrap();
        let replacement_key = runtime.block_on(store.put(replacement_bytes)).unwrap();
        let now = unix_timestamp().unwrap();
        let blob = |key: &BlobKey, bytes: &[u8]| db::blobs::BlobRecord {
            object_key: key.to_string(),
            media_type: "image/png".to_string(),
            byte_len: bytes.len() as u64,
            hash: blake3::hash(bytes).to_hex().to_string(),
            created_at: now,
        };
        let old_blob = blob(&old_key, old_bytes);
        let reused_blob = blob(&reused_key, reused_bytes);
        let shared_blob = blob(&shared_key, shared_bytes);
        let replacement_blob = blob(&replacement_key, replacement_bytes);
        let page = |id: &str, index: usize, key: &BlobKey| db::visual_pages::VisualPage {
            id: id.to_string(),
            book_id: book.id.clone(),
            source_id: source.id.clone(),
            content_unit_id: Some(unit.id.clone()),
            page_index: index,
            object_key: key.to_string(),
            width: 800,
            height: 1_200,
            render_scale: 1.0,
            renderer: "test-png".to_string(),
            renderer_version: "1".to_string(),
            document_revision: book.revision,
            unit_revision: unit.revision.get(),
            profile_id: "test-profile".to_string(),
            fidelity: "structural".to_string(),
            locator_json: serde_json::to_string(&crate::document::DocumentLocator::unit(
                &book.id, &unit.id,
            ))
            .unwrap(),
            created_at: now,
        };

        let initially_unreferenced = db::transactions::replace_visual_pages(
            &mut db::open_conn(&db_path).unwrap(),
            &book.id,
            &source.id,
            book.revision,
            &[old_blob.clone(), reused_blob.clone(), shared_blob.clone()],
            &[
                page("old-page", 0, &old_key),
                page("reused-page-old", 1, &reused_key),
                page("shared-page-old", 2, &shared_key),
            ],
        )
        .unwrap();
        assert!(initially_unreferenced.is_empty());

        let superseded = db::transactions::replace_visual_pages(
            &mut db::open_conn(&db_path).unwrap(),
            &book.id,
            &source.id,
            book.revision,
            &[shared_blob.clone(), replacement_blob.clone()],
            &[
                page("shared-page-current", 0, &shared_key),
                page("replacement-page", 1, &replacement_key),
            ],
        )
        .unwrap();
        assert_eq!(superseded.len(), 2);
        assert!(superseded.contains(&old_blob));
        assert!(superseded.contains(&reused_blob));

        // Republish one of the stale candidates before its delayed collector
        // runs. A content-addressed key can be reused by another page without
        // writing different bytes, so the collector must consult current DB
        // references instead of trusting `superseded`.
        let no_longer_unreferenced = db::transactions::replace_visual_pages(
            &mut db::open_conn(&db_path).unwrap(),
            &book.id,
            &source.id,
            book.revision,
            &[
                shared_blob.clone(),
                replacement_blob.clone(),
                reused_blob.clone(),
            ],
            &[
                page("shared-page-current", 0, &shared_key),
                page("replacement-page", 1, &replacement_key),
                page("reused-page-current", 2, &reused_key),
            ],
        )
        .unwrap();
        assert!(no_longer_unreferenced.is_empty());

        let blob_publication = BlobPublicationLock::for_store(&store).unwrap();
        runtime.block_on(reclaim_unreferenced_blobs(
            &db_path,
            &store,
            &blob_publication,
            superseded,
            "视觉页面对象",
        ));

        assert!(!runtime.block_on(store.exists(&old_key)).unwrap());
        assert!(runtime.block_on(store.exists(&reused_key)).unwrap());
        assert!(runtime.block_on(store.exists(&shared_key)).unwrap());
        assert!(runtime.block_on(store.exists(&replacement_key)).unwrap());
        let conn = db::open_conn(&db_path).unwrap();
        assert!(db::blobs::get(&conn, old_key.as_str()).unwrap().is_none());
        assert!(
            db::blobs::get(&conn, reused_key.as_str())
                .unwrap()
                .is_some()
        );
        assert!(
            db::blobs::get(&conn, shared_key.as_str())
                .unwrap()
                .is_some()
        );
        assert!(
            db::blobs::get(&conn, replacement_key.as_str())
                .unwrap()
                .is_some()
        );
    }

    #[cfg(target_os = "windows")]
    fn native_pdf_fixture() -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 18.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal("Native PDF visual")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }
}
