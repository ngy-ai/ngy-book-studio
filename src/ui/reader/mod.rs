use super::*;
use super::{
    ai_controller::AiSidebarController,
    ai_sidebar::{
        AiCitationTextFocus, AiSidebarEvent, AiSourceLink, citation_dom_navigation_script,
    },
};
use anyhow::ensure;
use gpui::{DragMoveEvent, EmptyView, Pixels};
use ngy_book_studio::{
    ai::chat::ChatWindowKind,
    document::{BookDocument, DocumentLocator, SourceLocator},
    media::MediaService,
    reader::{
        AuthorizedReaderAsset, READER_FONT_SIZE_DEFAULT, READER_FONT_SIZE_MAX,
        READER_FONT_SIZE_MIN, ReaderResourceAuthorizations, ResourceResponse,
        load_resource_with_range, reader_appearance_script, reader_font_size,
    },
    services::{AppServices, ReaderZoomSurface, TranslationDisplayMode},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

mod annotations;
use annotations::{AnnotationAction, ReaderAnnotations};

mod translations;
use translations::{ManualTranslationRequest, ReaderTranslations};

#[cfg(target_os = "windows")]
pub(crate) mod selection_menu;

mod app;

mod citation;

mod close;

mod gate;

mod ipc;

mod navigation;

mod pane;

mod progress;

mod progress_io;

mod protocol;

mod reference;

mod render;

mod script;

mod text_size;

mod translation_display;

mod webview;

mod webview_events;

pub(crate) use citation::*;

pub(crate) use gate::*;

pub(crate) use ipc::*;

pub(crate) use pane::*;

pub(crate) use progress::*;

pub(crate) use protocol::*;

pub(crate) use reference::*;

pub(crate) use script::*;

pub(crate) use translation_display::*;

pub(crate) use webview::*;

#[cfg(test)]
mod tests;
pub(crate) const MAX_READER_SELECTION_BYTES: usize = 32 * 1024;
/// The chapter runtime reports the text size once the reader stops wheeling;
/// the window waits a little longer before writing it, so a burst of wheel
/// notches still leaves exactly one row.
pub(crate) const TEXT_SIZE_SAVE_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(400);
pub(crate) const READER_NAVIGATION_DEFAULT_WIDTH: f32 = 286.;
pub(crate) const READER_NAVIGATION_MIN_WIDTH: f32 = 200.;
pub(crate) const READER_NAVIGATION_MAX_WIDTH: f32 = 480.;
pub(crate) const READER_NAVIGATION_COLLAPSED_WIDTH: f32 = 44.;
pub(crate) const READER_CONTENT_MIN_WIDTH: f32 = 320.;
pub(crate) const READER_PANE_RESIZE_HANDLE_WIDTH: f32 = 6.;

/// The root view of a reader window. Each book opens in its own window so
/// several books can be read side by side; the library window stays on screen.
pub struct ReaderApp {
    pub(crate) book_id: String,
    pub(crate) book_incarnation: u64,
    pub(crate) book: OpenedBook,
    pub(crate) current_spine: usize,
    pub(crate) progress_locators: Vec<DocumentLocator>,
    pub(crate) selected_toc: Option<usize>,
    pub(crate) webview: Option<Entity<WebView>>,
    pub(crate) library: LibraryStore,
    pub(crate) services: Arc<AppServices>,
    pub(crate) library_view: Entity<EpubReaderApp>,
    pub(crate) ai_sidebar: Entity<AiSidebar>,
    pub(crate) ai_controller: AiSidebarController,
    pub(crate) pane_layout: ReaderPaneLayout,
    pub(crate) ai_sidebar_collapsed: bool,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) search_query: String,
    pub(crate) search_results: Vec<SearchHit>,
    pub(crate) selected_text: Option<String>,
    pub(crate) annotations: ReaderAnnotations,
    pub(crate) translations: ReaderTranslations,
    pub(crate) _annotation_ai_subscription: Subscription,
    pub(crate) _annotation_ai_submit_subscription: Subscription,
    pub(crate) _annotation_ai_failure_subscription: Subscription,
    pub(crate) citation_request_id: u64,
    pub(crate) pending_citation_navigation: Option<PendingReaderCitationNavigation>,
    pub(crate) _search_subscription: Subscription,
    pub(crate) _ai_subscription: Subscription,
    pub(crate) _ai_layout_subscription: Subscription,
    /// 皮肤变化时把新配色重放进章节文档。
    pub(crate) _theme_subscription: Subscription,
    pub(crate) webview_build_gate: ReaderWebViewBuildGate,
    pub(crate) page_sync_task: Option<Task<()>>,
    pub(crate) progress_writer: Option<ReadingProgressWriter>,
    pub(crate) progress_sync_task: Option<Task<()>>,
    pub(crate) protocol_gate: Option<ReaderProtocolGate>,
    pub(crate) closing_webview: Option<WeakEntity<WebView>>,
    pub(crate) closing: bool,
    /// Reading text size of this book's chapters, in pixels. It is applied to
    /// every chapter the window opens and remembered per book.
    pub(crate) text_size: u8,
    /// Pending write of the reading text size, replaced on every new value.
    pub(crate) text_size_save_task: Option<Task<()>>,
    /// URL of the page currently shown, so the translation display preference can
    /// be re-applied live when it changes in the AI settings.
    pub(crate) current_reader_url: Option<String>,
    /// Set when the window closes because its book left the library: the final
    /// reading position belongs to a document that no longer exists.
    pub(crate) closing_for_removed_book: bool,
    pub(crate) progress_close_ready: bool,
    pub(crate) removal_scheduled: bool,
    pub(crate) notice: Option<Notice>,
}
