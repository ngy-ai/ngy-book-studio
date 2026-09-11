use super::*;

use gpui::{ClipboardItem, DragMoveEvent, EmptyView, Pixels, ScrollHandle};

#[cfg(test)]
mod layout_tests;
use moye_epub_editor::job_diagnostics::BackgroundJobLogSnapshot;
use moye_epub_editor::services::{
    AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
    TranslationBlockInfo, TranslationBlockList, translation_language_label,
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

/// Whether one translatable block was already processed, is being translated
/// right now, or is still waiting. Derived from the live durable cursor so the
/// inspector never needs a second source of truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockState {
    Done,
    Processing,
    Pending,
}

impl BlockState {
    fn label(self) -> &'static str {
        match self {
            Self::Done => "已处理",
            Self::Processing => "处理中",
            Self::Pending => "待处理",
        }
    }

    fn colors(self) -> (gpui::Rgba, gpui::Rgba) {
        match self {
            Self::Done => (rgb(0x376441), rgb(0xe3efe5)),
            Self::Processing => (rgb(ACCENT_DARK), rgb(ACCENT_SOFT)),
            Self::Pending => (rgb(0x5e6572), rgb(0xe7e9ed)),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Processing => "processing",
            Self::Pending => "pending",
        }
    }
}

/// Live translation counters for one job. `done` mirrors the durable cursor:
/// the block at the cursor is the one the running worker is translating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TranslationProgress {
    total: usize,
    done: usize,
    processing: usize,
    pending: usize,
    running: bool,
}

impl TranslationProgress {
    fn count(self, state: BlockState) -> usize {
        match state {
            BlockState::Done => self.done,
            BlockState::Processing => self.processing,
            BlockState::Pending => self.pending,
        }
    }

    fn filtered(self, filter: Option<BlockState>) -> usize {
        match filter {
            Some(state) => self.count(state),
            None => self.total,
        }
    }
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

    fn refresh_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab != DetailTab::Logs || self.logs_loading {
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
        let mut actions = div().h_flex().gap_1().flex_wrap();
        for action in available_actions(job) {
            let id = job.id.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!(
                    "background-job-{}-{}",
                    job.id,
                    action_id(action)
                )))
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
            DetailTab::Blocks => self
                .blocks
                .as_ref()
                .map(|blocks| translation_blocks_copy(job, blocks)),
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
            for row in self.translation_block_rows(job, cx) {
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
        let Some(blocks) = &self.blocks else {
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

        let progress = translation_progress(job, blocks.total);
        rows.push(
            div()
                .h_flex()
                .flex_wrap()
                .gap_2()
                .text_color(rgb(INK))
                .child(format!(
                    "共 {} 个文本块 · 已处理 {} · 处理中 {} · 待处理 {}",
                    progress.total, progress.done, progress.processing, progress.pending
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
                format!("处理中 {}", progress.processing),
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

        let filtered = progress.filtered(self.blocks_state);
        let pages = filtered.div_ceil(BLOCKS_PER_PAGE).max(1);
        let page = self.blocks_page.min(pages - 1);
        let indices = block_page_indices(blocks, progress, self.blocks_state, page);
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
            let state = translation_block_state(index, progress.done, progress.running);
            let (foreground, background) = state.colors();
            rows.push(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
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
                            this.blocks_page = (progress.done / BLOCKS_PER_PAGE).min(pages - 1);
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        rows
    }
}

impl Render for BackgroundJobsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_width = window.viewport_size().width;
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

fn tab_id(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Summary => "summary",
        DetailTab::Logs => "logs",
        DetailTab::Blocks => "blocks",
    }
}

/// Which block the durable cursor currently points at. `done` counts the
/// processed blocks, so the block at that index is the one the running worker
/// is translating; that is the only block without a final state.
fn translation_block_state(index: usize, done: usize, running: bool) -> BlockState {
    if index < done {
        BlockState::Done
    } else if running && index == done {
        BlockState::Processing
    } else {
        BlockState::Pending
    }
}

/// Live counters of one translation task. `completed` is the durable cursor,
/// so clamping it against the block count keeps stale snapshots consistent.
fn translation_progress(job: &BackgroundJobSnapshot, total: usize) -> TranslationProgress {
    let done = job.progress.completed.min(total);
    let running = job.status == BackgroundJobStatus::Running && done < total;
    let processing = usize::from(running);
    TranslationProgress {
        total,
        done,
        processing,
        pending: total - done - processing,
        running,
    }
}

/// Indices of the blocks on one page of the inspector. The scan stops as soon
/// as the page is filled, so a large book never materializes every match.
fn block_page_indices(
    blocks: &TranslationBlockList,
    progress: TranslationProgress,
    filter: Option<BlockState>,
    page: usize,
) -> Vec<usize> {
    let start = page.saturating_mul(BLOCKS_PER_PAGE);
    let end = start.saturating_add(BLOCKS_PER_PAGE);
    let mut matched = 0usize;
    let mut indices = Vec::new();
    for index in 0..blocks.total {
        let state = translation_block_state(index, progress.done, progress.running);
        if filter.is_some_and(|filter| filter != state) {
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

fn block_chapter_label(block: &TranslationBlockInfo) -> String {
    if block.unit_title.trim().is_empty() {
        format!("第 {} 章", block.unit_ordinal + 1)
    } else {
        block.unit_title.clone()
    }
}

fn translation_blocks_copy(job: &BackgroundJobSnapshot, blocks: &TranslationBlockList) -> String {
    let progress = translation_progress(job, blocks.total);
    [
        format!("任务 ID: {}", job.id),
        format!("图书 ID: {}", job.book_id),
        format!(
            "目标语言: {}",
            translation_language_label(&blocks.target_language)
                .unwrap_or(blocks.target_language.as_str())
        ),
        format!(
            "文本块: 共 {} · 已处理 {} · 处理中 {} · 待处理 {}",
            progress.total, progress.done, progress.processing, progress.pending
        ),
        "序号与任务游标一致，从 0 开始。".to_string(),
    ]
    .join("\n")
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
            app_id: Some("dev.moye.epub-editor.background-jobs".to_string()),
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
    use moye_epub_editor::services::BackgroundJobProgress;

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
}
