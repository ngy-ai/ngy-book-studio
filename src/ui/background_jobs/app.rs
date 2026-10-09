//! Construction, filtering, resizing and refreshing the job data.

use super::*;

impl BackgroundJobsWindow {
    pub(super) fn new(
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
                cx.background_executor().timer(Duration::from_secs(2)).await;
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

    pub(super) fn filtered_jobs(&self) -> Vec<&BackgroundJobSnapshot> {
        let query = self.query.trim().to_lowercase();
        self.jobs
            .iter()
            .filter(|job| job_matches(job, &self.books, self.kind, self.status, &query))
            .collect()
    }

    pub(super) fn reconcile_selection(&mut self) {
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
    pub(super) fn clear_detail_cache(&mut self) {
        self.detail_scroll
            .set_offset(gpui_kit::point(px(0.), px(0.)));
        self.logs_generation = self.logs_generation.wrapping_add(1);
        self.logs_loading = false;
        self.logs = None;
        self.logs_error = None;
        self.clear_blocks();
    }

    pub(super) fn clear_blocks(&mut self) {
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
    pub(super) fn maximum_list_width(&self, viewport_width: Pixels) -> f32 {
        let viewport = f32::from(viewport_width);
        if !viewport.is_finite() {
            return JOBS_LIST_MAX_WIDTH;
        }
        (viewport - JOBS_DETAIL_MIN_WIDTH - JOBS_RESIZE_HANDLE_WIDTH)
            .clamp(JOBS_LIST_MIN_WIDTH, JOBS_LIST_MAX_WIDTH)
    }

    /// Width the list column renders with. Clamping here (not only while
    /// dragging) keeps a shrinking window from squeezing the detail panel.
    pub(super) fn effective_list_width(&self, viewport_width: Pixels) -> f32 {
        self.list_width
            .clamp(JOBS_LIST_MIN_WIDTH, self.maximum_list_width(viewport_width))
    }

    pub(super) fn begin_list_resize(&mut self) -> bool {
        if self.resizing_list {
            return false;
        }
        self.resizing_list = true;
        true
    }

    pub(super) fn finish_list_resize(&mut self) -> bool {
        std::mem::take(&mut self.resizing_list)
    }

    /// Follows the divider with the pointer. The pointer sits on the handle
    /// center, so half the handle belongs to the list column on its left.
    pub(super) fn resize_list_from_pointer(
        &mut self,
        pointer_x: Pixels,
        viewport_width: Pixels,
    ) -> bool {
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

    pub(super) fn filters_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = 0;
        self.list_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.reconcile_selection();
        self.refresh_logs(window, cx);
        self.refresh_blocks(window, cx);
        cx.notify();
    }

    pub(super) fn apply_scope(
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
        self.list_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.loaded = false;
        self.loading = false;
        self.notice = None;
        self.refresh_error = None;
        self.refresh(window, cx);
    }

    pub(super) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn refresh_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn refresh_blocks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                this.detail_scroll
                    .set_offset(gpui_kit::point(px(0.), px(0.)));
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
    pub(super) fn open_block_detail(
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
    pub(super) fn probe_block_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn load_block_detail(
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
                detail.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
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
}
