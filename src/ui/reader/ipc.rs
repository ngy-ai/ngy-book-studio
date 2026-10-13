//! The messages the reader webview sends and the events it publishes.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ReaderIpcMessage {
    AnnotationAction {
        #[serde(flatten)]
        action: AnnotationAction,
    },
    ManualTranslation {
        #[serde(flatten)]
        request: ManualTranslationRequest,
    },
    RetranslateBlock {
        #[serde(flatten)]
        request: RetranslateBlockRequest,
    },
    SelectionChanged {
        selected_text: String,
    },
    /// Sent by the chapter runtime once scrolling stops: the chapter-internal
    /// reading position, in the same whitespace-free UTF-16 coordinates as note
    /// anchors.
    ReadingPosition {
        anchor: TextAnchor,
    },
    /// 页面请求从某一段开始朗读：列表是当前显示模式下的段落（id + 文字）。
    ReadAloudStart {
        start_block: String,
        blocks: Vec<ReadAloudPageBlock>,
    },
    /// Sent by the chapter runtime after Ctrl + wheel changed the reading text
    /// size. The host owns the stored size, so the runtime only reports it.
    ReaderTextSize {
        font_size: u32,
    },
    CitationNavigationResult {
        request_id: u64,
        found: bool,
        #[serde(default)]
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReaderWebEvent {
    AnnotationNavigationBlocked,
    AnnotationAction {
        url: String,
        action: AnnotationAction,
    },
    ManualTranslation {
        url: String,
        request: ManualTranslationRequest,
    },
    RetranslateBlock {
        url: String,
        request: RetranslateBlockRequest,
    },
    PageLoaded(String),
    ExplainSelection {
        url: String,
        selected_text: String,
    },
    SelectionChanged {
        url: String,
        selected_text: Option<String>,
    },
    ReadingPosition {
        url: String,
        anchor: TextAnchor,
    },
    ReadAloudStart {
        url: String,
        start_block: String,
        blocks: Vec<ReadAloudPageBlock>,
    },
    TextSize {
        url: String,
        font_size: u8,
    },
    CitationNavigationResult {
        url: String,
        request_id: u64,
        found: bool,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReflowableCitationTarget {
    pub(crate) spine_index: usize,
    pub(crate) href: String,
    pub(crate) focus: Option<AiCitationTextFocus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingReaderCitationNavigation {
    pub(crate) request_id: u64,
    pub(crate) spine_index: usize,
    pub(crate) focus: AiCitationTextFocus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderResizablePane {
    Navigation,
    Ai,
}
