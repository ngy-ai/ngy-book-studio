//! Construction, the collapsed rail and the pane resize handle.

use super::*;

impl ReaderApp {
    pub fn new(
        book_id: String,
        book_incarnation: u64,
        book: OpenedBook,
        current_spine: usize,
        progress_locators: Vec<DocumentLocator>,
        annotation_revisions: (u64, Vec<u64>),
        initial_citation: Option<ReflowableCitationTarget>,
        initial_anchor: Option<TextAnchor>,
        library: LibraryStore,
        services: Arc<AppServices>,
        library_view: Entity<EpubReaderApp>,
        webview_building: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前图书正文…"));
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let selected_toc = book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(current_spine));
        let current_book = AiBookOption::new(book_id.clone(), book.title.clone());
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
        let references = reader_reference_hints(
            &book_id,
            &book.spine,
            &progress_locators,
            current_spine,
            None,
        );
        ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
        let mut ai_controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book_id.clone()),
        )
        .expect("reader AI scope is valid");
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        let _annotation_ai_subscription =
            cx.subscribe(&ai_sidebar, Self::on_annotation_ai_completed);
        let _annotation_ai_submit_subscription =
            cx.subscribe(&ai_sidebar, Self::on_annotation_ai_submitted);
        let _annotation_ai_failure_subscription =
            cx.subscribe(&ai_sidebar, Self::on_annotation_ai_failed);
        let ai_sidebar_collapsed = ai_sidebar.read(cx).is_collapsed();
        let pane_layout = ReaderPaneLayout::new(ai_sidebar.read(cx).expanded_width());
        let _ai_layout_subscription = cx.observe(&ai_sidebar, |this, sidebar, cx| {
            let collapsed = sidebar.read(cx).is_collapsed();
            if this.ai_sidebar_collapsed != collapsed {
                this.ai_sidebar_collapsed = collapsed;
                cx.notify();
            }
        });
        ai_controller.restore(ai_sidebar.clone(), cx);
        let (progress_writer, progress_events) =
            ReadingProgressWriter::start(book_id.clone(), book_incarnation, Arc::clone(&services));
        let pending_citation_navigation = initial_citation.as_ref().and_then(|target| {
            target
                .focus
                .clone()
                .map(|focus| PendingReaderCitationNavigation {
                    request_id: 1,
                    spine_index: target.spine_index,
                    focus,
                })
        });
        // 打开书时进度里的章内位置在首个章节加载后恢复一次；在恢复之前它就
        // 是当前锚点，这样窗口提前关闭也不会把保存的位置覆盖成章节级。
        let pending_position_restore = initial_anchor.map(|anchor| (current_spine, anchor));
        let current_anchor = pending_position_restore
            .as_ref()
            .map(|(_, anchor)| anchor.clone());
        let global_display_mode = services
            .provider_settings()
            .map(|settings| settings.translation_display_mode)
            .unwrap_or_default();
        // 皮肤是应用级偏好：设置窗口保存后全局状态变一次，已打开的每一章都要跟着
        // 重放外观脚本，否则正在读的这本书会留在旧配色上直到翻页。
        let _theme_subscription =
            cx.observe_global::<ngy_book_studio::theme::AppTheme>(|this, cx| {
                this.apply_reading_appearance(cx);
            });
        let mut reader = Self {
            book_id,
            book_incarnation,
            book,
            current_spine,
            progress_locators,
            selected_toc,
            webview: None,
            library,
            services,
            library_view,
            ai_sidebar,
            ai_controller,
            pane_layout,
            ai_sidebar_collapsed,
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            selected_text: None,
            annotations: ReaderAnnotations::new(annotation_revisions),
            translations: ReaderTranslations::new(global_display_mode),
            read_aloud: ReadAloudHost::default(),
            speech_rate_menu_open: false,
            _annotation_ai_subscription,
            _annotation_ai_submit_subscription,
            _annotation_ai_failure_subscription,
            citation_request_id: u64::from(pending_citation_navigation.is_some()),
            pending_citation_navigation,
            current_anchor,
            pending_position_restore,
            _search_subscription,
            _ai_subscription,
            _ai_layout_subscription,
            _theme_subscription,
            webview_build_gate: ReaderWebViewBuildGate::new(webview_building),
            page_sync_task: None,
            progress_writer: Some(progress_writer),
            progress_sync_task: None,
            protocol_gate: None,
            closing_webview: None,
            closing: false,
            text_size: ngy_book_studio::reader::READER_FONT_SIZE_DEFAULT,
            text_size_save_task: None,
            current_reader_url: None,
            closing_for_removed_book: false,
            progress_close_ready: false,
            removal_scheduled: false,
            notice: None,
        };
        reader.progress_sync_task = Some(cx.spawn(async move |view, cx| {
            while let Ok(event) = progress_events.recv().await {
                if view
                    .update(cx, |this, cx| this.handle_progress_write_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        reader.start_translation_refresh(cx);
        reader.load_translation_display_override(cx);
        reader.refresh_navigation_labels(cx);
        reader.load_text_size(cx);
        reader
    }

    /// Sends a script to the reader webview after the caller's borrow is released.
    ///
    /// WebView2's `EvaluateScript` synchronously pumps window messages. Calling it
    /// from an entity update (especially a WebView IPC callback) re-enters GPUI's
    /// frame handler while `App` is still borrowed — that is the
    /// `RefCell already borrowed` in the log. The native handle is cheap to clone
    /// and the foreground task runs after the borrow is released.
    pub(crate) fn run_reader_script(
        &self,
        script: String,
        on_error: impl FnOnce(&mut Self, String, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self
            .webview
            .as_ref()
            .map(|webview| webview.read(cx).handle())
        else {
            return;
        };
        cx.spawn(async move |view, cx| {
            if let Err(error) = handle.raw().evaluate_script(&script) {
                let _ = view.update(cx, |this, cx| on_error(this, error.to_string(), cx));
            }
        })
        .detach();
    }

    pub(crate) fn toggle_navigation_collapsed(&mut self, cx: &mut Context<Self>) {
        if self.pane_layout.toggle_navigation_collapsed() {
            cx.notify();
        }
    }

    /// Narrow rail shown while the table of contents is collapsed. The reader
    /// keeps the resized width so expanding restores the previous layout.
    pub(crate) fn render_collapsed_navigation(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        div()
            .v_flex()
            .w(px(READER_NAVIGATION_COLLAPSED_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .border_r_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).sidebar))
            .py_3()
            .child(
                Button::new("reader-navigation-expand")
                    .ghost()
                    .icon(IconName::PanelLeftOpen)
                    .tooltip("展开目录")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle_navigation_collapsed(cx));
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .text_color(rgb(palette(cx).accent))
                    .child(Icon::new(IconName::Menu).small()),
            )
            .into_any_element()
    }

    pub(crate) fn render_pane_resize_handle(
        &self,
        pane: ReaderResizablePane,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let drag_view = view.clone();
        let handle_id = match pane {
            ReaderResizablePane::Navigation => "reader-navigation-resize-handle",
            ReaderResizablePane::Ai => "reader-ai-resize-handle",
        };
        let active = self.pane_layout.is_resizing(pane);

        div()
            .id(handle_id)
            .h_full()
            .w(px(READER_PANE_RESIZE_HANDLE_WIDTH))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .bg(rgb(palette(cx).surface))
            .child(
                div()
                    .h_full()
                    .w(px(if active { 2. } else { 1. }))
                    .bg(rgb(if active {
                        palette(cx).accent
                    } else {
                        palette(cx).border
                    })),
            )
            .hover(|this| this.bg(rgb(palette(cx).accent_soft)))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<ReaderPaneResizeDrag>, window, cx| {
                    if event.drag(cx).0 != pane {
                        return;
                    }
                    if this.pane_layout.resize_from_pointer(
                        pane,
                        event.event.position.x,
                        window.viewport_size().width,
                        this.ai_sidebar_collapsed,
                    ) {
                        if pane == ReaderResizablePane::Ai {
                            let width = this.pane_layout.ai_width();
                            this.ai_sidebar
                                .update(cx, |sidebar, cx| sidebar.set_expanded_width(width, cx));
                        }
                        cx.notify();
                    }
                },
            ))
            .on_drag(
                ReaderPaneResizeDrag(pane),
                move |_: &ReaderPaneResizeDrag, _, _, cx| {
                    cx.stop_propagation();
                    drag_view.update(cx, |this, cx| {
                        if this.pane_layout.begin_resize(pane) {
                            cx.notify();
                        }
                    });
                    cx.new(|_| EmptyView)
                },
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.pane_layout.finish_resize() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.pane_layout.finish_resize() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }
}
