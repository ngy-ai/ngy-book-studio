use super::*;

use gpui_kit::{ClipboardItem, DragMoveEvent, EmptyView, Pixels, ScrollHandle};

#[cfg(test)]
mod layout_tests;
use ngy_book_studio::ai::job_diagnostics::{
    BackgroundJobLogSnapshot, JobLogErrorKind, JobLogEvent,
};
// Only the debug-panel test names the request payload type directly; the
// production code reaches it through `TranslationBlockDetail::request`.
#[cfg(test)]
use ngy_book_studio::services::TranslationBlockRequest;
use ngy_book_studio::services::{
    AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
    TranslationBlockDetail, TranslationBlockInfo, TranslationBlockList, TranslationBlockProbe,
    translation_language_label,
};

mod action;

mod app;

mod blocks;

mod diagnostics;

mod filter;

mod helpers;

mod layout;

mod model;

mod presentation;

mod render;

mod render_blocks;

mod render_chrome;

mod render_detail;

mod render_list;

mod render_probe;

pub(crate) use blocks::*;

pub(crate) use diagnostics::*;

pub(crate) use filter::*;

pub(crate) use helpers::*;

pub(crate) use layout::*;

pub(crate) use model::*;

pub(crate) use presentation::*;

#[cfg(test)]
mod tests;
pub(crate) struct BackgroundJobsWindow {
    services: Arc<AppServices>,
    books: Vec<BackgroundJobBook>,
    scope_label: String,
    jobs: Vec<BackgroundJobSnapshot>,
    refresh_generation: u64,
    scope_generation: u64,
    loading: bool,
    loaded: bool,
    auto_refresh: bool,
    pending_job_id: Option<String>,
    selected_job_id: Option<String>,
    kind: &'static str,
    status: Option<BackgroundJobStatus>,
    search: Entity<InputState>,
    query: String,
    page: usize,
    /// Width of the task list column, draggable through the divider that
    /// separates the list from the detail panel.
    list_width: f32,
    resizing_list: bool,
    list_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    tab: DetailTab,
    logs: Option<BackgroundJobLogSnapshot>,
    logs_loading: bool,
    logs_generation: u64,
    logs_error: Option<String>,
    blocks: Option<TranslationBlockList>,
    /// Task identity the cached block list was loaded for. The list is pinned
    /// to the source revision, so one successful load per task is enough and
    /// polling never re-parses the book.
    blocks_job_id: Option<String>,
    blocks_loading: bool,
    blocks_generation: u64,
    blocks_error: Option<String>,
    blocks_state: Option<BlockState>,
    blocks_page: usize,
    /// Debug view of one text block, opened from the block list. `None` means no
    /// viewer is open.
    block_detail: Option<BlockDetailState>,
    notice: Option<JobsNotice>,
    refresh_error: Option<String>,
    _search_subscription: Subscription,
    _poll_task: Task<()>,
}

/// Drag payload of the list/detail divider. It carries no state; the window
/// itself tracks the live width that the handle reports.
#[derive(Clone, Copy)]
struct JobsListResizeDrag;
