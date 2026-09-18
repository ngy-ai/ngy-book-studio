//! Releasing the WebView and the close paths, including deferred closes.

use super::*;

impl EditorApp {
    pub(crate) fn release_webview(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<WeakEntity<WebView>> {
        // In-flight asynchronous custom-protocol tasks may outlive the Wry
        // child. Mark the channel closed before hiding/dropping the WebView so
        // late object-store reads cannot call its responder afterwards.
        self.web_state.close_protocol();
        self.active_web_page = None;
        self.ai_selected_text = None;
        self.pending_snapshot = None;
        self.pending_ready_action = None;
        self.pending_cover_action = None;
        self.pending_export_path = None;
        self.pending_ai_request = None;
        self.pending_citation_source = None;
        self.pending_citation_navigation = None;
        self.pending_toc_id = None;
        self.pending_media_action = None;
        self.media_modal = None;
        self.ipc_sync_task.take();
        self.ready_timeout_task.take();
        self.editor_webview.take().map(|webview| {
            let weak = webview.downgrade();
            webview.update(cx, |webview, _| webview.hide());
            weak
        })
    }

    pub(crate) fn schedule_window_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
        self.ai_controller.close();
        self.closing_webview = self.release_webview(cx);
        cx.notify();
        window.refresh();
    }

    pub(crate) fn handle_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.closing
            || self.close_prompt_open
            || self.close_without_saving_pending
            || self.save_and_close_pending()
        {
            return false;
        }
        if self.export_dialog_open {
            cancel_application_exit(cx);
            self.notice = Some(Notice {
                text: "请先关闭导出文件选择器，再关闭编辑器。".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        if self.webview_build_gate.request_close() {
            self.notice = Some(Notice {
                text: "正在等待编辑器初始化，完成后将询问是否保存。".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        // An explicitly accepted save/export may already be committing. Let it
        // settle before asking; closing must not silently authorize another save.
        if self.active_write.is_some() {
            self.close_confirmation_pending = true;
            self.notice = Some(Notice {
                text: "正在等待当前保存或导出完成，随后将询问是否保存并关闭。".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        self.close_prompt_open = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "关闭编辑窗口前，是否保存图书修改？",
            Some("选择“不保存”将放弃尚未保存的修改；选择“取消”继续编辑。"),
            &[
                PromptButton::ok("保存并关闭"),
                PromptButton::new("不保存"),
                PromptButton::cancel("取消"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            let answer = answer.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.close_prompt_open = false;
                match answer {
                    Ok(0) => this.begin_action(PendingEditorAction::Close, window, cx),
                    Ok(1) => {
                        // A previously requested Save/Export may have finished
                        // its snapshot barrier while the native prompt was open.
                        // Keep its result/projection callback alive, without
                        // authorizing any further write on close.
                        if this.active_write.is_some() {
                            this.close_without_saving_pending = true;
                            this.notice = Some(Notice {
                                text: "正在等待已开始的保存或导出结束，随后关闭编辑器。"
                                    .to_string(),
                                error: false,
                            });
                            cx.notify();
                        } else {
                            this.schedule_window_removal(window, cx);
                        }
                    }
                    Ok(_) => {
                        cancel_application_exit(cx);
                        cx.notify();
                    }
                    Err(_) => {
                        cancel_application_exit(cx);
                        this.notice = Some(Notice {
                            text: "保存确认未完成，编辑器保持打开。请重试关闭。".to_string(),
                            error: true,
                        });
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        false
    }

    /// Closes this editor because its book left the library.
    ///
    /// Unsaved edits are discarded: the document they belong to is already
    /// gone, so neither a save nor a confirmation may delay the close.
    pub(crate) fn close_for_removed_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing || self.closing_for_removed_book {
            return;
        }
        self.closing_for_removed_book = true;
        if self.webview_build_gate.request_close() {
            return;
        }
        self.finish_removal_close(window, cx);
    }

    pub(crate) fn finish_removal_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        // Drop every pending save/close decision so teardown cannot authorize
        // another write against a book that no longer exists.
        self.close_prompt_open = false;
        self.close_after_write = false;
        self.close_confirmation_pending = false;
        self.schedule_window_removal(window, cx);
    }

    pub(crate) fn save_and_close_pending(&self) -> bool {
        self.close_after_write
            || self
                .pending_snapshot
                .as_ref()
                .is_some_and(|pending| pending.action == PendingEditorAction::Close)
            || [
                self.pending_ready_action,
                self.pending_cover_action,
                self.pending_media_action,
            ]
            .contains(&Some(PendingEditorAction::Close))
    }

    pub(crate) fn resume_close_confirmation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if std::mem::take(&mut self.close_without_saving_pending) {
            self.schedule_window_removal(window, cx);
        } else if std::mem::take(&mut self.close_confirmation_pending) {
            self.handle_window_close(window, cx);
        }
    }
}
