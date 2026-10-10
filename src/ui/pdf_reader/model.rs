//! Paging limits and the shapes the reader receives from the host.

use super::*;

/// Bounded like the EPUB reader: a human thought may carry 64 KiB of Markdown
/// next to a 32 KiB quote, and every other PDF message is far smaller.
pub(crate) const MAX_PDF_IPC_BYTES: usize = 128 * 1024;
pub(crate) const MAX_PDF_SELECTION_BYTES: usize = 32 * 1024;
pub(crate) const PDF_PROTOCOL_CSP: &str = "default-src 'none'; script-src 'self'; worker-src 'self' blob:; \
    style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; font-src 'self' data:; \
    connect-src 'self'; media-src 'none'; object-src 'none'; frame-src 'none'; \
    child-src 'none'; base-uri 'none'; form-action 'none'";
/// The viewer reports the page scale once the reader stops wheeling; the window
/// waits a little longer before writing it, so a burst of wheel notches still
/// leaves exactly one row.
pub(crate) const PDF_ZOOM_SAVE_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PdfReaderPage {
    /// Canonical unit identity when this rendered page can be associated
    /// without guessing. Ephemeral Office PDFs deliberately leave this empty
    /// when pagination does not line up with the normalized document model.
    pub unit_id: Option<String>,
    pub unit_index: Option<usize>,
    /// Canonical unit revision used to bind notes to this exact page text.
    pub unit_revision: u64,
    pub title: String,
    pub page_number: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PdfTextSelection {
    pub(crate) request_id: u64,
    pub(crate) page_number: u32,
    pub(crate) text: String,
}

pub(crate) struct PdfReaderInit {
    pub book_id: String,
    pub book_incarnation: u64,
    /// Canonical document revision notes are bound to. It participates in every
    /// note's version check; whether a page can own notes at all is decided by
    /// the page's own canonical unit identity, never by this value.
    pub document_revision: u64,
    pub title: String,
    pub pages: Vec<PdfReaderPage>,
    pub initial_page: u32,
    pub library: LibraryStore,
    pub services: Arc<AppServices>,
    pub library_view: Entity<EpubReaderApp>,
    pub webview_building: bool,
    pub persist_progress: bool,
    pub preview_label: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum PdfIpcMessage {
    #[serde(rename = "ngy-pdf-viewer-ready")]
    ViewerReady,
    #[serde(rename = "ngy-pdf-ready")]
    Ready {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "pageCount")]
        page_count: u32,
    },
    #[serde(rename = "ngy-pdf-page-changed")]
    PageChanged {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "pageNumber")]
        page_number: u32,
        #[serde(rename = "pageCount")]
        page_count: u32,
    },
    #[serde(rename = "ngy-pdf-selection-changed")]
    SelectionChanged {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "pageNumber")]
        page_number: u32,
        #[serde(rename = "selectedText")]
        selected_text: String,
    },
    #[serde(rename = "ngy-pdf-error")]
    Error {
        #[serde(rename = "requestId")]
        request_id: u64,
        message: String,
    },
    /// Notes bridge action from the bundled viewer's private origin.
    #[serde(rename = "annotation_action")]
    AnnotationAction {
        #[serde(flatten)]
        action: PdfAnnotationAction,
    },
    /// Sent by the native WebView2 context menu, not by page script. The menu
    /// callback validated the private page/frame document before queueing it.
    #[serde(rename = "ngy-pdf-explain-selection")]
    ExplainSelection {
        #[serde(rename = "selectedText")]
        selected_text: String,
        #[serde(rename = "url")]
        url: String,
    },
    /// Sent by the bundled viewer when Ctrl + Arrow is pressed. The host owns
    /// page state, reading progress and note re-binding, so the shell only asks
    /// for a relative step instead of rendering a page on its own.
    #[serde(rename = "ngy-pdf-request-page")]
    RequestPage {
        /// -1 for the previous page, +1 for the next page.
        delta: i32,
    },
    /// Sent by the bundled viewer after Ctrl + wheel changed the page scale.
    /// The host owns the stored page size, so the shell only reports it.
    #[serde(rename = "ngy-pdf-zoom-changed")]
    ZoomChanged {
        #[serde(rename = "zoomMilli")]
        zoom_milli: u32,
    },
    /// 页面请求从某一段开始朗读：列表是已绘制页面文字层里的段落（id + 文字）。
    #[serde(rename = "read_aloud_start")]
    ReadAloudStart {
        start_block: String,
        blocks: Vec<ReadAloudPageBlock>,
    },
}
