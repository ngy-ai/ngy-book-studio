//! Everything the user can configure, and the persisted shapes it is read
//! from and written back as.

use super::*;

pub(crate) const OBJECT_DIRECTORY: &str = "objects";
pub(crate) const PROVIDER_SETTINGS_KEY: &str = "ai.openai_compatible.provider.v1";
pub(crate) const CHAT_GENERATION_SETTINGS_KEY: &str = "ai.openai_compatible.chat_generation.v1";
pub(crate) const ENDPOINT_ROUTING_SETTINGS_KEY: &str = "ai.openai_compatible.endpoint_routing.v1";
pub(crate) const BACKGROUND_JOB_SETTINGS_KEY: &str = "background_jobs.preferences.v1";
pub(crate) const PDF_READER_SETTINGS_KEY: &str = "pdf.reader.preferences.v1";
pub(crate) const APPEARANCE_SETTINGS_KEY: &str = "appearance.preferences.v1";
pub(crate) const TRANSLATION_SETTINGS_KEY: &str = "translation.preferences.v1";
pub(crate) const NETWORK_SETTINGS_KEY: &str = "ai.network.preferences.v1";
pub(crate) const WEB_SEARCH_CREDENTIAL_TARGET: &str = "ai.openai_compatible.web_search.v1";
pub(crate) const MAX_MODEL_NAME_CHARS: usize = 256;
/// 等待 `ai_configuration` 门闩的上限。
///
/// 这把门闩只用来把设置保存串行成一次「事务」：拿不到就说明另一次保存还没结束。
/// 另一次保存可能正被后台索引任务堵在自己的门闩上（现场 2026-09-17：设置窗口停在
/// 「正在安全保存 Provider 设置…」，而 `settings` 表根本没被改写），所以这里必须
/// 有上限：无限等待会让每一次后续保存也永远没有结果，界面上再点「保存设置」只会
/// 叠加等待。超时发生在进入写入之前，本次没有提交任何东西，重试是安全的。
pub(crate) const AI_SETTINGS_GATE_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bounds for one published visual page set (Office enhanced preview or
/// DjVu scan) loaded into a reading window.
pub(crate) const MAX_LOADED_VISUAL_PAGES: usize = 20_000;
pub(crate) const MAX_LOADED_VISUAL_PAGE_BYTES: u64 = 512 * 1024 * 1024;

pub const DEFAULT_CHAT_MODEL: &str = "qwen3.5:0.8b";
pub const DEFAULT_EMBEDDING_MODEL: &str = "qwen3-embedding:0.6b";
pub const DEFAULT_VISION_MODEL: &str = "qwen3.5:0.8b";
/// Upper bounds for one manually edited translation block. The reading window
/// submits the reader's own text for a single block, so these stay far above
/// any real edit while still bounding the WebView message and the stored row.
pub(crate) const MAX_MANUAL_TRANSLATION_SEGMENTS: usize = 512;
pub(crate) const MAX_MANUAL_TRANSLATION_SEGMENT_BYTES: usize = 8 * 1024;
pub(crate) const MAX_MANUAL_TRANSLATION_BYTES: usize = 64 * 1024;

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

pub(crate) fn default_auto_run_background_jobs() -> bool {
    DEFAULT_AUTO_RUN_BACKGROUND_JOBS
}

/// Upper bound of background model jobs running at the same time. The indexing
/// worker spawns this many tasks once and parks the ones above the configured
/// concurrency, so raising or lowering the setting takes effect without a
/// restart. Parked slots only sleep on the idle poll interval, so the bound is
/// deliberately generous; the actual pressure still comes from the concurrency
/// the user configures.
pub const MAX_BACKGROUND_JOB_CONCURRENCY: usize = 1024;
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

pub(crate) fn default_background_job_concurrency() -> usize {
    DEFAULT_BACKGROUND_JOB_CONCURRENCY
}

pub(crate) fn default_background_job_interval_ms() -> u64 {
    DEFAULT_BACKGROUND_JOB_INTERVAL_MS
}

pub(crate) fn default_embedding_dimensions() -> usize {
    DEFAULT_EMBEDDING_DIMENSIONS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedBackgroundJobSettings {
    pub(crate) auto_run: bool,
    /// `#[serde(default)]` keeps rows written before the scheduling options
    /// existed loadable.
    #[serde(default = "default_background_job_concurrency")]
    pub(crate) concurrency: usize,
    #[serde(default = "default_background_job_interval_ms")]
    pub(crate) interval_ms: u64,
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

pub(crate) fn default_pdf_compact_reading() -> bool {
    DEFAULT_PDF_COMPACT_READING
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedPdfReaderSettings {
    pub(crate) compact_reading: bool,
}

impl Default for PersistedPdfReaderSettings {
    fn default() -> Self {
        Self {
            compact_reading: default_pdf_compact_reading(),
        }
    }
}

/// 界面皮肤偏好。与上面的阅读偏好一样存进自己的 settings key，provider JSON 的
/// 既有契约不变。
///
/// 皮肤是**应用级**偏好而不是窗口级：它决定所有窗口（含已打开的阅读器与编辑器）
/// 的颜色，所以只有一份，切换时广播。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedAppearanceSettings {
    #[serde(default)]
    pub(crate) theme: Appearance,
}

impl Default for PersistedAppearanceSettings {
    fn default() -> Self {
        Self {
            theme: Appearance::default(),
        }
    }
}

/// Whether AI and web-search requests use the system proxy. Kept in its own
/// settings key, exactly like the preferences above.
pub(crate) fn default_ai_use_proxy() -> bool {
    DEFAULT_AI_USE_PROXY
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedNetworkSettings {
    pub(crate) use_proxy: bool,
}

impl Default for PersistedNetworkSettings {
    fn default() -> Self {
        Self {
            use_proxy: default_ai_use_proxy(),
        }
    }
}

/// Target language for the reading-time book translation. Simplified Chinese is
/// the out-of-the-box choice shown by the AI settings window; `None` (the
/// window's "不翻译（仅原文）" entry) keeps the original text only.
pub(crate) const DEFAULT_TRANSLATION_LANGUAGE: Option<&str> = Some("zh-Hans");

pub(crate) fn default_translation_language() -> Option<String> {
    DEFAULT_TRANSLATION_LANGUAGE.map(str::to_string)
}

/// How reading-time translations are presented relative to the original text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationDisplayMode {
    /// Translation text on top, original text below a divider.
    Bilingual,
    /// The original text only. Stored translations stay valid and the background
    /// task keeps running; the reader simply never inserts the layer.
    OriginalOnly,
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

pub(crate) fn default_translation_display_mode() -> TranslationDisplayMode {
    DEFAULT_TRANSLATION_DISPLAY_MODE
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedTranslationSettings {
    #[serde(default)]
    pub(crate) default_language: Option<String>,
    #[serde(default)]
    pub(crate) display_mode: TranslationDisplayMode,
}

impl Default for PersistedTranslationSettings {
    fn default() -> Self {
        Self {
            default_language: default_translation_language(),
            display_mode: default_translation_display_mode(),
        }
    }
}

/// One book's own display choice, written by the reading window. It is kept
/// under its own settings key (see `db::settings::translation_display_book_key`)
/// so it never touches the global provider JSON, and the global preference
/// stays the fallback for every book that was never switched by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedBookTranslationDisplay {
    pub(crate) display_mode: TranslationDisplayMode,
}

/// One reading surface whose page size the reader can change by hand.
///
/// A book is read by exactly one surface per unit, but an Office book can be
/// read as reflowable chapters and as enhanced page images in different
/// windows, so the surfaces never share a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderZoomSurface {
    /// EPUB / MOBI / AZW / KFX / structured Office chapters: font pixels.
    Text,
    /// Continuous PDF pages: thousandths of the reader's base page scale.
    Pdf,
    /// Office enhanced preview and DjVu pages: thousandths of one page, where
    /// `0` is the reader's own "fit the window".
    Page,
}

impl ReaderZoomSurface {
    pub(crate) fn key(self, book_id: &str) -> String {
        db::settings::reader_zoom_book_key(self.name(), book_id)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Pdf => "pdf",
            Self::Page => "page",
        }
    }
}

/// One book's own page size for one reading surface. It lives under its own
/// settings key (see `db::settings::reader_zoom_book_key`) so a window that
/// never changed its page size keeps the reader's default, and so deleting the
/// book takes the row with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedReaderZoom {
    pub(crate) value: u32,
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
    /// 界面皮肤（浅色 / 深色）。存进自己的 settings key，与上面的偏好同理；
    /// 它是应用级设置，切换时所有窗口与 WebView 一起跟随。
    #[serde(skip, default)]
    pub appearance: Appearance,
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
    /// Whether AI and web-search requests honour the system proxy. Stored under
    /// its own settings key, like the preferences above.
    #[serde(skip, default = "default_ai_use_proxy")]
    pub use_proxy: bool,
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

pub(crate) fn default_web_search_method() -> String {
    "GET".to_string()
}

pub(crate) fn default_web_search_timeout_secs() -> u64 {
    crate::ai::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS
}

pub(crate) fn default_web_search_max_results() -> usize {
    crate::ai::web_search::MAX_WEB_SEARCH_RESULTS
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
            appearance: Appearance::default(),
            default_language: default_translation_language(),
            translation_display_mode: default_translation_display_mode(),
            use_proxy: default_ai_use_proxy(),
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
            web_search_timeout_secs: crate::ai::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS,
            web_search_max_results: crate::ai::web_search::MAX_WEB_SEARCH_RESULTS,
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
