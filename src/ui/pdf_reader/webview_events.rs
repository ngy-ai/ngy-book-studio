//! Attaching the webview and reacting to the events it reports.

use super::*;

impl PdfReaderApp {
    pub(crate) fn on_ai_sidebar_event(
        &mut self,
        sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.submit(request.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_answer(
                            request.request_id,
                            "AI 会话尚未初始化，请检查设置后重试。".to_string(),
                            cx,
                        );
                    });
                }
            }
            AiSidebarEvent::Cancel { request_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.cancel(*request_id);
                }
            }
            AiSidebarEvent::NewSession => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.new_session(sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::SwitchSession { thread_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.switch_session(thread_id.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::DeleteSession { thread_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.delete_session(thread_id.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::DeleteTurn { user_db_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.delete_turn(user_db_id.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::ScopeChanged { book_ids } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.reconcile_scope(book_ids.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::OpenSource(source) => {
                self.open_ai_source(source.clone(), window, cx);
            }
        }
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
            current_canonical_source_unit_index(&lookup, &document).map_err(anyhow::Error::msg)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) => {
                        if source.book_id == this.book_id {
                            let page_number =
                                pdf_page_for_source(&this.pages, &source).or_else(|| {
                                    (!pdf_source_requires_exact_page(&source))
                                        .then(|| this.page_for_unit_index(Some(index)))
                                        .flatten()
                                });
                            if let Some(page_number) = page_number {
                                this.request_page(page_number, cx);
                            } else {
                                this.notice = Some(Notice {
                                    text: "引用对应的 PDF 页面已失效，未跳转到其它页面。"
                                        .to_string(),
                                    error: true,
                                });
                                cx.notify();
                            }
                        } else {
                            this.library_view.update(cx, |library, cx| {
                                library.open_book_at_source(source, Some(index), window, cx);
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        this.notice = Some(Notice {
                            text: format!("引用已失效：{error:#}"),
                            error: true,
                        });
                        cx.notify();
                    }
                    Err(error) => {
                        this.notice = Some(Notice {
                            text: format!("引用定位任务已停止：{error}"),
                            error: true,
                        });
                        cx.notify();
                    }
                });
            });
        })
        .detach();
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
                self.search_query = input.read(cx).value().trim().to_string();
                self.search_results.clear();
                if !self.search_query.is_empty() {
                    match self
                        .library
                        .search_book(&self.book_id, &self.search_query, 200)
                    {
                        Ok(results) => self.search_results = results,
                        Err(error) => {
                            self.notice = Some(Notice {
                                text: format!("搜索当前 PDF 失败：{error:#}"),
                                error: true,
                            });
                        }
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(index) = self.search_results.first().and_then(|hit| hit.spine_index) {
                    self.open_canonical_unit(index, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn attach_webview(
        &mut self,
        webview: Entity<WebView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.webview = Some(webview);
        if self.webview_build_gate.finish() {
            self.resume_deferred_close(window, cx);
            // A persistent PDF reader keeps its WebView until the final write
            // completes. Install the IPC receiver now so failure recovery does
            // not reopen a view that can no longer report page changes.
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

    /// Applies the global compact reading preference to this window's viewer.
    ///
    /// The saved preference only changes page spacing, so an already open reader
    /// follows it without being reopened and a stale viewer never reports an
    /// error: the update is one attribute write on the viewer document.
    pub(crate) fn apply_pdf_compact_reading(
        &mut self,
        compact_reading: bool,
        cx: &mut Context<Self>,
    ) {
        if self.pdf_compact_reading == compact_reading {
            return;
        }
        self.pdf_compact_reading = compact_reading;
        self.push_pdf_compact_reading(cx);
    }

    /// Sends the current preference to the viewer document. Safe to call more
    /// than once: the attribute write is idempotent, and a viewer that is still
    /// building simply receives it when it reports itself ready.
    pub(crate) fn push_pdf_compact_reading(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        let script = pdf_compact_reading_script(self.pdf_compact_reading);
        if let Err(error) = webview.read(cx).raw().evaluate_script(script) {
            // Presentation only: a missed update must not turn into reader error.
            tracing::warn!(%error, "cannot apply the PDF compact reading preference");
        }
    }

    /// Sends the current skin to the viewer document. Same idempotence contract as
    /// the reading preference: the writes are repeats of the same values, so a
    /// viewer that is still building simply receives them when it reports ready.
    pub(crate) fn push_pdf_appearance(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        let script = pdf_appearance_script(current_appearance(cx));
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            // Presentation only: a missed update must not turn into reader error.
            tracing::warn!(%error, "cannot apply the PDF appearance");
        }
    }
}
