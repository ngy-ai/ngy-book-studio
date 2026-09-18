//! Releasing the webview, draining the progress writer and the close paths.

use super::*;

impl ReaderApp {
    pub(crate) fn start_progress_writer(&mut self, cx: &mut Context<Self>) {
        let (writer, progress_events) = ReadingProgressWriter::start(
            self.book_id.clone(),
            self.book_incarnation,
            Arc::clone(&self.services),
        );
        self.progress_writer = Some(writer);
        self.progress_sync_task = Some(cx.spawn(async move |view, cx| {
            while let Ok(event) = progress_events.recv().await {
                if view
                    .update(cx, |this, cx| this.handle_progress_write_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub(crate) fn release_webview_for_close(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<WeakEntity<WebView>> {
        self.pending_citation_navigation = None;
        self.progress_sync_task.take();
        self.page_sync_task.take();
        self.webview.take().map(|webview| {
            let weak = webview.downgrade();
            webview.update(cx, |webview, _| webview.hide());
            weak
        })
    }

    pub(crate) fn finish_progress_writer(&mut self, cx: &mut Context<Self>) {
        let Some(final_progress) = self.current_progress_write() else {
            self.recover_from_progress_close(
                "当前章节缺少稳定阅读位置，无法确认最终进度已保存。".to_string(),
                cx,
            );
            return;
        };
        let Some(writer) = self.progress_writer.take() else {
            self.recover_from_progress_close("阅读进度后台任务不可用。".to_string(), cx);
            return;
        };
        let (queued, completion) = writer.finish(final_progress);
        let queue_error = queued.err();
        cx.spawn(async move |view, cx| {
            let result = match queue_error {
                Some(error) => Err(error),
                None => completion
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("阅读进度后台任务在完成关闭屏障前停止".to_string())),
            };
            let _ = view.update(cx, |this, cx| match result {
                Ok(projection) => {
                    // Apply the final projection in the same GPUI turn before
                    // close teardown drops the event listener.
                    this.apply_progress_projection(projection, cx);
                    this.complete_progress_close(cx);
                }
                Err(error) => this.recover_from_progress_close(error, cx),
            });
        })
        .detach();
    }

    pub(crate) fn complete_progress_close(&mut self, cx: &mut Context<Self>) {
        self.finish_close(cx);
    }

    /// Drops every resource the window owns. Render removes the native window
    /// once `closing` and `progress_close_ready` are both set.
    pub(crate) fn finish_close(&mut self, cx: &mut Context<Self>) {
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
        self.ai_controller.close();
        if let Some(protocol_gate) = self.protocol_gate.take() {
            protocol_gate.close();
        }
        self.closing_webview = self.release_webview_for_close(cx);
        self.progress_close_ready = true;
        cx.notify();
    }

    pub(crate) fn recover_from_progress_close(&mut self, error: String, cx: &mut Context<Self>) {
        cancel_application_exit(cx);
        tracing::warn!(%error, "final reading progress was not persisted");
        self.progress_sync_task.take();
        self.closing = false;
        self.progress_close_ready = false;
        self.removal_scheduled = false;
        self.start_progress_writer(cx);
        self.notice = Some(Notice {
            text: format!(
                "阅读进度保存失败，窗口已保持打开。请检查磁盘或权限后再次关闭以重试：{error}"
            ),
            error: true,
        });
        cx.notify();
    }

    pub(crate) fn schedule_window_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.progress_close_ready = false;
        self.finish_progress_writer(cx);
        cx.notify();
        window.refresh();
    }

    pub(crate) fn handle_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.closing {
            return false;
        }
        if self.annotation_navigation_blocked(cx) {
            cancel_application_exit(cx);
            return false;
        }
        if self.webview_build_gate.request_close() {
            self.notice = Some(Notice {
                text: "正在安全关闭阅读器…".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        self.schedule_window_removal(window, cx);
        false
    }

    /// Closes this reader because its book left the library.
    ///
    /// Unlike a user-initiated close there is nothing left to persist: the
    /// final position is dropped with its writer, and no failed write may
    /// reopen a window whose document no longer exists.
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
        self.closing = true;
        // Dropping the writer stops its runtime worker without queueing a
        // final position for a book that has already been removed.
        self.progress_writer.take();
        self.finish_close(cx);
        window.refresh();
    }
}
