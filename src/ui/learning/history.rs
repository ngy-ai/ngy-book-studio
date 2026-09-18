//! Loading history, exporting and restoring a round.

use super::*;

impl LearningWindow {
    pub(super) fn load_history(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.operation = Some(Operation::LoadingReport);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = service.load_report(id).await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match outcome {
                    Ok(report) => {
                        this.install_report(report);
                        this.evidence_pane = EvidencePane::Result;
                        this.advance_close(window, cx);
                    }
                    Err(error) => this.set_error(format!("无法读取历史记录：{error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs_disabled() {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出第{}章学习备份", self.service.chapter()))
            .set_filename(format!(
                "墨页-Agent第{}章学习备份.json",
                self.service.chapter()
            ))
            .add_filter("墨页学习备份", ["json"])
            .save_single_file();
        self.operation = Some(Operation::Exporting);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            // Native dialogs pump messages: show only outside the entity borrow.
            let selected = dialog.show();
            let outcome = match selected {
                Ok(Some(path)) => match service.save(workspace).await {
                    Ok(saved) => {
                        let exported = service.export(path.clone()).await;
                        (Some(saved), exported.map(|()| Some(path)))
                    }
                    Err(error) => (None, Err(error)),
                },
                Ok(None) => (None, Ok(None)),
                Err(error) => (None, Err(anyhow::anyhow!("无法打开导出对话框：{error}"))),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                if let Some(saved) = outcome.0 {
                    this.saved_workspace = Some(saved);
                }
                match outcome.1 {
                    Ok(path) => {
                        if let Some(path) = path {
                            this.notice = Some(LearningNotice {
                                text: format!("学习备份已导出到 {}", path.display()),
                                error: false,
                            });
                        }
                        this.advance_close(window, cx);
                    }
                    Err(error) => this.set_error(format!("导出学习备份失败：{error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || (self.reload_required && self.saved_workspace.is_some()) {
            return;
        }
        let workspace = self.entered_workspace(cx);
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("恢复第{}章学习备份", self.service.chapter()))
            .add_filter("墨页学习备份", ["json"]);
        self.operation = Some(Operation::Restoring);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let dialog = match service.restore_dialog_directory().await {
                Ok(directory) => dialog.set_location(&directory),
                Err(error) => {
                    // A default folder is only a convenience. A missing or
                    // inaccessible archive must not prevent choosing a backup.
                    tracing::warn!(%error, "无法准备学习备份默认目录，使用系统文件选择位置");
                    dialog
                }
            };
            // Native dialogs pump messages: show outside the entity borrow.
            let selected = dialog.open_single_file().show();
            let (saved, outcome, records_may_have_changed) = match selected {
                Ok(Some(path)) => match workspace {
                    Some(workspace) => match service.save(workspace).await {
                        Ok(saved) => (Some(saved), service.restore(path).await.map(Some), true),
                        Err(error) => (None, Err(error), true),
                    },
                    None => (None, service.restore(path).await.map(Some), true),
                },
                Ok(None) => (None, Ok(None), false),
                Err(error) => (
                    None,
                    Err(anyhow::anyhow!("无法打开恢复对话框：{error}")),
                    false,
                ),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.finish_restore(saved, outcome, records_may_have_changed, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn finish_restore(
        &mut self,
        saved: Option<LearningWorkspace>,
        outcome: Result<Option<LearningSnapshot>>,
        records_may_have_changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation = None;
        if let Some(saved) = saved {
            self.saved_workspace = Some(saved);
        }
        match outcome {
            Ok(snapshot) => {
                if let Some(snapshot) = snapshot {
                    self.replace_inputs(window, cx);
                    self.apply_snapshot(snapshot, window, cx);
                    self.report = None;
                    self.trace.clear();
                    self.trace_selected = None;
                    self.omitted_traces = 0;
                    self.showing_reference = false;
                    self.evidence_pane = EvidencePane::History;
                    self.notice = Some(LearningNotice {
                        text: "学习备份已恢复。导入的历史结果均标记为待核验。".to_string(),
                        error: false,
                    });
                }
                self.advance_close(window, cx);
            }
            Err(error) => {
                // Opening/cancelling the picker never touches the record file.
                // Keep existing uncertainty, but do not invent a revision conflict.
                if records_may_have_changed {
                    self.reload_required = true;
                    self.set_error(format!("恢复未完成，请重新检测当前记录：{error:#}"), cx);
                } else {
                    self.set_error(format!("{error:#}；当前输入已保留，可重试恢复。"), cx);
                }
            }
        }
        cx.notify();
    }
}
