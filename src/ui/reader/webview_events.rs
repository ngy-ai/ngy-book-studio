//! Attaching the webview and reacting to everything it reports.

use super::*;

impl ReaderApp {
    pub(crate) fn on_ai_sidebar_event(
        &mut self,
        _sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                if self.closing || self.webview_build_gate.close_requested {
                    self.ai_sidebar.update(cx, |sidebar, cx| {
                        sidebar.cancel_for_window_close(cx);
                    });
                    return;
                }
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

    pub(crate) fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source.book_id != self.book_id {
            self.library_view.update(cx, |library, cx| {
                library.open_book_at_source(source, None, window, cx);
            });
            return;
        }
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let opened = self.book.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            reflowable_citation_target(&lookup, &document, &opened).map_err(anyhow::Error::msg)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|_window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(target)) => this.navigate_to_citation(target, cx),
                    Ok(Err(error)) => this.set_error(format!("引用已失效：{error:#}"), cx),
                    Err(error) => this.set_error(format!("引用定位任务已停止：{error}"), cx),
                });
            });
        })
        .detach();
    }

    pub(crate) fn on_search_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = input.read(cx).value().trim().to_string();
                self.search_query = query.clone();
                self.search_results.clear();
                if self.notice.as_ref().is_some_and(|notice| {
                    notice.error && notice.text.starts_with("搜索当前图书失败：")
                }) {
                    self.notice = None;
                }
                if !query.is_empty() {
                    match self.library.search_book(&self.book_id, &query, 200) {
                        Ok(results) => self.search_results = results,
                        Err(error) => {
                            self.notice = Some(Notice {
                                text: format!("搜索当前图书失败：{error:#}"),
                                error: true,
                            });
                        }
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(spine_index) =
                    self.search_results.first().and_then(|hit| hit.spine_index)
                {
                    self.navigate_to_spine(spine_index, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn attach_webview(
        &mut self,
        webview: Entity<WebView>,
        protocol_gate: ReaderProtocolGate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.webview = Some(webview);
        self.protocol_gate = Some(protocol_gate);
        if self.webview_build_gate.finish() {
            self.resume_deferred_close(window, cx);
            // Final persistence is asynchronous. Keep the page event receiver
            // alive while the child WebView is retained so a failed close can
            // restore a fully functional reader; successful teardown cancels
            // the task in `release_webview_for_close`.
            return self.webview.is_some();
        }
        self.notice = None;
        cx.notify();
        true
    }

    /// Resumes a close that was vetoed while the child WebView was building.
    pub(crate) fn resume_deferred_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing_for_removed_book {
            self.finish_removal_close(window, cx);
        } else {
            self.schedule_window_removal(window, cx);
        }
    }

    pub(crate) fn fail_webview_build(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.webview_build_gate.finish() {
            self.resume_deferred_close(window, cx);
            return;
        }
        self.set_error(error, cx);
    }

    pub(crate) fn sync_web_event(&mut self, event: ReaderWebEvent, cx: &mut Context<Self>) {
        match event {
            ReaderWebEvent::AnnotationNavigationBlocked => {
                self.annotation_navigation_blocked(cx);
            }
            ReaderWebEvent::AnnotationAction { url, action } => {
                self.handle_annotation_action(&url, action, cx)
            }
            ReaderWebEvent::ManualTranslation { url, request } => {
                self.handle_manual_translation(&url, request, cx)
            }
            ReaderWebEvent::RetranslateBlock { url, request } => {
                self.handle_retranslate_block(&url, request, cx)
            }
            ReaderWebEvent::PageLoaded(url) => self.sync_loaded_page(&url, cx),
            ReaderWebEvent::ExplainSelection { url, selected_text } => {
                self.explain_selection(&url, &selected_text, cx);
            }
            ReaderWebEvent::SelectionChanged { url, selected_text } => {
                self.sync_selection(&url, selected_text, cx)
            }
            ReaderWebEvent::ReadingPosition { url, anchor } => {
                self.sync_reading_position(&url, anchor, cx)
            }
            ReaderWebEvent::ReadAloudStart {
                url,
                start_block,
                blocks,
            } => self.start_read_aloud_from_page(&url, start_block, blocks, cx),
            ReaderWebEvent::TextSize { url, font_size } => self.sync_text_size(&url, font_size, cx),
            ReaderWebEvent::CitationNavigationResult {
                url,
                request_id,
                found,
                reason,
            } => self.finish_citation_navigation(&url, request_id, found, &reason, cx),
        }
        self.refresh_note_navigation_gate();
    }

    /// 页面请求朗读。只接受**当前章页面**的请求：迟到的消息不能借旧页面启动朗读，
    /// 段落列表本身也要过容量校验。
    pub(crate) fn start_read_aloud_from_page(
        &mut self,
        url: &str,
        start_block: String,
        blocks: Vec<ReadAloudPageBlock>,
        cx: &mut Context<Self>,
    ) {
        if self.closing || !read_aloud_request_is_valid(&blocks) {
            return;
        }
        if self.current_reader_url.as_deref() != Some(url) {
            return;
        }
        self.start_read_aloud(blocks, &start_block, cx);
    }

    pub(crate) fn sync_loaded_page(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        // 新章节或重新加载的页面：旧页面的朗读段落与高亮都不再对应当前 DOM。
        self.stop_read_aloud(cx);
        let Some(spine_index) = self.book.spine_index_for_url(url) else {
            return;
        };
        if self.current_spine != spine_index {
            // 换章后位置未定：进度先记到章节级，直到页面报告新的章内位置。
            self.current_anchor = None;
        }
        self.current_spine = spine_index;
        self.current_reader_url = Some(url.to_string());
        self.selected_toc = self
            .book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(spine_index));
        self.selected_text = None;
        // 打开书后的首个章节：数据库里的章内位置仍然有效，直到用户滚出新的
        // 位置。带着它写入可以避免恢复失败时丢失保存的位置。
        if self.current_anchor.is_none()
            && let Some((spine, anchor)) = self.pending_position_restore.as_ref()
            && *spine == spine_index
        {
            self.current_anchor = Some(anchor.clone());
        }
        self.sync_ai_reference(cx);
        self.notice = None;
        self.queue_current_progress(cx);
        self.run_pending_citation_navigation(url, cx);
        self.configure_annotations(cx);
        self.configure_translations(url, cx);
        self.apply_reading_appearance(cx);
        self.run_pending_position_restore(url, cx);
        cx.notify();
    }

    pub(crate) fn run_pending_citation_navigation(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if self.book.spine_index_for_url(url) != Some(pending.spine_index) {
            return;
        }
        let Some(webview) = self.webview.as_ref() else {
            self.pending_citation_navigation = None;
            self.set_error("正文视图尚未就绪，无法定位引用".to_owned(), cx);
            return;
        };
        let script = match citation_dom_navigation_script(
            &pending.focus,
            serde_json::json!({
                "type": "citation_navigation_result",
                "request_id": pending.request_id,
            }),
        ) {
            Ok(script) => script,
            Err(error) => {
                self.pending_citation_navigation = None;
                self.set_error(error, cx);
                return;
            }
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.pending_citation_navigation = None;
            self.set_error(format!("无法执行引用定位：{error}"), cx);
        }
    }

    pub(crate) fn finish_citation_navigation(
        &mut self,
        url: &str,
        request_id: u64,
        found: bool,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if pending.request_id != request_id
            || self.book.spine_index_for_url(url) != Some(pending.spine_index)
        {
            return;
        }
        self.pending_citation_navigation = None;
        if found {
            self.notice = None;
        } else {
            let detail = match reason {
                "ambiguous" => "正文中存在多个相同片段",
                "not_found" | "missing_root" | "empty_text" => "正文中找不到该片段",
                _ => "正文无法映射该文字范围",
            };
            self.set_error(
                format!("引用定位失败：{detail}；未跳转到章节中的其它位置。"),
                cx,
            );
        }
    }

    pub(crate) fn sync_selection(
        &mut self,
        url: &str,
        selected_text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.closing
            || self.book.spine_index_for_url(url) != Some(self.current_spine)
            || self.selected_text == selected_text
        {
            return;
        }
        self.selected_text = selected_text;
        self.sync_ai_reference(cx);
        cx.notify();
    }

    /// The chapter runtime already applied the size the reader wheeled to, so
    /// this only records it. A message from a chapter the window has since left
    /// is ignored: the size belongs to the book, not to a superseded document.
    pub(crate) fn sync_text_size(&mut self, url: &str, font_size: u8, cx: &mut Context<Self>) {
        if self.closing
            || self.book.spine_index_for_url(url) != Some(self.current_spine)
            || self.text_size == font_size
        {
            return;
        }
        self.text_size = font_size;
        self.schedule_text_size_save(cx);
        cx.notify();
    }
}
