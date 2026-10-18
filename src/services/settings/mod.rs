//! Everything the user can configure, and the persisted shapes it is read
//! from and written back as.
//!
//! - this module holds the persisted keys, limits and default values;
//! - [`endpoint`] holds one authorized connection and the role bindings;
//! - [`preferences`] holds the per-surface persisted preference shapes;
//! - [`provider`] holds [`ProviderSettings`], the combined view the UI and
//!   the runtime read.

use super::*;

mod endpoint;

mod preferences;

mod provider;

/// The public service surface (`services/mod.rs` re-exports these), so these
/// stay `pub` here exactly as they were declared in the flat file.
pub use endpoint::{EndpointRoutingSettings, EndpointSettings, ModelRole};

pub use preferences::{ReaderZoomSurface, TranslationDisplayMode};

pub use provider::{ApiKeyUpdate, ProviderSettings};

/// Crate-internal items keep their previous reachability through the
/// `services::*` globs.
pub(crate) use endpoint::{resolve_blocks_per_request, resolve_requests_per_minute};

pub(crate) use preferences::{
    PersistedAppearanceSettings, PersistedBackgroundJobSettings, PersistedBookTranslationDisplay,
    PersistedNetworkSettings, PersistedPdfReaderSettings, PersistedQuestionSettings,
    PersistedReaderZoom, PersistedSpeechSettings, PersistedTranslationSettings,
};

pub(crate) const OBJECT_DIRECTORY: &str = "objects";
pub(crate) const PROVIDER_SETTINGS_KEY: &str = "ai.openai_compatible.provider.v1";
pub(crate) const CHAT_GENERATION_SETTINGS_KEY: &str = "ai.openai_compatible.chat_generation.v1";
/// 翻译模型自己的生成参数。与问答参数分开：两者的默认值不同（翻译默认温度 0），
/// 老版本没有这一行，缺失时用翻译的默认值补齐。
pub(crate) const TRANSLATION_GENERATION_SETTINGS_KEY: &str =
    "ai.openai_compatible.translation_generation.v1";
pub(crate) const ENDPOINT_ROUTING_SETTINGS_KEY: &str = "ai.openai_compatible.endpoint_routing.v1";
pub(crate) const BACKGROUND_JOB_SETTINGS_KEY: &str = "background_jobs.preferences.v1";
pub(crate) const PDF_READER_SETTINGS_KEY: &str = "pdf.reader.preferences.v1";
pub(crate) const APPEARANCE_SETTINGS_KEY: &str = "appearance.preferences.v1";
pub(crate) const TRANSLATION_SETTINGS_KEY: &str = "translation.preferences.v1";
pub(crate) const NETWORK_SETTINGS_KEY: &str = "ai.network.preferences.v1";
/// 朗读（系统语音）的全局偏好：音色与语速。与上面的阅读偏好一样存进自己的
/// settings key；它是**应用级**设置（所有阅读窗口共用一份），因此不按书记忆，
/// 也就不需要任何删书清理。
pub(crate) const SPEECH_SETTINGS_KEY: &str = "speech.preferences.v1";
/// AI 问答的行为偏好：选中文本点「AI 解释」时自动填入的问题。与上面的偏好一样
/// 存进自己的 settings key，provider JSON 的既有契约不变。
pub(crate) const AI_QUESTION_SETTINGS_KEY: &str = "ai.question.preferences.v1";
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
/// 翻译任务自带的默认模型。老版本没有这个字段，`load_provider_settings` 会把缺失的
/// 翻译模型填成当时保存的问答模型，因此升级不会改变翻译执行身份。
pub const DEFAULT_TRANSLATION_MODEL: &str = "qwen3.5:0.8b";
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

pub(crate) fn default_embedding_dimensions() -> usize {
    DEFAULT_EMBEDDING_DIMENSIONS
}

pub(crate) fn default_translation_model() -> String {
    DEFAULT_TRANSLATION_MODEL.to_string()
}

pub(crate) fn default_endpoint_id() -> String {
    DEFAULT_ENDPOINT_ID.to_string()
}

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

/// New derivative jobs start paused until the user opts into automatic
/// execution in the AI settings window.
pub(crate) const DEFAULT_AUTO_RUN_BACKGROUND_JOBS: bool = false;

pub(crate) fn default_auto_run_background_jobs() -> bool {
    DEFAULT_AUTO_RUN_BACKGROUND_JOBS
}

/// Upper bound of background model jobs running at the same time. The indexing
/// worker spawns this many tasks once and parks the ones above the configured
/// concurrency, so raising or lowering the setting takes effect without a
/// restart. Parked slots only sleep on the idle poll interval, so the bound
/// sits well above any usable concurrency; the actual pressure still comes
/// from the concurrency the user configures.
pub const MAX_BACKGROUND_JOB_CONCURRENCY: usize = 64;
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

/// Reading preferences of the PDF reader. Kept in their own settings key so the
/// established Provider JSON contract stays unchanged, exactly like the
/// background-job preference above.
pub(crate) const DEFAULT_PDF_COMPACT_READING: bool = false;

pub(crate) fn default_pdf_compact_reading() -> bool {
    DEFAULT_PDF_COMPACT_READING
}

/// 朗读的正常语速。只有这一个默认值：改它等于改变所有人的默认朗读手感，
/// 不是一次普通重构。
pub(crate) fn default_speech_rate() -> f32 {
    1.0
}

/// 选中文本点「AI 解释」时自动填入的问题。用户可以在 AI 设置 → 系统里改；
/// 默认值明确点出「已锁定文本」，模型因此知道解释对象是宿主冻结的那段选区。
pub const DEFAULT_SELECTION_EXPLANATION_QUESTION: &str = "用汉语详细解释一下已锁定文本";

pub(crate) fn default_selection_explanation_question() -> String {
    DEFAULT_SELECTION_EXPLANATION_QUESTION.to_string()
}

/// 解释问题的字节上限。它随每次解释请求体发送，并且要留在侧栏的
/// `MAX_QUESTION_BYTES` 预算之内，所以这里给一个远小于 32 KB 的上限。
pub(crate) const MAX_SELECTION_EXPLANATION_QUESTION_BYTES: usize = 2 * 1024;

/// Whether AI and web-search requests use the system proxy. Kept in its own
/// settings key, exactly like the preferences above.
pub(crate) fn default_ai_use_proxy() -> bool {
    DEFAULT_AI_USE_PROXY
}

/// Target language for the reading-time book translation. Simplified Chinese is
/// the out-of-the-box choice shown by the AI settings window; `None` (the
/// window's "不翻译（仅原文）" entry) keeps the original text only.
pub(crate) const DEFAULT_TRANSLATION_LANGUAGE: Option<&str> = Some("zh-Hans");

pub(crate) fn default_translation_language() -> Option<String> {
    DEFAULT_TRANSLATION_LANGUAGE.map(str::to_string)
}

/// Reading-time translations hide the original text by default; the reader can
/// still toggle a paragraph back to bilingual. `Bilingual` restores the old
/// side-by-side view.
pub(crate) const DEFAULT_TRANSLATION_DISPLAY_MODE: TranslationDisplayMode =
    TranslationDisplayMode::TranslationOnly;

pub(crate) fn default_translation_display_mode() -> TranslationDisplayMode {
    DEFAULT_TRANSLATION_DISPLAY_MODE
}
