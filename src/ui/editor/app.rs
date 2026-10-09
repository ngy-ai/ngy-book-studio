//! The editor application: construction, pane layout, AI integration and
//! the input events the view forwards.

use super::*;

/// Editor window root: lets the user rewrite the title, author, chapter titles
/// and chapter XHTML of a book, and add new chapters.
pub struct EditorApp {
    pub(crate) book_id: String,
    pub(crate) library: LibraryStore,
    pub(crate) library_projection_generation: u64,
    pub(crate) services: Arc<AppServices>,
    pub(crate) library_view: WeakEntity<EpubReaderApp>,
    pub(crate) ai_sidebar: Entity<AiSidebar>,
    pub(crate) ai_sidebar_collapsed: bool,
    pub(crate) left_sidebar_width: Pixels,
    pub(crate) right_sidebar_width: Pixels,
    pub(crate) pane_viewport_width: Pixels,
    pub(crate) ai_controller: AiSidebarController,
    pub(crate) pending_ai_request: Option<AiQuestionRequest>,
    pub(crate) pending_citation_source: Option<AiSourceLink>,
    pub(crate) pending_citation_navigation: Option<PendingEditorCitationNavigation>,
    pub(crate) cover: Option<CoverDraft>,
    pub(crate) cover_preview: Option<Arc<Image>>,
    pub(crate) cover_dirty: bool,
    pub(crate) cover_loading: bool,
    pub(crate) pending_cover_action: Option<PendingEditorAction>,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) search_query: String,
    pub(crate) search_results: Vec<EditorSearchHit>,
    pub(crate) _search_subscriptions: Vec<Subscription>,
    /// 皮肤变化时把新配色重新注入编辑器文档。
    pub(crate) _theme_subscription: Subscription,
    pub(crate) title_input: Entity<InputState>,
    pub(crate) author_input: Entity<InputState>,
    pub(crate) chapter_title_input: Entity<InputState>,
    pub(crate) body_input: Entity<InputState>,
    pub(crate) chapters: Vec<EditorChapter>,
    pub(crate) canonical_document: Option<BookDocument>,
    pub(crate) unit_states: Vec<EditorUnitState>,
    /// Chapter IDs whose source has been edited (source tab or rich text)
    /// since the last load or successful save. Used to skip pointless
    /// re-serialization when toggling the source kind on a pristine chapter.
    pub(crate) modified_chapter_ids: HashSet<String>,
    pub(crate) pending_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    pub(crate) media_loading: bool,
    pub(crate) media_modal: Option<EditorMediaModal>,
    pub(crate) media_target: Option<EditorMediaBlock>,
    pub(crate) pending_media_action: Option<PendingEditorAction>,
    pub(crate) selected: usize,
    pub(crate) selected_toc_id: Option<String>,
    pub(crate) pending_toc_id: Option<String>,
    pub(crate) tab: EditorTab,
    pub(crate) editor_webview: Option<Entity<WebView>>,
    pub(crate) web_state: EditorWebState,
    pub(crate) web_revision: u64,
    pub(crate) active_web_page: Option<ActiveEditorPage>,
    pub(crate) ai_selected_text: Option<String>,
    pub(crate) snapshot_request: u64,
    pub(crate) pending_snapshot: Option<PendingEditorSnapshot>,
    pub(crate) pending_ready_action: Option<PendingEditorAction>,
    pub(crate) export_dialog_open: bool,
    pub(crate) pending_export_path: Option<PathBuf>,
    pub(crate) active_write: Option<ActiveEditorWrite>,
    pub(crate) write_sequence: u64,
    pub(crate) close_after_write: bool,
    pub(crate) close_prompt_open: bool,
    pub(crate) close_confirmation_pending: bool,
    pub(crate) close_without_saving_pending: bool,
    pub(crate) draft_generation: u64,
    pub(crate) webview_build_gate: EditorWebViewBuildGate,
    pub(crate) ipc_sync_task: Option<Task<()>>,
    pub(crate) ready_timeout_task: Option<Task<()>>,
    pub(crate) closing_webview: Option<WeakEntity<WebView>>,
    pub(crate) closing: bool,
    /// Set when the window closes because its book left the library: unsaved
    /// edits belong to a document that no longer exists.
    pub(crate) closing_for_removed_book: bool,
    pub(crate) removal_scheduled: bool,
    pub(crate) notice: Option<Notice>,
}

impl EditorApp {
    pub fn new(
        book_id: String,
        library: LibraryStore,
        library_projection_generation: u64,
        services: Arc<AppServices>,
        library_view: WeakEntity<EpubReaderApp>,
        cover: Option<CoverDraft>,
        title_input: Entity<InputState>,
        author_input: Entity<InputState>,
        chapter_title_input: Entity<InputState>,
        body_input: Entity<InputState>,
        chapters: Vec<EditorChapter>,
        document: BookDocument,
        web_state: EditorWebState,
        webview_building: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前图书正文…"));
        let mut _search_subscriptions = vec![
            cx.subscribe_in(&search_input, window, Self::on_search_input_event),
            cx.subscribe_in(&title_input, window, Self::on_book_title_input_event),
            cx.subscribe_in(&author_input, window, Self::on_author_input_event),
            cx.subscribe_in(
                &chapter_title_input,
                window,
                Self::on_chapter_title_input_event,
            ),
            cx.subscribe_in(&body_input, window, Self::on_body_input_event),
        ];
        // 皮肤是应用级偏好：设置窗口保存后全局状态变一次，已经打开的编辑器（含
        // 富文本工具栏）要立刻换成新配色。
        let _theme_subscription =
            cx.observe_global::<ngy_book_studio::theme::AppTheme>(|this, cx| {
                this.apply_webview_appearance(cx);
            });
        let cover_preview = cover.as_ref().and_then(|cover| {
            image_format_from_mime(cover.mime())
                .map(|format| Arc::new(Image::from_bytes(format, (**cover.bytes()).clone())))
        });
        let canonical_document = if document.id == book_id && document.units.len() == chapters.len()
        {
            Some(document)
        } else {
            tracing::warn!(
                book_id,
                canonical_book_id = document.id,
                canonical_units = document.units.len(),
                editor_chapters = chapters.len(),
                "canonical document and editor projection are not aligned"
            );
            None
        };
        let unit_states = canonical_document
            .as_ref()
            .map(EditorUnitState::from_document)
            .unwrap_or_default();
        if let (Some(chapter), Some(unit)) = (chapters.first(), unit_states.first()) {
            web_state.set(
                0,
                unit.id.clone(),
                chapter.href.clone(),
                chapter.html.clone(),
            );
        }
        if let Some(source) = unit_states.first().map(|unit| unit.source.clone()) {
            body_input.update(cx, |state, cx| {
                state.set_value(source, window, cx);
            });
        }
        let current_book =
            AiBookOption::new(book_id.clone(), title_input.read(cx).value().to_string());
        let available_books = library
            .books()
            .iter()
            .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
            .collect();
        let ai_sidebar = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(current_book, available_books),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        let references =
            editor_reference_hints(&book_id, &chapters, &unit_states, 0, None, None, 0);
        ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
        let mut ai_controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Editor,
            Some(book_id.clone()),
        )
        .expect("editor AI scope is valid");
        _search_subscriptions.push(cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event));
        let ai_sidebar_collapsed = ai_sidebar.read(cx).is_collapsed();
        _search_subscriptions.push(cx.observe(&ai_sidebar, |this, sidebar, cx| {
            let collapsed = sidebar.read(cx).is_collapsed();
            if this.ai_sidebar_collapsed != collapsed {
                this.ai_sidebar_collapsed = collapsed;
                this.constrain_pane_widths(this.pane_viewport_width, cx);
                cx.notify();
            }
        }));
        _search_subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.pane_viewport_width = window.viewport_size().width;
            this.constrain_pane_widths(this.pane_viewport_width, cx);
        }));
        ai_controller.restore(ai_sidebar.clone(), cx);
        Self {
            book_id,
            library,
            library_projection_generation,
            services,
            library_view,
            ai_sidebar,
            ai_sidebar_collapsed,
            left_sidebar_width: px(EDITOR_LEFT_SIDEBAR_DEFAULT_WIDTH),
            right_sidebar_width: px(AI_SIDEBAR_WIDTH),
            pane_viewport_width: window.viewport_size().width,
            ai_controller,
            pending_ai_request: None,
            pending_citation_source: None,
            pending_citation_navigation: None,
            cover,
            cover_preview,
            cover_dirty: false,
            cover_loading: false,
            pending_cover_action: None,
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            _search_subscriptions,
            _theme_subscription,
            title_input,
            author_input,
            chapter_title_input,
            body_input,
            chapters,
            canonical_document,
            unit_states,
            modified_chapter_ids: HashSet::new(),
            pending_asset_bytes: HashMap::new(),
            media_loading: false,
            media_modal: None,
            media_target: None,
            pending_media_action: None,
            selected: 0,
            selected_toc_id: None,
            pending_toc_id: None,
            tab: EditorTab::Source,
            editor_webview: None,
            web_state,
            web_revision: 0,
            active_web_page: None,
            ai_selected_text: None,
            snapshot_request: 0,
            pending_snapshot: None,
            pending_ready_action: None,
            export_dialog_open: false,
            pending_export_path: None,
            active_write: None,
            write_sequence: 0,
            close_after_write: false,
            close_prompt_open: false,
            close_confirmation_pending: false,
            close_without_saving_pending: false,
            draft_generation: 0,
            webview_build_gate: EditorWebViewBuildGate::new(webview_building),
            ipc_sync_task: None,
            ready_timeout_task: None,
            closing_webview: None,
            closing: false,
            closing_for_removed_book: false,
            removal_scheduled: false,
            notice: None,
        }
    }

    pub(crate) fn resize_pane_from_pointer(
        &mut self,
        pane: EditorResizablePane,
        pointer_x: Pixels,
        viewport_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        match pane {
            EditorResizablePane::Left => {
                let right_width = if self.ai_sidebar_collapsed {
                    px(AI_SIDEBAR_COLLAPSED_WIDTH)
                } else {
                    self.right_sidebar_width
                };
                let width = constrained_editor_pane_width(
                    editor_pane_width_from_pointer(pane, pointer_x, viewport_width),
                    px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
                    px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
                    viewport_width,
                    right_width,
                );
                if self.left_sidebar_width != width {
                    self.left_sidebar_width = width;
                    cx.notify();
                }
            }
            EditorResizablePane::Right => {
                let width = constrained_editor_pane_width(
                    editor_pane_width_from_pointer(pane, pointer_x, viewport_width),
                    px(AI_SIDEBAR_MIN_WIDTH),
                    px(AI_SIDEBAR_MAX_WIDTH),
                    viewport_width,
                    self.left_sidebar_width,
                );
                if self.right_sidebar_width != width {
                    self.right_sidebar_width = width;
                    self.ai_sidebar.update(cx, |sidebar, cx| {
                        sidebar.set_expanded_width(width, cx);
                    });
                    cx.notify();
                }
            }
        }
    }

    pub(crate) fn constrain_pane_widths(&mut self, viewport_width: Pixels, cx: &mut Context<Self>) {
        let collapsed = self.ai_sidebar_collapsed;
        let occupied_right_width = if collapsed {
            px(AI_SIDEBAR_COLLAPSED_WIDTH)
        } else {
            self.right_sidebar_width
        };
        let left_width = constrained_editor_pane_width(
            self.left_sidebar_width,
            px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
            viewport_width,
            occupied_right_width,
        );
        let right_width = if collapsed {
            self.right_sidebar_width
        } else {
            constrained_editor_pane_width(
                self.right_sidebar_width,
                px(AI_SIDEBAR_MIN_WIDTH),
                px(AI_SIDEBAR_MAX_WIDTH),
                viewport_width,
                left_width,
            )
        };
        let left_changed = self.left_sidebar_width != left_width;
        let right_changed = self.right_sidebar_width != right_width;
        if left_changed {
            self.left_sidebar_width = left_width;
        }
        if right_changed {
            self.right_sidebar_width = right_width;
            self.ai_sidebar.update(cx, |sidebar, cx| {
                sidebar.set_expanded_width(right_width, cx);
            });
        }
        if left_changed || right_changed {
            cx.notify();
        }
    }

    pub(crate) fn on_ai_sidebar_event(
        &mut self,
        _sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                self.pending_ai_request = Some(request.clone());
                self.begin_action(PendingEditorAction::AskAi, window, cx);
            }
            AiSidebarEvent::Cancel { request_id } => {
                if self
                    .pending_ai_request
                    .as_ref()
                    .is_some_and(|request| request.request_id == *request_id)
                {
                    self.pending_ai_request = None;
                    if self
                        .pending_snapshot
                        .as_ref()
                        .is_some_and(|pending| pending.action == PendingEditorAction::Close)
                        || self.pending_ready_action == Some(PendingEditorAction::Close)
                    {
                        cancel_application_exit(cx);
                    }
                    self.pending_snapshot = None;
                    self.pending_ready_action = None;
                }
                self.ai_controller.cancel(*request_id);
            }
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

    pub(crate) fn perform_ai_question(&mut self, cx: &mut Context<Self>) {
        let Some(mut request) = self.pending_ai_request.take() else {
            return;
        };
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            self.ai_sidebar.update(cx, |sidebar, cx| {
                sidebar.fail_answer(
                    request.request_id,
                    "正文源码尚未通过解析和清洗，未发送可能过期的 AI 引用。",
                    cx,
                );
            });
            return;
        }
        // Rich text reaches this point only after the exact
        // session/chapter/revision/request-id snapshot acknowledgement. Source
        // mode was flushed above. Freeze every selected chapter from this editor
        // session so unsaved edits in previously visited chapters cannot be
        // replaced by older SQLite content while the model is answering.
        freeze_editor_reference_hints(
            &self.book_id,
            &self.chapters,
            &self.unit_states,
            self.selected,
            self.ai_selected_text.as_deref(),
            self.web_revision,
            &mut request.reference_hints,
        );
        let current_unit_id = self
            .unit_states
            .get(self.selected)
            .map(|unit| unit.id.as_str())
            .or_else(|| {
                self.chapters
                    .get(self.selected)
                    .map(|chapter| chapter.href.as_str())
            });
        request.reference = current_unit_id.and_then(|unit_id| {
            request
                .reference_hints
                .iter()
                .find(|reference| reference.book_id == self.book_id && reference.unit_id == unit_id)
                .cloned()
        });
        self.ai_controller
            .submit(request, self.ai_sidebar.clone(), cx);
    }

    pub(crate) fn fail_pending_ai(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let Some(request) = self.pending_ai_request.take() else {
            return;
        };
        let message = message.into();
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.fail_answer(request.request_id, message, cx);
        });
    }

    pub(crate) fn set_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: message.into(),
            error: true,
        });
        cx.notify();
    }

    pub(crate) fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            lookup
                .current_unit_index(&document)
                .context("引用对应的文档或内容版本已失效")
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) if source.book_id == this.book_id => {
                        this.pending_citation_source = Some(source);
                        this.begin_action(PendingEditorAction::OpenCitation(index), window, cx)
                    }
                    Ok(Ok(index)) => {
                        if let Some(library_view) = this.library_view.upgrade() {
                            library_view.update(cx, |library, cx| {
                                library.open_book_at_source(source, Some(index), window, cx);
                            });
                        }
                    }
                    Ok(Err(error)) => this.set_error(format!("引用已失效：{error:#}"), cx),
                    Err(error) => this.set_error(format!("引用定位任务已停止：{error}"), cx),
                });
            });
        })
        .detach();
    }

    pub(crate) fn on_book_title_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.draft_generation = self.draft_generation.wrapping_add(1);
            self.sync_ai_scope(cx);
        }
    }

    pub(crate) fn on_author_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.draft_generation = self.draft_generation.wrapping_add(1);
            cx.notify();
        }
    }

    pub(crate) fn on_search_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                // Source mode has no IPC bridge, so copy both editable fields
                // into the in-memory chapter before searching. Rich text emits
                // its latest body on blur; `apply_ipc_message` refreshes active
                // results when that snapshot arrives.
                self.flush_chapter_title(cx);
                if self.tab == EditorTab::Source {
                    let _ = self.flush_body(cx);
                }
                self.search_query = input.read(cx).value().trim().to_string();
                self.refresh_editor_search(cx);
            }
            InputEvent::PressEnter { .. } => {
                if let Some(chapter_index) =
                    self.search_results.first().map(|hit| hit.chapter_index)
                {
                    self.select_chapter(chapter_index, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn on_chapter_title_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.draft_generation = self.draft_generation.wrapping_add(1);
            self.sync_ai_reference(cx);
            if self.search_query.is_empty() {
                // The status bar mirrors the live chapter-title input.
                cx.notify();
            } else {
                self.flush_chapter_title(cx);
                self.refresh_editor_search(cx);
            }
        }
    }

    pub(crate) fn on_body_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            // A programmatic `sync_body_input` sets the value to exactly the
            // stored unit source, so the equality check below ignores those
            // reloads and only flags genuine user edits.
            let value = self.body_input.read(cx).value();
            if let Some(state) = self.unit_states.get(self.selected) {
                if value.as_ref() != state.source.as_str() {
                    self.modified_chapter_ids.insert(state.id.clone());
                }
            }
            self.draft_generation = self.draft_generation.wrapping_add(1);
            if self.tab == EditorTab::Source && !self.search_query.is_empty() {
                let _ = self.flush_body(cx);
                self.refresh_editor_search(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub(crate) fn refresh_editor_search(&mut self, cx: &mut Context<Self>) {
        self.search_results = search_editor_chapters(&self.chapters, &self.search_query);
        cx.notify();
    }

    pub(crate) fn attach_webview(
        &mut self,
        webview: Entity<WebView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor_webview = Some(webview);
        let close_after_build = self.webview_build_gate.finish();
        // Building is asynchronous, so honor whichever tab is active when the
        // native child WebView actually becomes ready.
        if self.tab.uses_webview() {
            self.load_webview(self.tab, window, cx);
            self.apply_webview_appearance(cx);
        } else if let Some(webview) = &self.editor_webview {
            webview.update(cx, |webview, _| webview.hide());
        }
        if close_after_build {
            self.resume_deferred_close(window, cx);
        }
        cx.notify();
        true
    }

    /// 把当前皮肤写进编辑器的 WebView。
    ///
    /// 每次页面加载后都要重放：切换章节与标签都会换一份文档，而自定义属性与注入的
    /// 样式表只存在于上一份文档上。
    pub(crate) fn apply_webview_appearance(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.editor_webview.as_ref() else {
            return;
        };
        let script = editor_appearance_script(current_appearance(cx));
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            // Presentation only: a missed injection must not become an editor error.
            tracing::warn!(%error, "cannot apply the editor appearance");
        }
    }

    /// Resumes a close that was vetoed while the child WebView was building.
    pub(crate) fn resume_deferred_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing_for_removed_book {
            self.finish_removal_close(window, cx);
        } else {
            self.handle_window_close(window, cx);
        }
    }
}
