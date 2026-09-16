use super::*;

use gpui::{ClipboardItem, DragMoveEvent, EmptyView, Pixels, ScrollHandle};

#[cfg(test)]
mod layout_tests;
use ngy_book_studio::job_diagnostics::{BackgroundJobLogSnapshot, JobLogErrorKind, JobLogEvent};
// Only the debug-panel test names the request payload type directly; the
// production code reaches it through `TranslationBlockDetail::request`.
#[cfg(test)]
use ngy_book_studio::services::TranslationBlockRequest;
use ngy_book_studio::services::{
    AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
    TranslationBlockDetail, TranslationBlockInfo, TranslationBlockList, TranslationBlockProbe,
    translation_language_label,
};

const JOBS_PER_PAGE: usize = 12;
/// One page of the translation block inspector. Blocks are rendered without
/// virtual scrolling, so the page bounds what one frame has to build.
const BLOCKS_PER_PAGE: usize = 50;
/// Width the task list column starts at; the divider between the list and the
/// detail panel drags it between the hard bounds below.
const JOBS_LIST_DEFAULT_WIDTH: f32 = 380.;
const JOBS_LIST_MIN_WIDTH: f32 = 260.;
const JOBS_LIST_MAX_WIDTH: f32 = 640.;
/// The detail panel never renders narrower than this, so dragging the divider
/// can never squeeze the main subject out of the window.
const JOBS_DETAIL_MIN_WIDTH: f32 = 360.;
const JOBS_RESIZE_HANDLE_WIDTH: f32 = 6.;
const JOB_KINDS: &[(&str, &str)] = &[
    ("", "全部任务"),
    ("translation", "图书翻译"),
    ("embedding", "向量索引"),
    ("vision", "视觉理解"),
    ("visual_render", "页面渲染"),
    ("other", "其他任务"),
];
const STATUS_FILTERS: &[(Option<BackgroundJobStatus>, &str)] = &[
    (None, "全部状态"),
    (Some(BackgroundJobStatus::Running), "运行中"),
    (Some(BackgroundJobStatus::Queued), "排队中"),
    (Some(BackgroundJobStatus::Paused), "已暂停"),
    (Some(BackgroundJobStatus::Failed), "失败"),
    (Some(BackgroundJobStatus::Succeeded), "已完成"),
    (Some(BackgroundJobStatus::Cancelled), "已取消"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BackgroundJobBook {
    pub id: String,
    pub title: String,
}

/// Which detail panel is open for the selected task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailTab {
    Summary,
    Logs,
    /// Per-block view of a whole-book translation task.
    Blocks,
}

/// State of one translatable block in the inspector. `Done`/`Processing`/
/// `Pending` come from the live durable cursor; `Failed` comes from the bounded
/// per-block diagnostics of the newest execution, because a skipped block
/// advances the cursor just like a cache hit and so cannot be named by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockState {
    Done,
    Processing,
    Failed,
    Pending,
}

impl BlockState {
    fn label(self) -> &'static str {
        match self {
            Self::Done => "已处理",
            Self::Processing => "处理中",
            Self::Failed => "失败",
            Self::Pending => "待处理",
        }
    }

    fn colors(self) -> (gpui::Rgba, gpui::Rgba) {
        match self {
            Self::Done => (rgb(0x376441), rgb(0xe3efe5)),
            Self::Processing => (rgb(ACCENT_DARK), rgb(ACCENT_SOFT)),
            Self::Failed => (rgb(DANGER), rgb(0xf8e2de)),
            Self::Pending => (rgb(0x5e6572), rgb(0xe7e9ed)),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Processing => "processing",
            Self::Failed => "failed",
            Self::Pending => "pending",
        }
    }
}

/// Blocks the newest execution of one translation task could not translate,
/// with the fixed category recorded for each of them. Diagnostics are the only
/// place that names *which* block failed: a skipped block advances the durable
/// cursor like a cache hit, so the counters alone can only say how far the task
/// got.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BlockFailures {
    /// Block ordinal -> fixed failure category, when one was recorded.
    reasons: BTreeMap<usize, Option<JobLogErrorKind>>,
    /// Per-block diagnostics of this task were pruned, so blocks that failed
    /// earlier in the same execution may be missing from `reasons`.
    truncated: bool,
}

impl BlockFailures {
    /// Reads the newest execution out of the task's persistent log. Lines older
    /// than the last `RunStarted` belong to an earlier attempt: a block that
    /// failed once and was translated by a retry is not a failure now.
    fn from_logs(logs: Option<&BackgroundJobLogSnapshot>) -> Self {
        let Some(logs) = logs else {
            return Self::default();
        };
        let mut reasons = BTreeMap::new();
        for entry in logs.entries.iter().rev() {
            if entry.event == JobLogEvent::RunStarted {
                break;
            }
            if !matches!(
                entry.event,
                JobLogEvent::ProtocolSkipped | JobLogEvent::StepFailed
            ) {
                continue;
            }
            let Some(ordinal) = entry.metrics.ordinal else {
                continue;
            };
            // A controlled stop is not a block failure: pausing or cancelling
            // aborts the in-flight request and would otherwise mark its block.
            if entry.metrics.error_kind == Some(JobLogErrorKind::Cancelled) {
                continue;
            }
            reasons
                .entry(ordinal as usize)
                .or_insert(entry.metrics.error_kind);
        }
        Self {
            reasons,
            truncated: logs.truncated,
        }
    }

    fn contains(&self, ordinal: usize) -> bool {
        self.reasons.contains_key(&ordinal)
    }

    fn reason(&self, ordinal: usize) -> Option<&str> {
        self.reasons
            .get(&ordinal)
            .and_then(|kind| *kind)
            .map(JobLogErrorKind::label)
    }

    fn failed(&self, total: usize) -> usize {
        self.reasons
            .keys()
            .filter(|ordinal| **ordinal < total)
            .count()
    }

    /// Blocks already accounted for by the cursor lose that state to `Failed`,
    /// so the counters keep adding up to the block count.
    fn before_cursor(&self, cursor: usize) -> usize {
        self.reasons
            .keys()
            .filter(|ordinal| **ordinal < cursor)
            .count()
    }
}

/// Live translation counters for one job. `cursor` mirrors the durable cursor:
/// the first block the task has not finished, and the first one a failed task
/// still has to translate. `processing` mirrors the worker's open window — whole
/// book translation keeps up to `任务并发` blocks in flight at once, so the cursor
/// alone can no longer say which block is being translated.
///
/// A block the newest execution recorded as failed leaves the state the cursor
/// would give it, so `done + processing + failed + pending` always equals
/// `total` and the header never disagrees with the list below it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TranslationProgress {
    total: usize,
    /// Raw durable cursor, kept for the anchor page.
    cursor: usize,
    done: usize,
    /// Ordinals the worker has sent and not committed yet, ascending. Empty for
    /// a task that is not walking a whole book, and for every terminal state.
    processing: Vec<usize>,
    failed: usize,
    pending: usize,
}

impl TranslationProgress {
    fn new(job: &BackgroundJobSnapshot, total: usize, failures: &BlockFailures) -> Self {
        let cursor = job.progress.completed.min(total);
        let failed = failures.failed(total);
        let failed_before_cursor = failures.before_cursor(cursor);
        // 窗口里的块不会再被算成失败：它对 `state()` 是 `Failed`（最新一次执行记过失败），
        // 对计数也是失败，否则表头加起来会超过块总数。
        let processing = in_flight_blocks(job, cursor, total)
            .into_iter()
            .filter(|ordinal| !failures.contains(*ordinal))
            .collect::<Vec<_>>();
        let failed_after_cursor = failed - failed_before_cursor;
        let pending = total - cursor - processing.len() - failed_after_cursor;
        Self {
            total,
            cursor,
            done: cursor - failed_before_cursor,
            processing,
            failed,
            pending,
        }
    }

    /// Whether one ordinal is inside the worker's open window.
    fn is_processing(&self, index: usize) -> bool {
        self.processing.binary_search(&index).is_ok()
    }

    fn count(&self, state: BlockState) -> usize {
        match state {
            BlockState::Done => self.done,
            BlockState::Processing => self.processing.len(),
            BlockState::Failed => self.failed,
            BlockState::Pending => self.pending,
        }
    }

    fn filtered(&self, filter: Option<BlockState>) -> usize {
        match filter {
            Some(state) => self.count(state),
            None => self.total,
        }
    }
}

/// Ordinals the running worker has in flight, read from the durable cursor.
///
/// 只有整本翻译会填这个集合（其他任务逐项走、窗口恒空），所以它同时也是「这本图书现在
/// 有几块在跑」的唯一权威读数。缺口一律按「没有在飞的块」处理：旧格式游标、写坏的 JSON、
/// 已经不在运行的任务都不该凭空显示成处理中。已提交的块（序号小于游标）永远不算在飞 ——
/// 提交意味着这一块已经写完，游标也不会越过还在等的块。
fn in_flight_blocks(job: &BackgroundJobSnapshot, cursor: usize, total: usize) -> Vec<usize> {
    if job.status != BackgroundJobStatus::Running {
        return Vec::new();
    }
    let Some(cursor_json) = job.cursor_json.as_deref() else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(cursor_json) else {
        return Vec::new();
    };
    let Some(ordinals) = parsed.get("inflight").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let mut blocks = ordinals
        .iter()
        .filter_map(|ordinal| ordinal.as_u64())
        .filter_map(|ordinal| usize::try_from(ordinal).ok())
        .filter(|ordinal| *ordinal >= cursor && *ordinal < total)
        .collect::<Vec<_>>();
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}

/// One inspected translation task: the live counters derived from the durable
/// cursor plus the blocks the newest execution could not translate, which only
/// the persistent diagnostics know about.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockInspector {
    progress: TranslationProgress,
    failures: BlockFailures,
}

impl BlockInspector {
    fn new(
        job: &BackgroundJobSnapshot,
        total: usize,
        logs: Option<&BackgroundJobLogSnapshot>,
    ) -> Self {
        let failures = BlockFailures::from_logs(logs);
        Self {
            progress: TranslationProgress::new(job, total, &failures),
            failures,
        }
    }

    fn state(&self, index: usize) -> BlockState {
        if self.failures.contains(index) {
            BlockState::Failed
        } else if index < self.progress.cursor {
            BlockState::Done
        } else if self.progress.is_processing(index) {
            BlockState::Processing
        } else {
            BlockState::Pending
        }
    }

    fn reason(&self, index: usize) -> Option<&str> {
        self.failures.reason(index)
    }
}

/// Which part of one block's debug view is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockDetailTab {
    /// Identifiers, sizes and request shape, with nothing that can be truncated.
    Overview,
    Source,
    /// The serialized chat-completions body — the payload actually sent.
    Body,
    Prompt,
    Message,
    /// The raw answer of one on-demand replay. Only offered for a failed block:
    /// a block that translated fine has no answer worth re-asking for.
    Response,
}

impl BlockDetailTab {
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "概览",
            Self::Source => "原文",
            Self::Body => "请求 Body",
            Self::Prompt => "System 提示词",
            Self::Message => "User 内容",
            Self::Response => "模型响应",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Source => "source",
            Self::Body => "body",
            Self::Prompt => "prompt",
            Self::Message => "message",
            Self::Response => "response",
        }
    }
}

/// Tabs of one block's debug view, in display order. The response tab exists
/// only for a failed block, so a healthy block keeps exactly the five request-side
/// tabs it always had.
fn block_detail_tabs(failed: bool) -> Vec<BlockDetailTab> {
    let tabs = [
        BlockDetailTab::Overview,
        BlockDetailTab::Body,
        BlockDetailTab::Source,
        BlockDetailTab::Prompt,
        BlockDetailTab::Message,
    ];
    if failed {
        return tabs.into_iter().chain([BlockDetailTab::Response]).collect();
    }
    tabs.to_vec()
}

/// One on-demand replay of the open block, cached while its viewer stays open.
/// Kept separate from `loading`/`detail`: a replay is a fresh model request the
/// user triggers on purpose, it can be repeated, and it must never be confused
/// with the pinned request payload that the other tabs describe.
#[derive(Clone, Debug, Default)]
struct BlockProbeState {
    /// A replay is in flight. The button is disabled while it is.
    loading: bool,
    result: Option<TranslationBlockProbe>,
    error: Option<String>,
}

/// The block whose debug view is open, and the payload once it is loaded. The
/// ordinal is kept separately so the panel can name the block while it is still
/// loading or after a failure.
#[derive(Clone, Debug)]
struct BlockDetailState {
    job_id: String,
    ordinal: usize,
    generation: u64,
    tab: BlockDetailTab,
    /// The block is one the newest execution could not translate, which is what
    /// unlocks the response tab.
    failed: bool,
    loading: bool,
    detail: Option<TranslationBlockDetail>,
    error: Option<String>,
    probe: BlockProbeState,
    scroll: ScrollHandle,
}

#[derive(Clone, Debug)]
struct JobsNotice {
    text: String,
    error: bool,
}

pub(super) struct BackgroundJobsWindow {
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

impl BackgroundJobsWindow {
    fn new(
        services: Arc<AppServices>,
        books: Vec<BackgroundJobBook>,
        scope_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索书名或任务 ID…"));
        let subscription = cx.subscribe_in(&search, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = this.search.read(cx).value().to_string();
                this.filters_changed(window, cx);
            }
        });
        let poll_task = cx.spawn_in(window, async move |view, cx| {
            loop {
                Timer::after(Duration::from_secs(2)).await;
                let alive = cx.update(|window, cx| {
                    view.update(cx, |this, cx| {
                        if this.auto_refresh && !this.loading && this.pending_job_id.is_none() {
                            this.refresh(window, cx);
                        }
                    })
                });
                if !matches!(alive, Ok(Ok(()))) {
                    break;
                }
            }
        });
        Self {
            services,
            books,
            scope_label,
            jobs: Vec::new(),
            refresh_generation: 0,
            scope_generation: 0,
            loading: false,
            loaded: false,
            auto_refresh: true,
            pending_job_id: None,
            selected_job_id: None,
            kind: "",
            status: None,
            search,
            query: String::new(),
            page: 0,
            list_width: JOBS_LIST_DEFAULT_WIDTH,
            resizing_list: false,
            list_scroll: ScrollHandle::default(),
            detail_scroll: ScrollHandle::default(),
            tab: DetailTab::Summary,
            logs: None,
            logs_loading: false,
            logs_generation: 0,
            logs_error: None,
            blocks: None,
            blocks_job_id: None,
            blocks_loading: false,
            blocks_generation: 0,
            blocks_error: None,
            blocks_state: None,
            blocks_page: 0,
            block_detail: None,
            notice: None,
            refresh_error: None,
            _search_subscription: subscription,
            _poll_task: poll_task,
        }
    }

    fn filtered_jobs(&self) -> Vec<&BackgroundJobSnapshot> {
        let query = self.query.trim().to_lowercase();
        self.jobs
            .iter()
            .filter(|job| job_matches(job, &self.books, self.kind, self.status, &query))
            .collect()
    }

    fn reconcile_selection(&mut self) {
        let jobs = self.filtered_jobs();
        let page_count = jobs.len().div_ceil(JOBS_PER_PAGE).max(1);
        let page = self.page.min(page_count - 1);
        let page_jobs = jobs.iter().skip(page * JOBS_PER_PAGE).take(JOBS_PER_PAGE);
        let ids: Vec<_> = page_jobs.map(|job| job.id.clone()).collect();
        let selected = self
            .selected_job_id
            .as_ref()
            .filter(|id| ids.contains(id))
            .cloned()
            .or_else(|| ids.first().cloned());
        self.page = page;
        if self.selected_job_id != selected {
            self.selected_job_id = selected;
            self.clear_detail_cache();
        }
    }

    /// Drops every cached per-task payload. Called whenever the inspected task
    /// changes or its identity may have been rewritten.
    fn clear_detail_cache(&mut self) {
        self.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.logs_generation = self.logs_generation.wrapping_add(1);
        self.logs_loading = false;
        self.logs = None;
        self.logs_error = None;
        self.clear_blocks();
    }

    fn clear_blocks(&mut self) {
        self.blocks_generation = self.blocks_generation.wrapping_add(1);
        self.blocks_loading = false;
        self.blocks = None;
        self.blocks_job_id = None;
        self.blocks_error = None;
        self.blocks_state = None;
        self.blocks_page = 0;
        // The block panel describes one block of the previously inspected task.
        self.block_detail = None;
    }

    /// Widest the list column may become while the detail panel still keeps its
    /// minimum body width inside the given viewport.
    fn maximum_list_width(&self, viewport_width: Pixels) -> f32 {
        let viewport = f32::from(viewport_width);
        if !viewport.is_finite() {
            return JOBS_LIST_MAX_WIDTH;
        }
        (viewport - JOBS_DETAIL_MIN_WIDTH - JOBS_RESIZE_HANDLE_WIDTH)
            .clamp(JOBS_LIST_MIN_WIDTH, JOBS_LIST_MAX_WIDTH)
    }

    /// Width the list column renders with. Clamping here (not only while
    /// dragging) keeps a shrinking window from squeezing the detail panel.
    fn effective_list_width(&self, viewport_width: Pixels) -> f32 {
        self.list_width
            .clamp(JOBS_LIST_MIN_WIDTH, self.maximum_list_width(viewport_width))
    }

    fn begin_list_resize(&mut self) -> bool {
        if self.resizing_list {
            return false;
        }
        self.resizing_list = true;
        true
    }

    fn finish_list_resize(&mut self) -> bool {
        std::mem::take(&mut self.resizing_list)
    }

    /// Follows the divider with the pointer. The pointer sits on the handle
    /// center, so half the handle belongs to the list column on its left.
    fn resize_list_from_pointer(&mut self, pointer_x: Pixels, viewport_width: Pixels) -> bool {
        if !self.resizing_list {
            return false;
        }
        let pointer_x = f32::from(pointer_x);
        if !pointer_x.is_finite() {
            return false;
        }
        let width = (pointer_x - JOBS_RESIZE_HANDLE_WIDTH / 2.)
            .clamp(JOBS_LIST_MIN_WIDTH, self.maximum_list_width(viewport_width));
        if self.list_width == width {
            return false;
        }
        self.list_width = width;
        true
    }

    fn filters_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = 0;
        self.list_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.reconcile_selection();
        self.refresh_logs(window, cx);
        self.refresh_blocks(window, cx);
        cx.notify();
    }

    fn apply_scope(
        &mut self,
        books: Vec<BackgroundJobBook>,
        scope_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        self.books = books;
        self.scope_label = scope_label;
        self.jobs.clear();
        self.selected_job_id = None;
        self.clear_detail_cache();
        self.page = 0;
        self.list_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.loaded = false;
        self.loading = false;
        self.notice = None;
        self.refresh_error = None;
        self.refresh(window, cx);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        let generation = self.refresh_generation;
        self.loading = true;
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services.background_jobs_for_books(book_ids).await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |this, cx| {
                    if generation != this.refresh_generation {
                        return;
                    }
                    this.loading = false;
                    match result {
                        Ok(mut jobs) => {
                            this.refresh_error = None;
                            // A stable order prevents polling from moving rows under the pointer.
                            jobs.sort_by(|a, b| {
                                b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id))
                            });
                            this.jobs = jobs;
                            this.loaded = true;
                            this.reconcile_selection();
                            this.refresh_logs(window, cx);
                            this.refresh_blocks(window, cx);
                        }
                        Err(error) => {
                            this.refresh_error = Some(format!("无法刷新后台任务：{error:#}"));
                        }
                    }
                    cx.notify();
                })
            });
        })
        .detach();
        cx.notify();
    }

    /// Loads the diagnostics of the selected task. The block inspector reads the
    /// same log to name the blocks that failed, so both tabs open it.
    fn refresh_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.tab, DetailTab::Logs | DetailTab::Blocks) || self.logs_loading {
            return;
        }
        let Some(job_id) = self.selected_job_id.clone() else {
            return;
        };
        self.logs_generation = self.logs_generation.wrapping_add(1);
        let generation = self.logs_generation;
        self.logs_loading = true;
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services.background_job_logs(job_id, book_ids).await;
            let _ = view.update(cx, |this, cx| {
                if generation != this.logs_generation {
                    return;
                }
                this.logs_loading = false;
                match result {
                    Ok(logs) => {
                        this.logs = Some(logs);
                        this.logs_error = None;
                    }
                    Err(error) => this.logs_error = Some(format!("无法读取运行日志：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Loads the block list of the selected translation task. The structural
    /// list is pinned to the source revision and never changes while the task
    /// runs, so one successful load per task is cached; only the live progress
    /// shown next to it keeps updating.
    fn refresh_blocks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab != DetailTab::Blocks || self.blocks_loading {
            return;
        }
        let Some(job_id) = self.selected_job_id.clone() else {
            return;
        };
        let Some(job) = self.jobs.iter().find(|job| job.id == job_id) else {
            return;
        };
        if job.kind != "translation" {
            return;
        }
        if self.blocks_job_id.as_deref() == Some(job_id.as_str()) {
            return;
        }
        self.blocks_job_id = Some(job_id.clone());
        self.blocks_generation = self.blocks_generation.wrapping_add(1);
        let generation = self.blocks_generation;
        self.blocks_loading = true;
        let detail_job_id = job_id.clone();
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services
                .background_job_translation_blocks(job_id, book_ids)
                .await;
            let _ = view.update(cx, |this, cx| {
                if generation != this.blocks_generation {
                    return;
                }
                this.blocks_loading = false;
                this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                match result {
                    Ok(list) => {
                        // Open on the page holding the block being translated so
                        // the running position is visible without scrolling.
                        let done = this
                            .jobs
                            .iter()
                            .find(|job| job.id == detail_job_id)
                            .map(|job| job.progress.completed)
                            .unwrap_or_default()
                            .min(list.total);
                        let pages = list.total.div_ceil(BLOCKS_PER_PAGE).max(1);
                        this.blocks_page = (done / BLOCKS_PER_PAGE).min(pages - 1);
                        this.blocks = Some(list);
                        this.blocks_error = None;
                    }
                    Err(error) => {
                        this.blocks = None;
                        this.blocks_error = Some(format!("无法读取文本块明细：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the debug view of one block and loads its payload. Rebuilding the
    /// request body re-parses the pinned revision, so a block is loaded exactly
    /// once per open; the cached list only ever holds 96-character previews.
    /// `failed` comes from the inspector row that was clicked and decides whether
    /// the model-response tab is offered.
    fn open_block_detail(
        &mut self,
        ordinal: usize,
        failed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(job_id) = self.selected_job_id.clone() else {
            return;
        };
        let generation = self
            .block_detail
            .as_ref()
            .filter(|detail| detail.job_id == job_id && detail.ordinal == ordinal)
            .map(|detail| detail.generation.wrapping_add(1))
            .unwrap_or(0);
        self.block_detail = Some(BlockDetailState {
            job_id: job_id.clone(),
            ordinal,
            generation,
            tab: BlockDetailTab::Overview,
            failed,
            loading: true,
            detail: None,
            error: None,
            probe: BlockProbeState::default(),
            scroll: ScrollHandle::default(),
        });
        self.load_block_detail(job_id, ordinal, window, cx);
    }

    /// Re-asks the model for the open failed block and shows the raw answer.
    ///
    /// This is the only viewer in the window that sends a real request, so it
    /// runs on an explicit click and never on a poll. A block that is no longer
    /// the open one discards its own answer through the same generation guard
    /// [`Self::load_block_detail`] uses.
    fn probe_block_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.block_detail.as_ref() else {
            return;
        };
        if detail.probe.loading {
            return;
        }
        let job_id = detail.job_id.clone();
        let ordinal = detail.ordinal;
        let generation = detail.generation;
        if let Some(detail) = self.block_detail.as_mut() {
            detail.probe = BlockProbeState {
                loading: true,
                result: None,
                error: None,
            };
        }
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services
                .background_job_translation_block_response(job_id, ordinal, book_ids)
                .await;
            let _ = view.update(cx, |this, cx| {
                let Some(detail) = this.block_detail.as_mut() else {
                    return;
                };
                // A newer open of the same block, or of another one, owns the panel.
                if detail.ordinal != ordinal || detail.generation != generation {
                    return;
                }
                match result {
                    Ok(probe) => {
                        detail.probe = BlockProbeState {
                            loading: false,
                            result: Some(probe),
                            error: None,
                        };
                    }
                    Err(error) => {
                        detail.probe = BlockProbeState {
                            loading: false,
                            result: None,
                            error: Some(format!("无法重新请求模型：{error:#}")),
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_block_detail(
        &mut self,
        job_id: String,
        ordinal: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = self.block_detail.as_mut() else {
            return;
        };
        if detail.job_id != job_id || detail.ordinal != ordinal {
            return;
        }
        detail.loading = true;
        detail.error = None;
        let generation = detail.generation;
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services
                .background_job_translation_block_detail(job_id, ordinal, book_ids)
                .await;
            let _ = view.update(cx, |this, cx| {
                let Some(detail) = this.block_detail.as_mut() else {
                    return;
                };
                // A newer open of the same block, or of another one, owns the panel.
                if detail.ordinal != ordinal || detail.generation != generation {
                    return;
                }
                detail.loading = false;
                detail.scroll.set_offset(gpui::point(px(0.), px(0.)));
                match result {
                    Ok(loaded) => {
                        detail.detail = Some(loaded);
                        detail.error = None;
                    }
                    Err(error) => {
                        detail.detail = None;
                        detail.error = Some(format!("无法读取文本块详情：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_block_detail(&mut self, cx: &mut Context<Self>) {
        if self.block_detail.take().is_some() {
            cx.notify();
        }
    }

    fn apply_action(
        &mut self,
        job_id: String,
        action: BackgroundJobAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_job_id.is_some() {
            return;
        }
        self.pending_job_id = Some(job_id.clone());
        let scope = self.scope_generation;
        self.notice = Some(JobsNotice {
            text: format!("正在{}任务…", action_verb(action)),
            error: false,
        });
        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            let result = services.control_background_job(job_id, action).await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |this, cx| {
                    this.pending_job_id = None;
                    if scope == this.scope_generation {
                        this.notice = Some(match result {
                            Ok(true) => JobsNotice {
                                text: format!("已请求{}任务。", action_verb(action)),
                                error: false,
                            },
                            Ok(false) => JobsNotice {
                                text: "任务状态已发生变化，未重复执行操作。".into(),
                                error: false,
                            },
                            Err(error) => JobsNotice {
                                text: format!("{}任务失败：{error:#}", action_verb(action)),
                                error: true,
                            },
                        });
                    }
                    // Invalidate any snapshot that began before the accepted operation.
                    this.refresh_generation = this.refresh_generation.wrapping_add(1);
                    this.loading = false;
                    this.clear_detail_cache();
                    this.refresh(window, cx);
                })
            });
        })
        .detach();
        cx.notify();
    }

    /// Re-runs the named text blocks of the selected translation task.
    ///
    /// A run that skipped blocks still finished as `Succeeded`, so this is the
    /// only path back to them: the user switches to a stronger model first, then
    /// retries. Blocks that already have a translation are filtered out by the
    /// service, so a stale row cannot overwrite good text.
    fn retry_translation_blocks(
        &mut self,
        ordinals: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_job_id.is_some() || ordinals.is_empty() {
            return;
        }
        let Some(job_id) = self.selected_job_id.clone() else {
            return;
        };
        self.pending_job_id = Some(job_id.clone());
        let count = ordinals.len();
        self.notice = Some(JobsNotice {
            text: format!("正在重试 {count} 个文本块…"),
            error: false,
        });
        let scope = self.scope_generation;
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services
                .background_job_retry_translation_blocks(job_id, ordinals, book_ids)
                .await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |this, cx| {
                    this.pending_job_id = None;
                    if scope == this.scope_generation {
                        this.notice = Some(match result {
                            Ok(true) => JobsNotice {
                                text: format!("已请求重试 {count} 个文本块。"),
                                error: false,
                            },
                            // Every requested block already has a translation (or
                            // the task moved on): the caller's list is stale and
                            // there is nothing worth re-sending.
                            Ok(false) => JobsNotice {
                                text: "这些文本块已有译文或任务状态已变化，未重试。".into(),
                                error: false,
                            },
                            Err(error) => JobsNotice {
                                text: format!("重试文本块失败：{error:#}"),
                                error: true,
                            },
                        });
                    }
                    // Drop the cached list so the statuses are re-derived from
                    // the job log the retry just appended to.
                    this.clear_blocks();
                    this.refresh_generation = this.refresh_generation.wrapping_add(1);
                    this.loading = false;
                    this.refresh(window, cx);
                })
            });
        })
        .detach();
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .debug_selector(|| "jobs-layout-header".into())
            .flex_none()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_1()
                    .child(div().text_lg().font_semibold().child("后台任务"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(format!(
                                "{} · {} 本图书 · {} 个任务",
                                self.scope_label,
                                self.books.len(),
                                self.jobs.len()
                            )),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .flex_none()
                    .child(
                        Button::new("jobs-auto-refresh")
                            .small()
                            .ghost()
                            .label(if self.auto_refresh {
                                "自动刷新 · 2 秒"
                            } else {
                                "自动刷新已暂停"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.auto_refresh = !this.auto_refresh;
                                if this.auto_refresh {
                                    this.refresh(window, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("background-jobs-refresh")
                            .small()
                            .outline()
                            .icon(IconName::Redo2)
                            .label(if self.loading {
                                "刷新中…"
                            } else {
                                "刷新"
                            })
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.notice = None;
                                this.refresh(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Task-kind switcher pinned to the top of the window, above the list and
    /// detail columns. Wraps instead of clipping so every category stays
    /// reachable at the minimum window width.
    fn render_kinds_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut bar = div()
            .debug_selector(|| "jobs-layout-kinds".into())
            .flex_none()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .mr_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("任务分类"),
            );
        for &(kind, label) in JOB_KINDS {
            let count = self
                .jobs
                .iter()
                .filter(|job| kind_matches(&job.kind, kind))
                .count();
            if kind == "other" && count == 0 {
                continue;
            }
            let selected = self.kind == kind;
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("jobs-kind-{kind}")))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_sm()
                    .bg(rgb(if selected { ACCENT_SOFT } else { SIDEBAR }))
                    .text_color(rgb(if selected { ACCENT_DARK } else { INK }))
                    .child(label)
                    .child(div().text_xs().child(count.to_string()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.kind = kind;
                        this.filters_changed(window, cx);
                    })),
            );
        }
        bar.into_any_element()
    }

    /// Left column of the body: the status filters, the paged task list and its
    /// footer. Its width is user-draggable; the divider on its right draws the
    /// separation from the detail panel.
    fn render_list_column(
        &self,
        viewport_width: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .debug_selector(|| "jobs-layout-list-column".into())
            .v_flex()
            .w(px(self.effective_list_width(viewport_width)))
            .flex_none()
            .min_h(px(0.))
            .overflow_hidden()
            .child(self.render_filters(cx))
            .child(self.render_list(cx))
            .into_any_element()
    }

    /// Draggable divider between the list column and the detail panel. The
    /// pointer grabs it, follows it horizontally, and releases it in place.
    fn render_resize_handle(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let handle_view = cx.entity().clone();
        let active = self.resizing_list;
        div()
            .id("jobs-list-resize-handle")
            .debug_selector(|| "jobs-layout-resize-handle".into())
            .h_full()
            .w(px(JOBS_RESIZE_HANDLE_WIDTH))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .bg(rgb(SURFACE))
            .hover(|this| this.bg(rgb(ACCENT_SOFT)))
            .on_drag(JobsListResizeDrag, move |_, _, _, cx| {
                cx.stop_propagation();
                handle_view.update(cx, |this, cx| {
                    if this.begin_list_resize() {
                        cx.notify();
                    }
                });
                cx.new(|_| EmptyView)
            })
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<JobsListResizeDrag>, window, cx| {
                    if this.resize_list_from_pointer(
                        event.event.position.x,
                        window.viewport_size().width,
                    ) {
                        cx.notify();
                    }
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.finish_list_resize() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.finish_list_resize() {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .h_full()
                    .w(px(if active { 2. } else { 1. }))
                    .bg(rgb(if active { ACCENT } else { BORDER })),
            )
            .into_any_element()
    }

    /// Status filters and the task search box. The kind switcher above already
    /// names and counts the current scope, so no extra summary line is drawn
    /// here.
    fn render_filters(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut filters = div().h_flex().flex_wrap().gap_1();
        for &(status, label) in STATUS_FILTERS {
            filters = filters.child(
                Button::new(SharedString::from(format!("jobs-filter-{label}")))
                    .small()
                    .ghost()
                    .label(label)
                    .when(self.status == status, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(ACCENT_SOFT).into())
                                .foreground(rgb(ACCENT_DARK).into()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.status = status;
                        this.filters_changed(window, cx);
                    })),
            );
        }
        div()
            .debug_selector(|| "jobs-layout-filters".into())
            .flex_none()
            .v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .w_full()
                    .child(Input::new(&self.search).small().prefix(IconName::Search)),
            )
            .child(filters)
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let jobs = self.filtered_jobs();
        let count = jobs.len();
        let mut list = div()
            .id("background-jobs-list")
            .debug_selector(|| "jobs-layout-list".into())
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .px_3()
            .py_2()
            .gap_1();
        if count == 0 {
            list = list.child(div().p_6().text_sm().text_color(rgb(MUTED)).child(
                if self.loading && !self.loaded {
                    "正在读取任务…"
                } else if !self.loaded && self.refresh_error.is_some() {
                    "读取失败，请刷新重试。"
                } else if self.books.is_empty() {
                    "当前范围没有图书。"
                } else if self.jobs.is_empty() {
                    "当前范围还没有后台任务。"
                } else {
                    "没有符合搜索或筛选条件的任务。"
                },
            ));
        }
        for job in jobs
            .into_iter()
            .skip(self.page * JOBS_PER_PAGE)
            .take(JOBS_PER_PAGE)
        {
            let id = job.id.clone();
            let selected = self.selected_job_id.as_deref() == Some(&job.id);
            let (label, color, background) = status_presentation(job);
            let title = self
                .books
                .iter()
                .find(|book| book.id == job.book_id)
                .map(|book| book.title.as_str())
                .unwrap_or("图书已移除");
            list =
                list.child(
                    div()
                        .id(SharedString::from(format!("job-row-{}", job.id)))
                        .flex_none()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded(px(7.))
                        .border_1()
                        .border_color(rgb(if selected { ACCENT } else { BORDER }))
                        .bg(rgb(if selected { ACCENT_SOFT } else { SURFACE }))
                        .cursor_pointer()
                        .child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .truncate()
                                        .child(title.to_string()),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(MUTED)).truncate().child(
                                        format!("{} · {}", job_kind_label(&job.kind), job.id),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_1()
                                .items_end()
                                .flex_none()
                                .child(
                                    div()
                                        .px_2()
                                        .rounded_full()
                                        .bg(background)
                                        .text_xs()
                                        .text_color(color)
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(MUTED))
                                        .child(progress_label(job)),
                                ),
                        )
                        .child(Icon::new(IconName::ChevronRight).small())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.selected_job_id.as_ref() != Some(&id) {
                                this.selected_job_id = Some(id.clone());
                                this.clear_detail_cache();
                            }
                            this.refresh_logs(window, cx);
                            this.refresh_blocks(window, cx);
                            cx.notify();
                        })),
                );
        }
        let pages = count.div_ceil(JOBS_PER_PAGE).max(1);
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(list.size_full())
                    .vertical_scrollbar(&self.list_scroll),
            )
            .child(
                div()
                    .debug_selector(|| "jobs-layout-footer".into())
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "共 {count} 个任务 · 第 {} / {pages} 页",
                        self.page + 1
                    ))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("jobs-previous")
                                    .small()
                                    .ghost()
                                    .label("上一页")
                                    .disabled(self.page == 0)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.page = this.page.saturating_sub(1);
                                        this.list_scroll.set_offset(gpui::point(px(0.), px(0.)));
                                        this.reconcile_selection();
                                        this.refresh_logs(window, cx);
                                        this.refresh_blocks(window, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("jobs-next")
                                    .debug_selector(|| "jobs-next".into())
                                    .small()
                                    .ghost()
                                    .label("下一页")
                                    .disabled(self.page + 1 >= pages)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.page += 1;
                                        this.list_scroll.set_offset(gpui::point(px(0.), px(0.)));
                                        this.reconcile_selection();
                                        this.refresh_logs(window, cx);
                                        this.refresh_blocks(window, cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(job) = self
            .jobs
            .iter()
            .find(|job| Some(&job.id) == self.selected_job_id.as_ref())
        else {
            return div()
                .debug_selector(|| "jobs-layout-detail".into())
                .v_flex()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .justify_center()
                .p_5()
                .text_sm()
                .text_color(rgb(MUTED))
                .child("选择任务查看运行情况。")
                .into_any_element();
        };
        // Built once per frame: the counters, the header's repair button, the copy
        // payload and the block rows all read the same view of the task.
        let inspector = self
            .blocks
            .as_ref()
            .map(|blocks| BlockInspector::new(job, blocks.total, self.logs.as_ref()));
        // A finished translation that skipped blocks offers a second repair route
        // in front of 重新翻译: re-send only the blocks that never got a
        // translation, after the user switched to a stronger model. The ordinals
        // are resolved here because a click handler must not borrow the per-frame
        // inspector, and the button is limited to the block tab because the failed
        // set is derived from that list.
        let failed_ordinals: Option<Vec<usize>> = if self.tab == DetailTab::Blocks {
            inspector.as_ref().and_then(|inspector| {
                let ordinals: Vec<usize> = (0..inspector.progress.total)
                    .filter(|index| inspector.state(*index) == BlockState::Failed)
                    .collect();
                (!ordinals.is_empty()).then_some(ordinals)
            })
        } else {
            None
        };
        let mut actions = div().h_flex().gap_1().flex_wrap();
        if let Some(ordinals) = failed_ordinals {
            actions = actions.child(
                Button::new("jobs-blocks-retry-failed")
                    .debug_selector(|| "jobs-blocks-retry-failed".into())
                    .small()
                    .outline()
                    .label(format!("重试失败块（{}）", ordinals.len()))
                    .disabled(self.pending_job_id.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.retry_translation_blocks(ordinals.clone(), window, cx);
                    })),
            );
        }
        for action in available_actions(job) {
            let id = job.id.clone();
            // The debug selector drops the job id so the layout test can name it
            // as a literal while it asserts where the repair button sits relative
            // to 重新翻译.
            let selector = format!("background-job-action-{}", action_id(action));
            actions = actions.child(
                Button::new(SharedString::from(format!(
                    "background-job-{}-{}",
                    job.id,
                    action_id(action)
                )))
                .debug_selector(move || selector)
                .small()
                .outline()
                .label(action_label(action))
                .disabled(self.pending_job_id.is_some())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_action(id.clone(), action, window, cx)
                })),
            );
        }
        let mut tabs = div().h_flex().gap_1();
        let mut tab_items = vec![
            (DetailTab::Summary, "任务详情"),
            (DetailTab::Logs, "运行日志"),
        ];
        if job.kind == "translation" {
            tab_items.push((DetailTab::Blocks, "文本块明细"));
        }
        for (tab, label) in tab_items {
            tabs = tabs.child(
                Button::new(SharedString::from(format!("jobs-tab-{}", tab_id(tab))))
                    .small()
                    .ghost()
                    .label(label)
                    .when(self.tab == tab, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(ACCENT_SOFT).into())
                                .foreground(rgb(ACCENT_DARK).into()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.tab = tab;
                        this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                        this.refresh_logs(window, cx);
                        this.refresh_blocks(window, cx);
                        cx.notify();
                    })),
            );
        }
        let copy_text = match self.tab {
            DetailTab::Logs => self
                .logs
                .as_ref()
                .map(|logs| diagnostic_log_copy(job, logs)),
            DetailTab::Blocks => self.blocks.as_ref().and_then(|blocks| {
                inspector
                    .as_ref()
                    .map(|inspector| translation_blocks_copy(job, blocks, inspector))
            }),
            DetailTab::Summary => Some(diagnostic_summary(job)),
        };
        let copy_disabled = copy_text.is_none()
            || match self.tab {
                DetailTab::Logs => self.logs_error.is_some() || self.logs_loading,
                DetailTab::Blocks => self.blocks_loading,
                DetailTab::Summary => false,
            };
        let copy_view = cx.entity().clone();
        let header = div()
            .flex_none()
            .h_flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(tabs)
            .child(actions)
            .child(
                Button::new("jobs-copy-diagnostics")
                    .debug_selector(|| "jobs-copy-diagnostics".into())
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .label(match self.tab {
                        DetailTab::Logs => "复制日志",
                        DetailTab::Blocks => "复制概览",
                        DetailTab::Summary => "复制详情",
                    })
                    .disabled(copy_disabled)
                    .on_click(move |_, _, cx| {
                        if let Some(text) = &copy_text {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                            copy_view.update(cx, |this, cx| {
                                this.notice = Some(JobsNotice {
                                    text: "已复制所选任务的诊断信息。".into(),
                                    error: false,
                                });
                                cx.notify();
                            });
                        }
                    }),
            );
        let mut content = div()
            .id(SharedString::from(format!(
                "job-detail-{}-{}",
                job.id,
                tab_id(self.tab)
            )))
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .p_4()
            .gap_2()
            .text_xs();
        if self.tab == DetailTab::Blocks {
            for row in self.translation_block_rows(job, inspector.as_ref(), cx) {
                content = content.child(row);
            }
        } else if self.tab == DetailTab::Logs {
            content = content.child(
                div()
                    .text_color(rgb(MUTED))
                    .child("最新记录在前 · 时间为 UTC · ordinal 从 0 开始 · 复制按时间正序排列。"),
            );
            if let Some(error) = &self.logs_error {
                content = content.child(div().text_color(rgb(DANGER)).child(error.clone()));
            }
            if let Some(logs) = &self.logs {
                if logs.truncated {
                    content = content.child(
                        div()
                            .text_color(rgb(MUTED))
                            .child("较早的记录已按保留上限清理，以下为最近的运行日志。"),
                    );
                }
                if logs.entries.is_empty() {
                    content = content.child(div().py_3().text_color(rgb(MUTED)).child(
                        "暂无保留的运行日志。任务可能尚未开始、在升级前执行，或记录已清理；后续运行会自动记录。",
                    ));
                }
                for entry in logs.entries.iter().rev() {
                    content = content.child(
                        div()
                            .flex_none()
                            .line_height(gpui::relative(1.5))
                            .border_b_1()
                            .border_color(rgb(BORDER))
                            .py_1()
                            .child(entry.format_line()),
                    );
                }
            } else if self.logs_loading {
                content = content.child("正在读取日志…");
            }
        } else {
            for (label, value) in detail_rows(job) {
                content = content.child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .text_color(rgb(MUTED))
                                .child(label),
                        )
                        .child(div().flex_1().min_w(px(0.)).child(value)),
                );
            }
            if let Some(error) = &job.error {
                content = content.child(
                    div()
                        .p_2()
                        .rounded(px(6.))
                        .bg(rgb(0xf8e2de))
                        .text_color(rgb(DANGER))
                        .child(error.clone()),
                );
            }
            if let Some(cursor) = &job.cursor_json {
                content = content
                    .child(div().text_color(rgb(MUTED)).child("执行游标"))
                    .child(div().p_2().bg(rgb(SIDEBAR)).child(prettify_cursor(cursor)));
            }
        }
        div()
            .debug_selector(|| "jobs-layout-detail".into())
            .v_flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .bg(rgb(SURFACE))
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(content.size_full())
                    .vertical_scrollbar(&self.detail_scroll),
            )
            .into_any_element()
    }

    /// Rows of the translation block inspector: live counters, state filters,
    /// the current page of blocks, and the page controls. Blocks are listed by
    /// ascending task cursor so `#n` matches the `ordinal` in the run log.
    fn translation_block_rows(
        &self,
        job: &BackgroundJobSnapshot,
        inspector: Option<&BlockInspector>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        rows.push(
            div()
                .text_color(rgb(MUTED))
                .child(
                    "文本块按任务游标顺序排列，序号从 0 开始，与运行日志的 ordinal 一致；预览为原文开头。状态随任务进度实时更新。",
                )
                .into_any_element(),
        );
        if let Some(error) = &self.blocks_error {
            rows.push(
                div()
                    .text_color(rgb(DANGER))
                    .child(error.clone())
                    .into_any_element(),
            );
            rows.push(
                Button::new("jobs-blocks-reload")
                    .small()
                    .outline()
                    .label("重新加载文本块明细")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_blocks();
                        this.refresh_blocks(window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            return rows;
        }
        // The inspector and the block list are built together, so neither can
        // exist without the other.
        let Some((blocks, inspector)) = self.blocks.as_ref().zip(inspector) else {
            rows.push(
                div()
                    .text_color(rgb(MUTED))
                    .child(if self.blocks_loading {
                        "正在读取文本块明细…"
                    } else {
                        "尚未加载文本块明细。"
                    })
                    .into_any_element(),
            );
            return rows;
        };
        let progress = &inspector.progress;
        if let Some(banner) = failure_banner(job, inspector) {
            rows.push(banner);
        }
        rows.push(
            div()
                .h_flex()
                .flex_wrap()
                .gap_2()
                .text_color(rgb(INK))
                .child(format!(
                    "共 {} 个文本块 · 已处理 {} · 处理中 {} · 失败 {} · 待处理 {}",
                    progress.total,
                    progress.done,
                    progress.processing.len(),
                    progress.failed,
                    progress.pending
                ))
                .into_any_element(),
        );
        rows.push(
            div()
                .text_color(rgb(MUTED))
                .child(format!(
                    "目标语言：{}",
                    translation_language_label(&blocks.target_language)
                        .unwrap_or(blocks.target_language.as_str())
                ))
                .into_any_element(),
        );

        let mut filters = div().h_flex().flex_wrap().gap_1();
        for (state, label) in [
            (None, format!("全部 {}", progress.total)),
            (Some(BlockState::Done), format!("已处理 {}", progress.done)),
            (
                Some(BlockState::Processing),
                format!("处理中 {}", progress.processing.len()),
            ),
            (
                Some(BlockState::Failed),
                format!("失败 {}", progress.failed),
            ),
            (
                Some(BlockState::Pending),
                format!("待处理 {}", progress.pending),
            ),
        ] {
            let id = state.map(BlockState::id).unwrap_or("all");
            filters = filters.child(
                Button::new(SharedString::from(format!("jobs-blocks-filter-{id}")))
                    .small()
                    .ghost()
                    .label(label)
                    .when(self.blocks_state == state, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(ACCENT_SOFT).into())
                                .foreground(rgb(ACCENT_DARK).into()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.blocks_state = state;
                        this.blocks_page = 0;
                        this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                        cx.notify();
                    })),
            );
        }
        rows.push(filters.into_any_element());

        // The repair entry point sits in the header, in front of 重新翻译. This
        // line stays here because it is about the list it belongs to: it explains
        // how the two retries differ, so a failed row is not confused with a
        // whole-list re-send.
        if progress.failed > 0 {
            rows.push(
                div()
                    .text_color(rgb(MUTED))
                    .child(
                        "更换更擅长指令的对话模型后，可用右上方「重试失败块」重跑全部未翻译的块；\
                         行内「重试」只重跑这一块。",
                    )
                    .into_any_element(),
            );
        }

        let filtered = progress.filtered(self.blocks_state);
        let pages = filtered.div_ceil(BLOCKS_PER_PAGE).max(1);
        let page = self.blocks_page.min(pages - 1);
        let indices = block_page_indices(blocks, &inspector, self.blocks_state, page);
        // Resolved here: a click handler must not borrow the block list, and the
        // anchor page depends on the filter that is active right now.
        let anchor = anchor_page(blocks, &inspector, self.blocks_state).min(pages - 1);
        rows.push(
            div()
                .h_flex()
                .gap_2()
                .text_color(rgb(MUTED))
                .child(match (indices.first(), indices.last()) {
                    (Some(first), Some(last)) => format!(
                        "显示 #{first}–#{last}（筛选后 {filtered} 块 · 第 {}/{} 页）",
                        page + 1,
                        pages
                    ),
                    _ => format!("筛选后 0 块 · 第 {}/{} 页", page + 1, pages),
                })
                .into_any_element(),
        );
        if indices.is_empty() {
            rows.push(
                div()
                    .py_2()
                    .text_color(rgb(MUTED))
                    .child("当前筛选下没有文本块。")
                    .into_any_element(),
            );
        }
        for index in indices {
            let Some(block) = blocks.blocks.get(index) else {
                continue;
            };
            let state = inspector.state(index);
            let (foreground, background) = state.colors();
            // Resolved before the row is built: the click handler must not
            // borrow the block list, which lives only for this frame.
            let ordinal = block.ordinal;
            rows.push(
                div()
                    .v_flex()
                    .gap_1()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .h_flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(52.))
                                    .flex_none()
                                    .text_color(rgb(MUTED))
                                    .child(format!("#{}", block.ordinal)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px_2()
                                    .rounded(px(4.))
                                    .bg(background)
                                    .text_color(foreground)
                                    .child(state.label()),
                            )
                            .child(
                                div()
                                    .w(px(128.))
                                    .flex_none()
                                    .truncate()
                                    .text_color(rgb(INK))
                                    .child(block_chapter_label(block)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(rgb(MUTED))
                                    .child(if block.source_preview.is_empty() {
                                        "（无文字内容）".to_string()
                                    } else {
                                        block.source_preview.clone()
                                    }),
                            )
                            // The preview is capped at 96 characters and the list
                            // holds no full text, so inspecting one block is the
                            // only way to see what the worker actually sends.
                            .child(
                                Button::new(SharedString::from(format!(
                                    "jobs-block-view-{}",
                                    block.ordinal
                                )))
                                .debug_selector({
                                    let selector = format!("jobs-block-view-{ordinal}");
                                    move || selector
                                })
                                .small()
                                .ghost()
                                .flex_none()
                                .label("查看")
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.open_block_detail(
                                            ordinal,
                                            state == BlockState::Failed,
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            // Only a failed row offers a retry, and it re-sends
                            // exactly this block: the run that skipped it finished
                            // as `Succeeded`, so this button is the way back to one
                            // block after switching to a better model. Other
                            // failed blocks are left alone; the header's
                            // 重试失败块 is the bulk route.
                            .when(state == BlockState::Failed, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "jobs-block-retry-{}",
                                        ordinal
                                    )))
                                    .debug_selector({
                                        let selector = format!("jobs-block-retry-{ordinal}");
                                        move || selector
                                    })
                                    .small()
                                    .flex_none()
                                    .label("重试")
                                    .disabled(self.pending_job_id.is_some())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.retry_translation_blocks(vec![ordinal], window, cx);
                                    })),
                                )
                            }),
                    )
                    // Only failed rows get a second line, so a healthy task keeps
                    // exactly the width the columns had before.
                    .when_some(
                        inspector.reason(index).map(str::to_string),
                        |row, reason| {
                            row.child(
                                div()
                                    .pl(px(60.))
                                    .text_color(rgb(DANGER))
                                    .child(format!("失败原因：{reason}")),
                            )
                        },
                    )
                    .into_any_element(),
            );
        }

        rows.push(
            div()
                .h_flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("jobs-blocks-prev")
                        .small()
                        .outline()
                        .label("上一页")
                        .disabled(page == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.blocks_page = this.blocks_page.saturating_sub(1);
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("jobs-blocks-next")
                        .small()
                        .outline()
                        .label("下一页")
                        .disabled(page + 1 >= pages)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.blocks_page = (this.blocks_page + 1).min(pages - 1);
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("jobs-blocks-current")
                        .small()
                        .ghost()
                        .label("跳到当前进度")
                        .disabled(progress.total == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.blocks_page = anchor;
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        rows
    }

    /// Debug view of one text block: the identifiers, the full source, and the
    /// exact request body the worker would send. Rendered as an in-window
    /// overlay so it stays attached to the task it describes.
    fn render_block_detail_modal(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.block_detail.as_ref()?;
        let ordinal = state.ordinal;
        let detail = state.detail.as_ref();
        let text = block_detail_text(state, detail);

        let mut tabs = div().h_flex().flex_wrap().gap_1();
        for tab in block_detail_tabs(state.failed) {
            tabs = tabs.child(
                Button::new(SharedString::from(format!(
                    "jobs-block-detail-tab-{}",
                    tab.id()
                )))
                .small()
                .ghost()
                .label(tab.label())
                .when(state.tab == tab, |button| {
                    button.custom(
                        ButtonCustomVariant::new(cx)
                            .color(rgb(ACCENT_SOFT).into())
                            .foreground(rgb(ACCENT_DARK).into()),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(state) = this.block_detail.as_mut() {
                        state.tab = tab;
                        state.scroll.set_offset(gpui::point(px(0.), px(0.)));
                    }
                    cx.notify();
                })),
            );
        }

        let copy_view = cx.entity().clone();
        let copy_text = text.clone();
        let title = match detail {
            Some(detail) if !detail.unit_title.is_empty() => format!(
                "文本块 #{ordinal} · {} · 第 {} 章",
                detail.block_id,
                detail.unit_ordinal + 1
            ),
            _ => format!("文本块 #{ordinal}"),
        };

        // Height is resolved here so the scroll container owns a real bound: a
        // naturally sized body would make the overlay grow past the viewport and
        // the body text past the largest texture GPUI can rasterize.
        let available = f32::from(window.viewport_size().height) - 40.;
        let panel_height = if available.is_finite() {
            available.clamp(320., 720.)
        } else {
            640.
        };
        let scroll_handle = state.scroll.clone();
        let mut body = div()
            .id("jobs-block-detail-body")
            .debug_selector(|| "jobs-block-detail-body".into())
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .pr_1();
        if let Some(error) = state.error.as_ref() {
            body = body.child(div().text_xs().text_color(rgb(DANGER)).child(error.clone()));
        } else if state.loading {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("正在读取文本块详情…"),
            );
        } else if state.tab == BlockDetailTab::Response {
            body = body.child(self.render_block_probe(&state.probe, cx));
        } else if let Some(text) = text.as_ref() {
            body = body.child(
                div()
                    .id(SharedString::from(format!(
                        "jobs-block-detail-text-{}",
                        state.tab.id()
                    )))
                    .w_full()
                    .flex_none()
                    .text_xs()
                    .line_height(gpui::relative(1.5))
                    .text_color(rgb(INK))
                    // `whitespace_normal` keeps the JSON indentation while still
                    // breaking long lines, so a body never runs off the panel.
                    .whitespace_normal()
                    .child(text.clone()),
            );
        }

        let close_view = cx.entity().clone();
        let hint = (detail.is_some()).then(|| match state.tab {
            // The one tab whose text is not reconstructible: say where it came
            // from, so a replayed answer is never mistaken for a stored one.
            BlockDetailTab::Response => {
                "模型响应为一次现场重放的原始回答，只显示在这里，不落库、不写译文。"
            }
            _ => "请求 Body 为工作进程构造的完整 chat-completions 请求，仅用于调试，不落库。",
        });
        let footer = div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .children(hint.map(str::to_string)),
            )
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new("jobs-block-detail-copy")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label("复制当前页")
                            .disabled(copy_text.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(text) = &copy_text {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    copy_view.update(cx, |this, cx| {
                                        this.notice = Some(JobsNotice {
                                            text: "已复制文本块调试信息。".into(),
                                            error: false,
                                        });
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("jobs-block-detail-close")
                            .small()
                            .primary()
                            .label("关闭")
                            .on_click(move |_, _, cx| {
                                close_view.update(cx, |this, cx| this.close_block_detail(cx));
                            }),
                    ),
            );

        Some(
            div()
                .id("jobs-block-detail-overlay")
                .absolute()
                .inset_0()
                // Painting an overlay does not remove the hitboxes underneath it;
                // make the panel a real mouse barrier so a dismissal cannot also
                // activate a block row behind it.
                .occlude()
                .bg(rgba(0x1f1d1a80))
                .v_flex()
                .items_center()
                .justify_center()
                .on_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.button == gpui::MouseButton::Left {
                        this.close_block_detail(cx);
                    }
                }))
                .child(
                    div()
                        .id("jobs-block-detail-panel")
                        .debug_selector(|| "jobs-block-detail-panel".into())
                        .occlude()
                        .v_flex()
                        .w(px(720.))
                        .h(px(panel_height))
                        .max_w(gpui::relative(0.92))
                        .p_5()
                        .gap_3()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(SURFACE))
                        .shadow_lg()
                        .child(
                            div()
                                .v_flex()
                                .flex_none()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(rgb(INK))
                                        .truncate()
                                        .child(title),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(MUTED)).truncate().child(
                                        detail
                                            .map(|detail| {
                                                // A task that never ran records no
                                                // model, so the header must not
                                                // open on a leading separator.
                                                let model = match detail.request.model.trim() {
                                                    "" => "未记录模型".to_string(),
                                                    model => model.to_string(),
                                                };
                                                format!(
                                                    "{model} · 原文 {} 字符 · {} 个片段 · 单元 {}",
                                                    detail.source_text.chars().count(),
                                                    detail.segments.len(),
                                                    detail.unit_id
                                                )
                                            })
                                            .unwrap_or_else(|| {
                                                "该文本块尚无可用详情。".to_string()
                                            }),
                                    ),
                                ),
                        )
                        .child(tabs)
                        .child(div().flex_none().h(px(1.)).w_full().bg(rgb(BORDER)))
                        .child(body)
                        .child(footer),
                )
                .into_any_element(),
        )
    }

    /// Panel of the model-response tab: one on-demand replay of the open failed
    /// block, its raw answer, and the worker's own verdict on that answer.
    ///
    /// This is the only place in the window that sends a model request, so it
    /// runs exclusively on the button below — never on a poll, never on open —
    /// and the button is disabled while a replay is in flight.
    fn render_block_probe(
        &self,
        probe: &BlockProbeState,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut panel = div().v_flex().w_full().flex_none().gap_2().child(
            div().text_xs().text_color(rgb(MUTED)).child(
                "失败块才有这一页：诊断日志只保存固定分类，所以这里按同一份冻结原文重新请求一次，\
                 把模型返回的原文原样显示出来。这次请求不写译文、不推进任务、不改任何记录。",
            ),
        );
        panel = panel.child(
            div().h_flex().flex_none().items_center().gap_2().child(
                Button::new("jobs-block-probe-run")
                    .debug_selector(|| "jobs-block-probe-run".into())
                    .small()
                    .outline()
                    .icon(IconName::Redo)
                    .label(if probe.loading {
                        "正在请求模型…"
                    } else if probe.result.is_some() {
                        "再请求一次"
                    } else {
                        "重新请求一次"
                    })
                    .disabled(probe.loading)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.probe_block_response(window, cx);
                    })),
            ),
        );
        if probe.loading {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("已发出请求，正在等待模型返回；响应会先在这里显示，不会写进书库。"),
            );
        }
        if let Some(error) = probe.error.as_ref() {
            panel = panel.child(div().text_xs().text_color(rgb(DANGER)).child(error.clone()));
        }
        let Some(result) = probe.result.as_ref() else {
            return panel.into_any_element();
        };
        panel = panel
            .child(div().flex_none().h(px(1.)).w_full().bg(rgb(BORDER)))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "调用模型 {} · 耗时 {:.1} 秒 · 收到 {} 字节 · {}",
                        result.model,
                        result.elapsed_ms as f64 / 1000.,
                        result.response_bytes,
                        probe_end_label(result)
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if result.decode_error_kind.is_some() {
                        rgb(DANGER)
                    } else {
                        rgb(0x376441)
                    })
                    .child(probe_decode_label(result)),
            );
        let body = if result.response_text.is_empty() {
            div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child("（模型没有返回任何内容）")
                .into_any_element()
        } else {
            div()
                .id("jobs-block-probe-text")
                .debug_selector(|| "jobs-block-probe-text".into())
                .w_full()
                .flex_none()
                .text_xs()
                .line_height(gpui::relative(1.5))
                .text_color(rgb(INK))
                .whitespace_normal()
                .child(prettify_json(&result.response_text))
                .into_any_element()
        };
        panel.child(body).into_any_element()
    }
}

/// Text of the open tab. `None` while the payload is still loading or failed,
/// which is exactly when the panel shows a status line instead of content.
fn block_detail_text(
    state: &BlockDetailState,
    detail: Option<&TranslationBlockDetail>,
) -> Option<String> {
    // The replayed answer is not part of the pinned payload — it only exists
    // after the user asked the model again — so it is answered before the
    // payload gate below rather than through it.
    if state.tab == BlockDetailTab::Response {
        return state
            .probe
            .result
            .as_ref()
            .map(|probe| prettify_json(&probe.response_text));
    }
    let detail = detail?;
    let request = &detail.request;
    Some(match state.tab {
        BlockDetailTab::Overview => {
            let target = translation_language_label(&request.target_language)
                .unwrap_or(request.target_language.as_str());
            [
                format!("文本块序号: #{}", detail.ordinal),
                format!("文本块标识: {}", detail.block_id),
                format!("所属单元: {}", detail.unit_id),
                format!(
                    "单元序号: 第 {} 章{}",
                    detail.unit_ordinal + 1,
                    match detail.unit_title.trim() {
                        "" => String::new(),
                        title => format!("（{title}）"),
                    }
                ),
                format!("单元版本: {}", detail.unit_revision),
                format!("原文长度: {} 字符", detail.source_text.chars().count()),
                format!("片段数量: {}", detail.segments.len()),
                format!("片段字符数: {}", segment_char_label(&detail.segments)),
                String::new(),
                format!("调用模型: {}", request.model),
                format!("目标语言: {target}（{}）", request.target_language),
                format!(
                    "原文语言标记: {}",
                    request
                        .source_language
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or("未设置")
                ),
                format!("temperature: {}", request.temperature),
                format!("max_tokens: {}", request.max_tokens),
                "reasoning_effort: none".to_string(),
                "messages: 2（system + user）".to_string(),
                format!(
                    "system 长度: {} 字符",
                    request.system_prompt.chars().count()
                ),
                format!("user 长度: {} 字符", request.user_content.chars().count()),
                format!("body 长度: {} 字节", request.body.len()),
            ]
            .join("\n")
        }
        BlockDetailTab::Source => detail.source_text.clone(),
        BlockDetailTab::Body => prettify_json(&request.body),
        BlockDetailTab::Prompt => request.system_prompt.clone(),
        BlockDetailTab::Message => prettify_json(&request.user_content),
        // The replayed answer itself, never the replayed *result*: a valid JSON
        // object is indented for reading, anything else (a fenced block, a
        // truncated object, provider prose) is shown byte for byte, because that
        // shape is the finding.
        BlockDetailTab::Response => unreachable!("the replayed answer is returned above"),
    })
}

/// How one replayed stream ended, in the window's words. Ending early is the
/// most common reason a block "fails" while the model looks healthy, so the
/// model's own `finish_reason` is named whenever there is one.
fn probe_end_label(probe: &TranslationBlockProbe) -> String {
    match probe.end {
        "done" => "模型正常收尾".to_string(),
        "unexpected_finish_reason" => format!(
            "模型提前结束响应（finish_reason={}）",
            probe.finish_reason.unwrap_or("other")
        ),
        "stream_eof" => "响应流在没有收尾标记的情况下关闭".to_string(),
        "size_limit" => "响应超过大小上限，只显示已收到的部分".to_string(),
        "tool_call" => "模型意外请求了工具".to_string(),
        other => other.to_string(),
    }
}

/// What the worker's own strict decoder made of the replayed answer. The window
/// renders the category label instead of re-implementing the protocol rules.
fn probe_decode_label(probe: &TranslationBlockProbe) -> String {
    match probe.decode_error_kind {
        None => format!(
            "严格解码通过：{} 个片段",
            probe.decoded_segments.unwrap_or(0)
        ),
        Some(kind) => {
            let coordinates = match (probe.decode_error_line, probe.decode_error_column) {
                (Some(line), Some(column)) => format!("（第 {line} 行第 {column} 列）"),
                _ => String::new(),
            };
            format!("严格解码失败：{}{coordinates}", kind.label())
        }
    }
}

/// Per-segment character counts of one block. Numbers only, so it can be read
/// without printing the source text again.
fn segment_char_label(segments: &[String]) -> String {
    if segments.is_empty() {
        return "无片段".to_string();
    }
    segments
        .iter()
        .map(|segment| segment.chars().count().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pretty-prints a JSON payload for display. A value that cannot be re-parsed
/// is shown verbatim rather than silently dropped.
fn prettify_json(raw: &str) -> String {
    let parsed = match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => value,
        Err(_) => return raw.to_string(),
    };
    match serde_json::to_string_pretty(&parsed) {
        Ok(pretty) => pretty,
        Err(_) => raw.to_string(),
    }
}

impl Render for BackgroundJobsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_width = window.viewport_size().width;
        let detail_modal = self.render_block_detail_modal(window, cx);
        div()
            .debug_selector(|| "jobs-layout-root".into())
            .v_flex()
            .size_full()
            .overflow_hidden()
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .child(self.render_header(cx))
            .when_some(self.refresh_error.as_ref(), |this, error| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .text_xs()
                        .bg(rgb(0xf8e2de))
                        .text_color(rgb(DANGER))
                        .child(error.clone()),
                )
            })
            .when_some(self.notice.as_ref(), |this, notice| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .text_xs()
                        .bg(rgb(if notice.error { 0xf8e2de } else { ACCENT_SOFT }))
                        .text_color(rgb(if notice.error { DANGER } else { ACCENT_DARK }))
                        .child(notice.text.clone()),
                )
            })
            .child(self.render_kinds_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(self.render_list_column(viewport_width, cx))
                    .child(self.render_resize_handle(cx))
                    .child(self.render_detail(cx)),
            )
            .children(detail_modal)
    }
}

fn kind_matches(kind: &str, filter: &str) -> bool {
    match filter {
        "" => true,
        "other" => !matches!(
            kind,
            "translation" | "embedding" | "vision" | "visual_render"
        ),
        _ => kind == filter,
    }
}

fn job_matches(
    job: &BackgroundJobSnapshot,
    books: &[BackgroundJobBook],
    kind: &str,
    status: Option<BackgroundJobStatus>,
    query: &str,
) -> bool {
    kind_matches(&job.kind, kind)
        && status.is_none_or(|status| job.status == status)
        && (query.is_empty()
            || job.id.to_lowercase().contains(query)
            || books
                .iter()
                .any(|book| book.id == job.book_id && book.title.to_lowercase().contains(query)))
}

/// Banner above the block list. A failed task used to show nothing at all about
/// why it stopped, so this carries the persisted reason and names the blocks the
/// newest execution left untranslated. `None` when there is nothing to report.
fn failure_banner(
    job: &BackgroundJobSnapshot,
    inspector: &BlockInspector,
) -> Option<gpui::AnyElement> {
    let failed = inspector.progress.failed;
    let failed_task = job.status == BackgroundJobStatus::Failed;
    if !failed_task && failed == 0 {
        return None;
    }
    let mut content = div().v_flex().gap_1();
    if failed_task {
        content = content.child(format!(
            "任务失败：{}",
            job.error.as_deref().unwrap_or("未记录失败原因")
        ));
    } else {
        content = content.child(format!(
            "本次执行有 {failed} 个文本块没有译文，已保留原文；可筛选“失败”查看。"
        ));
    }
    if let Some(summary) = untranslated_summary(inspector) {
        content = content.child(summary);
    }
    if inspector.failures.truncated {
        content = content.child("诊断记录已按保留上限清理，上面的清单可能不完整。");
    }
    let (foreground, background) = if failed_task {
        (rgb(DANGER), rgb(0xf8e2de))
    } else {
        (rgb(ACCENT_DARK), rgb(ACCENT_SOFT))
    };
    Some(
        div()
            .p_2()
            .rounded(px(6.))
            .bg(background)
            .text_color(foreground)
            .child(content)
            .into_any_element(),
    )
}

/// Which blocks the newest execution left untranslated. The list is bounded so
/// a book where every block failed cannot turn the banner into a wall of
/// numbers; the "失败" filter shows the rest.
fn untranslated_summary(inspector: &BlockInspector) -> Option<String> {
    const LISTED: usize = 12;
    let ordinals = inspector
        .failures
        .reasons
        .keys()
        .filter(|ordinal| **ordinal < inspector.progress.total)
        .collect::<Vec<_>>();
    if ordinals.is_empty() {
        return None;
    }
    let listed = ordinals
        .iter()
        .take(LISTED)
        .map(|ordinal| format!("#{ordinal}"))
        .collect::<Vec<_>>()
        .join("、");
    Some(if ordinals.len() > LISTED {
        format!(
            "本次执行未翻译的文本块：{listed} … 共 {} 个",
            ordinals.len()
        )
    } else {
        format!("本次执行未翻译的文本块：{listed}")
    })
}

fn tab_id(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Summary => "summary",
        DetailTab::Logs => "logs",
        DetailTab::Blocks => "blocks",
    }
}

/// Indices of the blocks on one page of the inspector. The scan stops as soon
/// as the page is filled, so a large book never materializes every match.
fn block_page_indices(
    blocks: &TranslationBlockList,
    inspector: &BlockInspector,
    filter: Option<BlockState>,
    page: usize,
) -> Vec<usize> {
    let start = page.saturating_mul(BLOCKS_PER_PAGE);
    let end = start.saturating_add(BLOCKS_PER_PAGE);
    let mut matched = 0usize;
    let mut indices = Vec::new();
    for index in 0..blocks.total {
        if filter.is_some_and(|filter| filter != inspector.state(index)) {
            continue;
        }
        if matched >= start {
            indices.push(index);
        }
        matched += 1;
        if matched >= end {
            break;
        }
    }
    indices
}

/// Page holding the block at the durable cursor inside one filtered view. The
/// inspector pages the filtered list, so "跳到当前进度" has to map the cursor's
/// ordinal into that view instead of assuming the unfiltered ordering.
fn anchor_page(
    blocks: &TranslationBlockList,
    inspector: &BlockInspector,
    filter: Option<BlockState>,
) -> usize {
    let Some(last) = blocks.total.checked_sub(1) else {
        return 0;
    };
    let target = inspector.progress.cursor.min(last);
    let position = (0..target)
        .filter(|index| filter.is_none_or(|state| state == inspector.state(*index)))
        .count();
    position / BLOCKS_PER_PAGE
}

fn block_chapter_label(block: &TranslationBlockInfo) -> String {
    if block.unit_title.trim().is_empty() {
        format!("第 {} 章", block.unit_ordinal + 1)
    } else {
        block.unit_title.clone()
    }
}

fn translation_blocks_copy(
    job: &BackgroundJobSnapshot,
    blocks: &TranslationBlockList,
    inspector: &BlockInspector,
) -> String {
    let progress = &inspector.progress;
    let mut lines = vec![
        format!("任务 ID: {}", job.id),
        format!("图书 ID: {}", job.book_id),
        format!(
            "目标语言: {}",
            translation_language_label(&blocks.target_language)
                .unwrap_or(blocks.target_language.as_str())
        ),
        format!(
            "文本块: 共 {} · 已处理 {} · 处理中 {} · 失败 {} · 待处理 {}",
            progress.total,
            progress.done,
            progress.processing.len(),
            progress.failed,
            progress.pending
        ),
        "序号与任务游标一致，从 0 开始。".to_string(),
    ];
    if progress.failed > 0 {
        let ordinals = inspector
            .failures
            .reasons
            .iter()
            .map(|(ordinal, kind)| match kind {
                Some(kind) => format!("#{ordinal} {}", kind.label()),
                None => format!("#{ordinal} 未记录分类"),
            })
            .collect::<Vec<_>>()
            .join("、");
        lines.push(format!("本次执行未翻译的文本块: {ordinals}"));
        if inspector.failures.truncated {
            lines.push("诊断记录已按保留上限清理，上面的列表可能不完整。".into());
        }
    }
    if let Some(error) = &job.error {
        lines.push(format!("失败原因: {error}"));
    }
    lines.join("\n")
}

fn detail_rows(job: &BackgroundJobSnapshot) -> Vec<(&'static str, String)> {
    vec![
        ("任务 ID", job.id.clone()),
        ("图书 ID", job.book_id.clone()),
        ("来源 ID", job.source_id.clone().unwrap_or_else(not_set)),
        ("任务类型", job_kind_label(&job.kind).into()),
        (
            "状态 / 尝试",
            format!(
                "{} / {} 次",
                status_database_label(job.status),
                job.attempts
            ),
        ),
        ("处理进度", progress_label(job)),
        ("创建时间 UTC", format_timestamp(job.created_at)),
        ("更新时间 UTC", format_timestamp(job.updated_at)),
        (
            "开始时间 UTC",
            job.started_at.map(format_timestamp).unwrap_or_else(not_set),
        ),
        (
            "结束时间 UTC",
            job.finished_at
                .map(format_timestamp)
                .unwrap_or_else(not_set),
        ),
        (
            "控制请求",
            if job.cancel_requested {
                "正在取消"
            } else if job.pause_requested {
                "正在暂停"
            } else {
                "无"
            }
            .into(),
        ),
    ]
}

fn diagnostic_log_copy(job: &BackgroundJobSnapshot, logs: &BackgroundJobLogSnapshot) -> String {
    let summary = detail_rows(job)
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = logs
        .entries
        .iter()
        .map(|entry| entry.format_line())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{summary}\n\n最近保留的日志：{} 条；较早记录{}清理；时间正序，ordinal 从 0 开始。\n{lines}",
        logs.entries.len(),
        if logs.truncated {
            "已"
        } else {
            "可能按全局上限"
        }
    )
}

fn diagnostic_summary(job: &BackgroundJobSnapshot) -> String {
    let mut lines = detail_rows(job)
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>();
    if let Some(error) = &job.error {
        lines.push(format!("错误: {error}"));
    }
    if let Some(cursor) = &job.cursor_json {
        lines.push(format!("执行游标:\n{}", prettify_cursor(cursor)));
    }
    lines.join("\n")
}

pub(super) fn open_background_jobs_window(
    services: Arc<AppServices>,
    books: Vec<BackgroundJobBook>,
    scope_label: String,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    // One background task window for the whole application: every window would
    // only drive the same shared job queue. A repeat request activates the live
    // window and retargets it at the new library scope.
    let key = background_jobs_window_key();
    match reserve_singleton_window(&key, cx) {
        SingletonWindowReservation::Activate(handle) => {
            if let Some(view) = existing_background_jobs_window(cx) {
                let _ = handle.update(cx, |_, window, cx| {
                    view.update(cx, |this, cx| {
                        this.apply_scope(books, scope_label, window, cx);
                    });
                    window.activate_window();
                });
            } else {
                activate_singleton_window(handle, cx);
            }
            return Ok(());
        }
        SingletonWindowReservation::InFlight => return Ok(()),
        SingletonWindowReservation::Reserved => {}
    }
    let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(900.), px(640.))),
            titlebar: Some(TitlebarOptions {
                title: Some("墨页 · 后台任务".into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio.background-jobs".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let jobs = cx.new(|cx| {
                BackgroundJobsWindow::new(Arc::clone(&services), books, scope_label, window, cx)
            });
            jobs.update(cx, |jobs, cx| jobs.refresh(window, cx));
            register_background_jobs_window(jobs.downgrade(), cx);
            on_window_close(window, cx, |_, _| true);
            cx.new(|cx| Root::new(jobs, window, cx))
        },
    );
    match opened {
        Ok(handle) => {
            complete_singleton_window(&key, handle.into(), cx);
            Ok(())
        }
        Err(error) => {
            release_singleton_window(&key, cx);
            Err(error).context("无法创建后台任务窗口")
        }
    }
}

fn job_kind_label(kind: &str) -> &'static str {
    match kind {
        "embedding" => "向量索引",
        "vision" => "视觉理解",
        "visual_render" => "逻辑页面渲染",
        "translation" => "图书翻译",
        _ => "未知任务",
    }
}

fn status_presentation(job: &BackgroundJobSnapshot) -> (&'static str, gpui::Rgba, gpui::Rgba) {
    if job.cancel_requested {
        return ("正在取消", rgb(0x8a5a20), rgb(0xf4e8d5));
    }
    if job.pause_requested {
        return ("正在暂停", rgb(0x8a5a20), rgb(0xf4e8d5));
    }
    match job.status {
        BackgroundJobStatus::Queued => ("排队中", rgb(0x5e6572), rgb(0xe7e9ed)),
        BackgroundJobStatus::Running => ("运行中", rgb(ACCENT_DARK), rgb(ACCENT_SOFT)),
        BackgroundJobStatus::Paused => ("已暂停", rgb(0x8a5a20), rgb(0xf4e8d5)),
        BackgroundJobStatus::Succeeded => ("已完成", rgb(0x376441), rgb(0xe3efe5)),
        BackgroundJobStatus::Failed => ("失败", rgb(DANGER), rgb(0xf8e2de)),
        BackgroundJobStatus::Cancelled => ("已取消", rgb(0x5e6572), rgb(0xe7e9ed)),
    }
}

fn available_actions(job: &BackgroundJobSnapshot) -> Vec<BackgroundJobAction> {
    if job.pause_requested || job.cancel_requested {
        return Vec::new();
    }
    let mut actions = match job.status {
        BackgroundJobStatus::Queued | BackgroundJobStatus::Running => {
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        }
        BackgroundJobStatus::Paused => {
            vec![BackgroundJobAction::Resume, BackgroundJobAction::Cancel]
        }
        BackgroundJobStatus::Failed | BackgroundJobStatus::Cancelled => {
            vec![BackgroundJobAction::Retry]
        }
        BackgroundJobStatus::Succeeded => Vec::new(),
    };
    // A completed translation can be re-run from the first block; other kinds
    // have no equivalent that would not discard published derived indexes.
    if job.kind == "translation" {
        actions.push(BackgroundJobAction::Retranslate);
    }
    actions
}

fn action_label(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "暂停",
        BackgroundJobAction::Resume => "恢复",
        BackgroundJobAction::Retry => "重试",
        BackgroundJobAction::Cancel => "取消",
        BackgroundJobAction::Retranslate => "重新翻译",
    }
}

fn action_verb(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "暂停",
        BackgroundJobAction::Resume => "恢复",
        BackgroundJobAction::Retry => "重试",
        BackgroundJobAction::Cancel => "取消",
        BackgroundJobAction::Retranslate => "重新翻译",
    }
}

fn action_id(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "pause",
        BackgroundJobAction::Resume => "resume",
        BackgroundJobAction::Retry => "retry",
        BackgroundJobAction::Cancel => "cancel",
        BackgroundJobAction::Retranslate => "retranslate",
    }
}

fn progress_label(job: &BackgroundJobSnapshot) -> String {
    let unit = match job.kind.as_str() {
        "embedding" => "分块",
        "vision" | "visual_render" => "页面",
        "translation" => "文本块",
        _ => "项",
    };
    match job.progress.total {
        Some(total) => format!(
            "进度 {} / {} {unit}",
            job.progress.completed.min(total),
            total
        ),
        None => format!("已处理 {} {unit}", job.progress.completed),
    }
}

/// Database-level status label (ignoring transient pause/cancel requests).
/// Mirrors the strings used by `index_jobs::IndexJobStatus` so users see the
/// actual durable state alongside the softer presentation label.
fn status_database_label(status: BackgroundJobStatus) -> &'static str {
    match status {
        BackgroundJobStatus::Queued => "queued",
        BackgroundJobStatus::Running => "running",
        BackgroundJobStatus::Paused => "paused",
        BackgroundJobStatus::Succeeded => "succeeded",
        BackgroundJobStatus::Failed => "failed",
        BackgroundJobStatus::Cancelled => "cancelled",
    }
}

/// Format a Unix timestamp (seconds) as `YYYY-MM-DD HH:MM:SS` in UTC without depending on `chrono` or `time` crates.
fn format_timestamp(unix_seconds: u64) -> String {
    format_utc_datetime(unix_seconds).unwrap_or_else(|| "—".to_string())
}

fn not_set() -> String {
    "—".to_string()
}

fn format_utc_datetime(unix_seconds: u64) -> Option<String> {
    let secs = i64::try_from(unix_seconds).ok()?;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (year, month, day) = civil_from_days(days)?;
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    ))
}

/// Howard Hinnant's date algorithm: convert Unix days since 1970-01-01 into a
/// proleptic Gregorian (year, month, day). Pure integer math, no leap-second
/// or calendar library required.
fn civil_from_days(days_since_epoch: i64) -> Option<(i32, u32, u32)> {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y } as i32;
    Some((year, m, d))
}

/// Pretty-print a small JSON cursor for inspection in the details panel.
/// Falls back to the raw string for non-object payloads or when the parser
/// itself can't make the result more readable.
fn prettify_cursor(cursor: &str) -> String {
    let parsed = match serde_json::from_str::<serde_json::Value>(cursor) {
        Ok(value) => value,
        Err(_) => return cursor.to_string(),
    };
    match serde_json::to_string_pretty(&parsed) {
        Ok(pretty) => pretty,
        Err(_) => cursor.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngy_book_studio::services::BackgroundJobProgress;

    fn job(status: BackgroundJobStatus) -> BackgroundJobSnapshot {
        BackgroundJobSnapshot {
            id: "job-1".to_string(),
            book_id: "book-1".to_string(),
            source_id: None,
            kind: "embedding".to_string(),
            status,
            pause_requested: false,
            cancel_requested: false,
            attempts: 2,
            progress: BackgroundJobProgress {
                completed: 3,
                total: Some(10),
            },
            error: None,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_500,
            started_at: Some(1_700_000_100),
            finished_at: None,
            cursor_json: None,
        }
    }

    #[test]
    fn actions_follow_the_durable_job_state_machine() {
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Queued)),
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Running)),
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Paused)),
            vec![BackgroundJobAction::Resume, BackgroundJobAction::Cancel]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Failed)),
            vec![BackgroundJobAction::Retry]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Cancelled)),
            vec![BackgroundJobAction::Retry]
        );
        assert!(available_actions(&job(BackgroundJobStatus::Succeeded)).is_empty());
    }

    #[test]
    fn task_filters_combine_kind_status_and_owning_book() {
        let snapshot = job(BackgroundJobStatus::Failed);
        let books = vec![
            BackgroundJobBook {
                id: "book-1".into(),
                title: "Rust 开发".into(),
            },
            BackgroundJobBook {
                id: "book-2".into(),
                title: "其他图书".into(),
            },
        ];
        assert!(job_matches(
            &snapshot,
            &books,
            "embedding",
            Some(BackgroundJobStatus::Failed),
            "rust"
        ));
        assert!(job_matches(&snapshot, &books, "", None, "job-1"));
        assert!(!job_matches(&snapshot, &books, "translation", None, ""));
        assert!(!job_matches(
            &snapshot,
            &books,
            "",
            Some(BackgroundJobStatus::Running),
            ""
        ));
        assert!(!job_matches(&snapshot, &books, "", None, "其他图书"));
        assert!(kind_matches("future-job", "other"));
        assert!(!kind_matches("translation", "other"));
    }

    #[test]
    fn copied_details_preserve_full_identifiers_and_failure_context() {
        let mut snapshot = job(BackgroundJobStatus::Failed);
        snapshot.id = "translation:0123456789abcdef0123456789abcdef:zh-Hans".into();
        snapshot.error = Some("HTTP 请求失败".into());
        snapshot.cursor_json = Some(r#"{"next_ordinal":17}"#.into());
        let copied = diagnostic_summary(&snapshot);
        assert!(copied.contains(&snapshot.id));
        assert!(copied.contains("HTTP 请求失败"));
        assert!(copied.contains("\"next_ordinal\": 17"));
        assert!(copied.contains("UTC"));
        let logs = diagnostic_log_copy(&snapshot, &BackgroundJobLogSnapshot::default());
        assert!(!logs.contains("HTTP 请求失败"));
        assert!(!logs.contains("next_ordinal"));
        assert!(logs.contains(&snapshot.id));
    }

    #[test]
    fn pending_control_hides_duplicate_actions_and_updates_status() {
        let mut pending = job(BackgroundJobStatus::Running);
        pending.pause_requested = true;
        assert!(available_actions(&pending).is_empty());
        assert_eq!(status_presentation(&pending).0, "正在暂停");

        pending.pause_requested = false;
        pending.cancel_requested = true;
        assert!(available_actions(&pending).is_empty());
        assert_eq!(status_presentation(&pending).0, "正在取消");
    }

    #[test]
    fn progress_is_bounded_by_the_latest_total() {
        let mut snapshot = job(BackgroundJobStatus::Running);
        assert_eq!(progress_label(&snapshot), "进度 3 / 10 分块");
        snapshot.progress.completed = 12;
        assert_eq!(progress_label(&snapshot), "进度 10 / 10 分块");
        snapshot.progress.total = None;
        assert_eq!(progress_label(&snapshot), "已处理 12 分块");
    }

    #[test]
    fn status_database_label_matches_durable_state() {
        assert_eq!(status_database_label(BackgroundJobStatus::Queued), "queued");
        assert_eq!(
            status_database_label(BackgroundJobStatus::Running),
            "running"
        );
        assert_eq!(status_database_label(BackgroundJobStatus::Paused), "paused");
        assert_eq!(
            status_database_label(BackgroundJobStatus::Succeeded),
            "succeeded"
        );
        assert_eq!(status_database_label(BackgroundJobStatus::Failed), "failed");
        assert_eq!(
            status_database_label(BackgroundJobStatus::Cancelled),
            "cancelled"
        );
    }

    #[test]
    fn timestamp_round_trips_through_civil_from_days() {
        // 1700000000 == 2023-11-14 22:13:20 UTC; the local-zone display only
        // affects HH:MM:SS so the date must be stable across machines.
        let formatted = format_timestamp(1_700_000_000);
        assert!(formatted.starts_with("2023-11-14 "));
        assert_eq!(formatted.len(), "YYYY-MM-DD HH:MM:SS".len());
    }

    #[test]
    fn prettify_cursor_falls_back_for_invalid_json() {
        assert_eq!(prettify_cursor("not json"), "not json");
        let pretty = prettify_cursor(r#"{"next_ordinal":42,"model":"qwen"}"#);
        assert!(pretty.contains("\"next_ordinal\""));
        assert!(pretty.contains("\"model\""));
    }

    fn detail_fixture(model: &str) -> TranslationBlockDetail {
        TranslationBlockDetail {
            ordinal: 7,
            unit_id: "unit-3".into(),
            unit_ordinal: 2,
            unit_title: "第三章".into(),
            unit_revision: 4,
            block_id: "unit-3::h5".into(),
            source_text: "Hello world".into(),
            segments: vec!["Hello".into(), "world".into()],
            request: TranslationBlockRequest {
                model: model.into(),
                target_language: "zh-Hans".into(),
                source_language: Some("en".into()),
                max_tokens: 4_096,
                temperature: 0.0,
                body: r#"{"model":"m"}"#.into(),
                system_prompt: "系统".into(),
                user_content: r#"{"source":"Hello world","segments":[]}"#.into(),
            },
        }
    }

    fn detail_state(tab: BlockDetailTab) -> BlockDetailState {
        BlockDetailState {
            job_id: "job".into(),
            ordinal: 7,
            generation: 0,
            tab,
            failed: false,
            loading: false,
            detail: None,
            error: None,
            probe: BlockProbeState::default(),
            scroll: ScrollHandle::default(),
        }
    }

    fn probe_fixture(response_text: &str) -> TranslationBlockProbe {
        TranslationBlockProbe {
            ordinal: 7,
            model: "chat-test".into(),
            response_text: response_text.into(),
            response_bytes: response_text.len(),
            end: "done",
            finish_reason: Some("stop"),
            decoded_segments: None,
            decode_error_kind: Some(JobLogErrorKind::SegmentCountMismatch),
            decode_error_line: None,
            decode_error_column: None,
            elapsed_ms: 1500,
        }
    }

    /// The response tab is the only one that is not part of the pinned payload,
    /// and the only one that must stay hidden for a block that translated fine.
    #[test]
    fn only_a_failed_block_offers_the_model_response_tab() {
        let healthy = block_detail_tabs(false);
        assert!(!healthy.contains(&BlockDetailTab::Response));
        assert_eq!(
            healthy,
            vec![
                BlockDetailTab::Overview,
                BlockDetailTab::Body,
                BlockDetailTab::Source,
                BlockDetailTab::Prompt,
                BlockDetailTab::Message,
            ]
        );

        let failed = block_detail_tabs(true);
        assert_eq!(failed.len(), healthy.len() + 1);
        assert_eq!(failed.last(), Some(&BlockDetailTab::Response));
        // Every tab keeps a distinct id and label, so a click can never land on
        // the wrong page.
        let ids: std::collections::BTreeSet<_> = failed.iter().map(|tab| tab.id()).collect();
        let labels: std::collections::BTreeSet<_> = failed.iter().map(|tab| tab.label()).collect();
        assert_eq!(ids.len(), failed.len());
        assert_eq!(labels.len(), failed.len());
    }

    /// A replayed answer is shown the way it arrived: a valid JSON object is
    /// indented, and anything the decoder rejects is shown byte for byte, because
    /// that broken shape is exactly what the user opened the tab to see.
    #[test]
    fn the_response_tab_shows_the_replayed_answer_without_cleaning_it() {
        // Nothing is shown before the user asks for a replay.
        assert!(block_detail_text(&detail_state(BlockDetailTab::Response), None).is_none());

        let mut state = detail_state(BlockDetailTab::Response);
        state.probe.result = Some(probe_fixture(r#"{"translations":[{"id":0,"text":"甲"}]}"#));
        let pretty = block_detail_text(&state, None).unwrap();
        assert!(pretty.contains('\n'), "a valid object is indented: {pretty}");
        assert!(serde_json::from_str::<serde_json::Value>(&pretty).is_ok());

        // A fenced answer is the classic weak-model failure; it must be visible
        // as it was written, fence and all.
        let fenced = "```json\n{\"translations\":[{\"id\":0,\"text\":\"甲\"}]}\n```";
        state.probe.result = Some(probe_fixture(fenced));
        assert_eq!(block_detail_text(&state, None).unwrap(), fenced);

        // So is a truncated object with no closing braces.
        let truncated = r#"{"translations":[{"id":0,"text":"甲""#;
        state.probe.result = Some(probe_fixture(truncated));
        assert_eq!(block_detail_text(&state, None).unwrap(), truncated);

        // The replayed text is copied verbatim, so the clipboard never carries a
        // request payload from another tab.
        assert_eq!(
            block_detail_text(&detail_state(BlockDetailTab::Source), None),
            None
        );
    }

    #[test]
    fn the_replay_panel_names_how_the_stream_ended_and_what_the_decoder_said() {
        let mut probe = probe_fixture("{}");
        assert_eq!(probe_end_label(&probe), "模型正常收尾");

        probe.end = "unexpected_finish_reason";
        probe.finish_reason = Some("length");
        assert_eq!(
            probe_end_label(&probe),
            "模型提前结束响应（finish_reason=length）"
        );

        probe.end = "stream_eof";
        assert_eq!(probe_end_label(&probe), "响应流在没有收尾标记的情况下关闭");

        probe.end = "size_limit";
        assert_eq!(probe_end_label(&probe), "响应超过大小上限，只显示已收到的部分");

        probe.end = "tool_call";
        assert_eq!(probe_end_label(&probe), "模型意外请求了工具");

        // The verdict comes from the worker's own decoder, labelled from the
        // closed category table rather than re-derived in the window.
        probe.decode_error_kind = Some(JobLogErrorKind::SegmentCountMismatch);
        probe.decode_error_line = Some(3);
        probe.decode_error_column = Some(12);
        assert_eq!(
            probe_decode_label(&probe),
            "严格解码失败：片段数量与原文不一致（第 3 行第 12 列）"
        );

        probe.decode_error_kind = Some(JobLogErrorKind::IncompleteJson);
        probe.decode_error_line = None;
        probe.decode_error_column = None;
        assert_eq!(probe_decode_label(&probe), "严格解码失败：响应 JSON 不完整");

        probe.decode_error_kind = None;
        probe.decoded_segments = Some(2);
        assert_eq!(probe_decode_label(&probe), "严格解码通过：2 个片段");

        // A replay that produced no answer at all still says what the model did.
        let mut empty = probe_fixture("");
        empty.end = "unexpected_finish_reason";
        empty.finish_reason = None;
        assert_eq!(
            probe_end_label(&empty),
            "模型提前结束响应（finish_reason=other）"
        );
    }

    #[test]
    fn block_detail_tabs_show_the_payload_and_hide_nothing_behind_the_preview() {
        let detail = detail_fixture("chat-test");
        let source =
            block_detail_text(&detail_state(BlockDetailTab::Source), Some(&detail)).unwrap();
        // The whole block, not the 96-character preview the list holds.
        assert_eq!(source, "Hello world");
        assert_eq!(
            block_detail_text(&detail_state(BlockDetailTab::Prompt), Some(&detail)).unwrap(),
            "系统"
        );

        let body = block_detail_text(&detail_state(BlockDetailTab::Body), Some(&detail)).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());

        // A body that is not JSON must still be shown verbatim rather than
        // rendering an empty panel.
        let mut broken = detail.clone();
        broken.request.body = "<html>error page</html>".into();
        let body = block_detail_text(&detail_state(BlockDetailTab::Body), Some(&broken)).unwrap();
        assert_eq!(body, "<html>error page</html>");

        let overview =
            block_detail_text(&detail_state(BlockDetailTab::Overview), Some(&detail)).unwrap();
        assert!(overview.contains("#7"));
        assert!(overview.contains("unit-3::h5"));
        assert!(overview.contains("chat-test"));
        assert!(overview.contains("中文（简体）"));
        assert!(overview.contains("5, 5"), "per-segment character counts");
        assert!(overview.contains("原文语言标记: en"));

        // A task that never ran records no model; the overview must say so
        // instead of printing an empty field.
        let empty_model = block_detail_text(
            &detail_state(BlockDetailTab::Overview),
            Some(&detail_fixture("")),
        )
        .unwrap();
        assert!(empty_model.contains("调用模型: \n"));

        // Nothing is rendered while the payload is missing.
        for tab in block_detail_tabs(true) {
            assert!(block_detail_text(&detail_state(tab), None).is_none());
        }
    }
}
