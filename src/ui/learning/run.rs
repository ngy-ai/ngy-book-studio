//! Starting a run, collecting its trace and installing the report.

use super::*;

impl LearningWindow {
    pub(super) fn start_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs_disabled() || !self.environment.as_ref().is_some_and(|env| env.ready) {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        let submitted_revision = workspace.revision;
        self.running = true;
        self.run_generation = self.run_generation.wrapping_add(1);
        let generation = self.run_generation;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellation = Some(Arc::clone(&cancellation));
        self.report = None;
        self.trace.clear();
        self.trace_selected = None;
        self.omitted_traces = 0;
        self.evidence_pane = EvidencePane::Trace;
        self.notice = Some(LearningNotice {
            text: "正在保存本次代码并运行实验；结果和轨迹来自实际执行。".to_string(),
            error: false,
        });
        let (sender, receiver) = async_channel::bounded::<LearningTrace>(128);
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = receiver.recv().await {
                let accepted = view.update(cx, |this, cx| {
                    if this.run_generation != generation || !this.running {
                        return false;
                    }
                    this.push_trace(event);
                    cx.notify();
                    true
                });
                if !matches!(accepted, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let result = service.run(workspace, cancellation, sender).await;
            // run persists the exact code revision before execution. Refresh even
            // after cancellation/error so the next save uses that new revision.
            let snapshot = service.snapshot().await;
            let _ = view.update_in(cx, |this, window, cx| {
                if this.run_generation != generation {
                    return;
                }
                this.running = false;
                this.cancellation = None;
                this.run_generation = this.run_generation.wrapping_add(1);
                match result {
                    Ok(report) => {
                        this.install_report(report);
                        this.evidence_pane = EvidencePane::Result;
                        this.notice = Some(LearningNotice {
                            text: "本次实验已结束。运行判定与学习证据请分别查看。".to_string(),
                            error: false,
                        });
                    }
                    Err(error) => {
                        // Persistence failures must remain visible even when the
                        // user requested closing during the run.
                        this.set_error(format!("实验未完成：{error:#}"), cx);
                    }
                }
                match snapshot {
                    Ok(snapshot) => {
                        if snapshot.workspace.revision == submitted_revision && this.dirty(cx) {
                            // Validation may fail before run saved anything. Keep
                            // unsaved input instead of restoring the older code.
                            this.lessons = snapshot.lessons;
                            this.history = snapshot.history;
                            this.environment = Some(snapshot.environment);
                        } else {
                            this.apply_snapshot(snapshot, window, cx);
                        }
                        this.advance_close(window, cx);
                    }
                    Err(error) => {
                        this.reload_required = true;
                        this.set_error(
                            format!("实验已结束，工作区版本尚未确认；请点击重新检测载入已保存记录：{error:#}"),
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

    pub(super) fn push_trace(&mut self, trace: LearningTrace) {
        if self.trace.len() == MAX_VISIBLE_TRACES {
            self.trace.remove(0);
            self.omitted_traces += 1;
            self.trace_selected = self.trace_selected.and_then(|index| index.checked_sub(1));
        }
        self.trace.push(trace);
    }

    pub(super) fn install_report(&mut self, report: LearningRunReport) {
        self.omitted_traces = report.trace.len().saturating_sub(MAX_VISIBLE_TRACES);
        self.trace = report
            .trace
            .iter()
            .skip(self.omitted_traces)
            .cloned()
            .collect();
        self.trace_selected = None;
        self.report = Some(report);
    }

    pub(super) fn cancel_run(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.store(true, Ordering::Release);
            self.notice = Some(LearningNotice {
                text: "已请求取消，正在等待执行器停止并保存真实状态…".to_string(),
                error: false,
            });
            cx.notify();
        }
    }
}
