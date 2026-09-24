use super::*;
use anyhow::ensure;
use std::collections::HashMap;

use ngy_book_studio::{
    ai::agent::PassageRecord,
    ai::search::{SearchMode, SearchRequest},
    annotations::Annotation,
    document::{BookDocument, BookFormat, BookSource as CanonicalBookSource, DocumentLocator},
    office_com::OfficeCancellation,
    preview::VisualJobState,
    services::{PublishedVisualPage, VisualPageSourceKind},
};

mod annotations;

mod app;

mod book_author;

mod book_menu;

mod book_ops;

mod book_rename;

mod editor_open;

mod export;

mod groups;

mod import;

mod office;

mod office_preview;

mod open;

mod pdf_window;

mod render;

mod render_books;

mod render_library;

mod render_list;

mod render_picker;

mod render_search;

mod render_sidebar;

mod request;

mod search;

mod share_ops;

mod state;

pub(crate) use annotations::*;

pub(crate) use export::*;

pub(crate) use office::*;

pub(crate) use pdf_window::*;

pub(crate) use render::*;

pub(crate) use search::*;

pub(crate) use share_ops::*;

pub(crate) use state::*;

// The library surface the rest of the UI uses, unchanged by the split.
pub use render::wrap_root;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_test;

const LIBRARY_SEARCH_LIMIT: usize = 200;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
const OFFICE_PREVIEW_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
const OFFICE_PREVIEW_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct EpubReaderApp {
    library: LibraryStore,
    services: Arc<AppServices>,
    library_window: gpui::AnyWindowHandle,
    library_mutations: LibraryMutationLifecycle,
    ai_sidebar: Entity<AiSidebar>,
    ai_controller: AiSidebarController,
    search_input: Entity<InputState>,
    search_query: String,
    search_mode: SearchMode,
    search_results: Vec<SearchHit>,
    search_generation: u64,
    search_results_generation: Option<u64>,
    search_loading: bool,
    search_error: Option<String>,
    _search_subscription: Subscription,
    _ai_subscription: Subscription,
    notice: Option<Notice>,
    is_importing: bool,
    library_projection_generation: u64,
    library_request_generation: u64,
    library_notice_request: Option<u64>,
    group_filter_generation: u64,
    office_preview_generation: u64,
    office_preview: Option<OfficePreviewRun>,
    office_enabled_books: HashSet<String>,
    office_settings_generation: u64,
    _office_state_task: Task<()>,
    group_filter: GroupFilter,
    collapsed_groups: HashSet<String>,
    book_sort: BookSortField,
    book_sort_direction: BookSortDirection,
    book_view: BookViewMode,
    group_modal: Option<GroupModal>,
}
