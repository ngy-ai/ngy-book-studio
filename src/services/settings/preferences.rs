//! The per-surface persisted preference shapes. Each one lives under its own
//! settings key so the established provider JSON contract stays unchanged.

use super::*;

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

/// 朗读（系统语音）偏好的持久化形状。字段在 [`ProviderSettings`] 里展开成
/// `speech_voice_id` / `speech_rate` 两个基本类型，公开接口因此不暴露这个结构。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedSpeechSettings {
    #[serde(default)]
    pub(crate) voice_id: String,
    #[serde(default = "default_speech_rate")]
    pub(crate) rate: f32,
}

impl Default for PersistedSpeechSettings {
    fn default() -> Self {
        Self {
            voice_id: String::new(),
            rate: default_speech_rate(),
        }
    }
}

/// AI 问答的行为偏好：选中文本点「AI 解释」时自动填入的问题。
///
/// 与其它偏好一样存进自己的 settings key。`#[serde(default)]` 让没有这一行的老库
/// 直接落到默认问题，不需要迁移。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedQuestionSettings {
    #[serde(default = "default_selection_explanation_question")]
    pub(crate) selection_explanation_question: String,
}

impl Default for PersistedQuestionSettings {
    fn default() -> Self {
        Self {
            selection_explanation_question: default_selection_explanation_question(),
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
