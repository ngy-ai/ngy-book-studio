use super::reader::{
    READER_NAVIGATION_COLLAPSED_WIDTH, READER_PANE_RESIZE_HANDLE_WIDTH, ReaderPaneLayout,
    ReaderResizablePane, ReadingProgressProjection, ReadingProgressWrite,
    ReadingProgressWriteEvent, ReadingProgressWriter,
};
use super::*;
use gpui_kit::{DragMoveEvent, EmptyView, Focusable};
use ngy_book_studio::document::DocumentLocator;
use ngy_book_studio::services::ReaderZoomSurface;

mod annotations;
use annotations::{PdfAnnotationAction, PdfAnnotations};

/// Bounded like the EPUB reader: a human thought may carry 64 KiB of Markdown
/// next to a 32 KiB quote, and every other PDF message is far smaller.
mod app;

mod close;

mod ipc;

mod model;

mod navigation_ui;

mod page;

mod pane;

mod progress_io;

mod reference;

mod render;

mod selection;

mod viewer;

mod webview;

mod webview_events;

pub(crate) use model::*;

pub(crate) use pane::*;

pub(crate) use reference::*;

pub(crate) use selection::*;

pub(crate) use viewer::*;

pub(crate) use webview::*;

#[cfg(test)]
mod tests;
/// A native reader window that renders the immutable imported PDF with the
/// checked-in PDF.js build. Search and AI use the canonical page units, while
/// the WebView never receives an object-store path or a network URL.
pub struct PdfReaderApp {
    pub(crate) book_id: String,
    pub(crate) book_incarnation: u64,
    pub(crate) title: String,
    pub(crate) pages: Vec<PdfReaderPage>,
    pub(crate) current_page: u32,
    pub(crate) actual_page_count: u32,
    pub(crate) page_request_id: u64,
    pub(crate) pdf_selection: Option<PdfTextSelection>,
    pub(crate) pdf_ready: bool,
    pub(crate) webview: Option<Entity<WebView>>,
    pub(crate) library: LibraryStore,
    pub(crate) services: Arc<AppServices>,
    pub(crate) library_view: Entity<EpubReaderApp>,
    pub(crate) ai_sidebar: Entity<AiSidebar>,
    pub(crate) ai_controller: Option<AiSidebarController>,
    pub(crate) pane_layout: ReaderPaneLayout,
    pub(crate) ai_sidebar_collapsed: bool,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) search_query: String,
    pub(crate) search_results: Vec<SearchHit>,
    pub(crate) page_list_scroll: gpui_kit::UniformListScrollHandle,
    pub(crate) page_list_synced_page: Option<u32>,
    pub(crate) annotations: PdfAnnotations,
    pub(crate) _annotation_ai_subscription: Subscription,
    pub(crate) _annotation_ai_submit_subscription: Subscription,
    pub(crate) _annotation_ai_failure_subscription: Subscription,
    pub(crate) _search_subscription: Subscription,
    pub(crate) _ai_subscription: Subscription,
    pub(crate) _ai_layout_subscription: Subscription,
    /// 皮肤变化时把新配色重新推给 viewer 文档。
    pub(crate) _theme_subscription: Subscription,
    pub(crate) webview_build_gate: ReaderWebViewBuildGate,
    pub(crate) ipc_sync_task: Option<Task<()>>,
    pub(crate) progress_writer: Option<ReadingProgressWriter>,
    pub(crate) progress_sync_task: Option<Task<()>>,
    pub(crate) closing_webview: Option<WeakEntity<WebView>>,
    pub(crate) closing: bool,
    /// Set when the window closes because its book left the library: the final
    /// reading position belongs to a document that no longer exists.
    pub(crate) closing_for_removed_book: bool,
    pub(crate) progress_close_ready: bool,
    pub(crate) removal_scheduled: bool,
    pub(crate) notice: Option<Notice>,
    pub(crate) persist_progress_enabled: bool,
    pub(crate) preview_label: String,
    /// Global PDF reading preference, applied to this window's viewer document.
    pub(crate) pdf_compact_reading: bool,
    /// Page scale of this window's viewer, in thousandths of its base scale.
    /// It is remembered per book and pushed to the viewer, which owns the
    /// layout: the host never measures a page.
    pub(crate) pdf_zoom_milli: u32,
    /// Pending write of this book's page scale, replaced on every new value.
    pub(crate) pdf_zoom_save_task: Option<Task<()>>,
}
