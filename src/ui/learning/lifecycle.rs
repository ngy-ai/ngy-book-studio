//! New rounds, the close handshake and the drop hook.

use super::*;

impl LearningWindow {
    pub(super) fn new_round(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs_disabled() {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        self.operation = Some(Operation::NewRound);
        self.notice = Some(LearningNotice {
            text: "正在保存当前编辑并归档本轮历史…".to_string(),
            error: false,
        });
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = service.new_round(workspace).await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match outcome {
                    Ok(snapshot) => {
                        this.replace_inputs(window, cx);
                        this.apply_snapshot(snapshot, window, cx);
                        this.report = None;
                        this.trace.clear();
                        this.trace_selected = None;
                        this.omitted_traces = 0;
                        this.evidence_pane = EvidencePane::History;
                        this.notice = Some(LearningNotice {
                            text: "已开始新一轮，当前代码与笔记保留。此前历史已归档，可通过恢复备份重新载入。".to_string(),
                            error: false,
                        });
                        this.advance_close(window, cx);
                    }
                    Err(error) => {
                        this.reload_required = true;
                        this.set_error(format!("新一轮未完成，请重新检测当前记录：{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.removing {
            return;
        }
        self.closing = true;
        cx.global_mut::<LearningWindowTracker>().closing = true;
        self.cancel_run(cx);
        self.advance_close(window, cx);
        cx.notify();
    }

    pub(super) fn advance_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing || self.removing {
            return;
        }
        if self.reload_required && !self.running && self.operation.is_none() {
            self.load(window, cx);
            return;
        }
        match close_action(self.running, self.operation.is_some(), self.dirty(cx)) {
            CloseAction::Wait => {
                self.notice = Some(LearningNotice {
                    text: "关闭前正在等待实验停止和已接受的保存完成…".to_string(),
                    error: false,
                });
            }
            CloseAction::Save => self.save(window, cx),
            CloseAction::Remove => {
                self.removing = true;
                remove_window_after_current_frame(window, cx, None);
            }
        }
    }
}

impl Drop for LearningWindow {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.store(true, Ordering::Release);
        }
    }
}
