//! Entering a chapter and saving the edits made to it.

use super::*;

impl LearningWindow {
    pub(super) fn enter_chapter(
        &mut self,
        chapter: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chapter_entry_disabled() {
            return;
        }
        if chapter == self.service.chapter() {
            self.study_pane = StudyPane::Lesson;
            self.showing_reference = false;
            if self.saved_workspace.is_none() {
                self.load(window, cx);
            }
            cx.notify();
            return;
        }
        let next = match self.service.for_chapter(chapter) {
            Ok(service) => Arc::new(service),
            Err(error) => {
                self.set_error(format!("无法打开本章实验：{error:#}"), cx);
                return;
            }
        };
        let current = Arc::clone(&self.service);
        let pending = self.dirty(cx).then(|| self.entered_workspace(cx)).flatten();
        self.operation = Some(Operation::SwitchingChapter);
        cx.spawn_in(window, async move |view, cx| {
            // Save against the old immutable service before leaving its inputs.
            // A target load failure still opens that chapter's recovery state,
            // so its own backup can be restored from the visible controls.
            let result = match pending {
                Some(workspace) => match current.save(workspace).await {
                    Ok(_) => Ok(next.snapshot().await),
                    Err(error) => Err(error),
                },
                None => Ok(next.snapshot().await),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match result {
                    Ok(Ok(snapshot)) => {
                        this.service = next;
                        this.replace_inputs(window, cx);
                        this.apply_snapshot(snapshot, window, cx);
                        this.report = None;
                        this.trace.clear();
                        this.trace_selected = None;
                        this.omitted_traces = 0;
                        this.showing_reference = false;
                        this.evidence_pane = EvidencePane::Result;
                        this.study_pane = StudyPane::Lesson;
                        this.notice = None;
                        this.advance_close(window, cx);
                    }
                    Ok(Err(error)) => {
                        this.service = next;
                        this.clear_unloaded_chapter(window, cx);
                        this.set_error(
                            format!(
                                "第{chapter}章记录未能载入。此前章节已保存；可恢复本章备份、重新检测，或从左侧进入其它章节：{error:#}"
                            ),
                            cx,
                        );
                    }
                    Err(error) => {
                        this.set_error(
                            format!("当前章节保存失败，切换未完成，输入已保留：{error:#}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.is_some() || self.running || self.reload_required {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        self.operation = Some(Operation::Saving);
        self.notice = Some(LearningNotice {
            text: "正在保存代码、预测和学习记录…".to_string(),
            error: false,
        });
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = service.save(workspace).await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match outcome {
                    Ok(saved) => {
                        this.saved_workspace = Some(saved);
                        this.notice = Some(LearningNotice {
                            text: "代码、预测和学习记录已保存。".to_string(),
                            error: false,
                        });
                        this.advance_close(window, cx);
                    }
                    Err(error) => {
                        this.set_error(format!("保存失败，窗口保持打开，请重试：{error:#}"), cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
