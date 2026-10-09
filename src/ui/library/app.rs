//! Construction and the window-level requests the library forwards to other
//! windows.

use super::*;

impl EpubReaderApp {
    pub fn new(
        library: LibraryStore,
        services: Arc<AppServices>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前范围的正文…"));
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let ai_sidebar = cx.new(|cx| {
            AiSidebar::new(
                library_ai_scope(&library, &GroupFilter::All),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        let mut ai_controller =
            AiSidebarController::new(Arc::clone(&services), ChatWindowKind::Library, None)
                .expect("library AI scope is valid");
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        ai_controller.restore(ai_sidebar.clone(), cx);
        let library_projection_generation = services.library_generation();
        let office_book_ids = library
            .books()
            .iter()
            .filter(|book| is_office_format_name(&book.format))
            .map(|book| book.id.clone())
            .collect::<Vec<_>>();
        let office_services = Arc::clone(&services);
        let office_state_task = cx.spawn(async move |view, cx| {
            let mut enabled = HashSet::new();
            for book_id in office_book_ids {
                match office_services
                    .office_enhancement_enabled(book_id.clone())
                    .await
                {
                    Ok(true) => {
                        enabled.insert(book_id);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(%error, %book_id, "cannot load Office enhancement setting");
                    }
                }
            }
            let _ = view.update(cx, |this, cx| {
                if this.office_settings_generation == 0 {
                    this.office_enabled_books = enabled;
                    cx.notify();
                }
            });
        });
        Self {
            library,
            services,
            library_window: gpui_kit::Window::window_handle(window),
            library_mutations: LibraryMutationLifecycle::default(),
            ai_sidebar,
            ai_controller,
            search_input,
            search_query: String::new(),
            search_mode: SearchMode::Hybrid,
            search_results: Vec::new(),
            search_generation: 0,
            search_results_generation: None,
            search_loading: false,
            search_error: None,
            _search_subscription,
            _ai_subscription,
            notice: None,
            is_importing: false,
            library_projection_generation,
            library_request_generation: 0,
            library_notice_request: None,
            group_filter_generation: 0,
            office_preview_generation: 0,
            office_preview: None,
            office_enabled_books: HashSet::new(),
            office_settings_generation: 0,
            _office_state_task: office_state_task,
            group_filter: GroupFilter::All,
            collapsed_groups: HashSet::new(),
            book_sort: BookSortField::default(),
            book_sort_direction: BookSortField::default().default_direction(),
            book_view: BookViewMode::default(),
            group_modal: None,
        }
    }

    pub(super) fn on_ai_sidebar_event(
        &mut self,
        _sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                self.ai_controller
                    .submit(request.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::Cancel { request_id } => self.ai_controller.cancel(*request_id),
            AiSidebarEvent::NewSession => {
                self.ai_controller.new_session(self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::SwitchSession { thread_id } => {
                self.ai_controller
                    .switch_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::DeleteSession { thread_id } => {
                self.ai_controller
                    .delete_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::DeleteTurn { user_db_id } => {
                self.ai_controller
                    .delete_turn(user_db_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::ScopeChanged { book_ids } => {
                self.ai_controller
                    .reconcile_scope(book_ids.clone(), self.ai_sidebar.clone(), cx)
            }
            AiSidebarEvent::OpenSource(source) => self.open_ai_source(source.clone(), window, cx),
        }
    }

    pub(super) fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            current_canonical_source_unit_index(&lookup, &document).map_err(anyhow::Error::msg)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) => this.open_book_at_source(source, Some(index), window, cx),
                    Ok(Err(error)) => this.set_error(format!("引用已失效：{error:#}"), cx),
                    Err(error) => this.set_error(format!("引用定位任务已停止：{error}"), cx),
                });
            });
        })
        .detach();
    }

    /// 打开五个 AI 设置窗口里的一个；每个窗口各自保存自己那一块配置。
    pub(super) fn open_ai_settings(&mut self, section: AiSettingsSection, cx: &mut Context<Self>) {
        if let Err(error) = open_ai_settings_window(Arc::clone(&self.services), section, cx) {
            self.set_error(
                format!("无法打开「{}」设置：{error:#}", section.label()),
                cx,
            );
        }
    }

    pub(super) fn open_learning(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = open_learning_window(Arc::clone(&self.services), cx) {
            self.set_error(format!("无法打开学习中心：{error:#}"), cx);
        }
    }

    pub(super) fn open_all_notes(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = open_notes_window(Arc::clone(&self.services), cx.entity(), None, cx) {
            self.set_error(format!("无法打开全部笔记：{error:#}"), cx);
        }
    }

    pub(super) fn open_background_jobs(&mut self, cx: &mut Context<Self>) {
        let indices = self.visible_book_indices();
        let books = indices
            .iter()
            .map(|index| {
                let book = &self.library.books()[*index];
                BackgroundJobBook {
                    id: book.id.clone(),
                    title: book.title.clone(),
                }
            })
            .collect();
        let (scope_label, _) = self.group_heading(indices.len());
        if let Err(error) =
            open_background_jobs_window(Arc::clone(&self.services), books, scope_label, cx)
        {
            self.set_error(format!("无法打开后台任务：{error:#}"), cx);
        }
    }

    pub(super) fn begin_library_request(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        if !self.library_mutations.begin() {
            return None;
        }
        self.library_request_generation = self.library_request_generation.wrapping_add(1).max(1);
        self.library_notice_request = Some(self.library_request_generation);
        self.notice = Some(Notice {
            text: message.into(),
            error: false,
        });
        cx.notify();
        Some(self.library_request_generation)
    }

    pub(super) fn finish_library_request(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyWindowHandle> {
        let should_close = self.library_mutations.finish();
        cx.notify();
        should_close.then_some(self.library_window)
    }

    pub(super) fn request_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.library_mutations.close_requested() {
            // Invalidate every in-flight completion before mutation callbacks
            // can refresh the projection. Those callbacks may still finish,
            // but must not revive a search while this window is closing.
            self.search_generation = self.search_generation.wrapping_add(1);
            self.search_loading = false;
            self.search_results_generation = None;
        }
        self.cancel_ai_for_window_close(cx);
        self.group_modal = None;
        let should_close = self.library_mutations.request_close();
        cx.notify();
        should_close
    }

    /// Merges the projection captured under the service mutation lock. Service
    /// generations reflect actual commit order, while `merge_cached_projection`
    /// preserves a newer editor revision already applied to this window.
    pub(super) fn apply_shared_library_projection(
        &mut self,
        generation: u64,
        snapshot: LibraryStore,
    ) -> bool {
        if !should_apply_library_projection(generation, self.library_projection_generation) {
            return false;
        }
        self.library.merge_cached_projection(snapshot);
        self.library_projection_generation = generation;
        self.collapsed_groups
            .retain(|group_id| self.library.group(group_id).is_some());
        if self
            .group_filter
            .group_id()
            .is_some_and(|group_id| self.library.group(group_id).is_none())
        {
            self.group_filter = GroupFilter::All;
            self.group_filter_generation = self.group_filter_generation.wrapping_add(1);
        }
        true
    }
}
