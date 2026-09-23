//! Closing the block detail and applying job actions.

use super::*;

impl BackgroundJobsWindow {
    pub(super) fn close_block_detail(&mut self, cx: &mut Context<Self>) {
        if self.block_detail.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn apply_action(
        &mut self,
        job_id: String,
        action: BackgroundJobAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_job_id.is_some() {
            return;
        }
        self.pending_job_id = Some(job_id.clone());
        let scope = self.scope_generation;
        self.notice = Some(JobsNotice {
            text: format!("正在{}任务…", action_verb(action)),
            error: false,
        });
        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            let result = services.control_background_job(job_id, action).await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |this, cx| {
                    this.pending_job_id = None;
                    if scope == this.scope_generation {
                        this.notice = Some(match result {
                            Ok(true) => JobsNotice {
                                text: format!("已请求{}任务。", action_verb(action)),
                                error: false,
                            },
                            Ok(false) => JobsNotice {
                                text: "任务状态已发生变化，未重复执行操作。".into(),
                                error: false,
                            },
                            Err(error) => JobsNotice {
                                text: format!("{}任务失败：{error:#}", action_verb(action)),
                                error: true,
                            },
                        });
                    }
                    // Invalidate any snapshot that began before the accepted operation.
                    this.refresh_generation = this.refresh_generation.wrapping_add(1);
                    this.loading = false;
                    this.clear_detail_cache();
                    this.refresh(window, cx);
                })
            });
        })
        .detach();
        cx.notify();
    }

    /// Re-runs the named text blocks of the selected translation task.
    ///
    /// A run that skipped blocks still finished as `Succeeded`, so this is the
    /// only path back to them: the user switches to a stronger model first, then
    /// retries. Blocks that already have a translation are filtered out by the
    /// service, so a stale row cannot overwrite good text.
    pub(super) fn retry_translation_blocks(
        &mut self,
        ordinals: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_job_id.is_some() || ordinals.is_empty() {
            return;
        }
        let Some(job_id) = self.selected_job_id.clone() else {
            return;
        };
        self.pending_job_id = Some(job_id.clone());
        let count = ordinals.len();
        self.notice = Some(JobsNotice {
            text: format!("正在重试 {count} 个文本块…"),
            error: false,
        });
        let scope = self.scope_generation;
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services
                .background_job_retry_translation_blocks(job_id, ordinals, book_ids)
                .await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |this, cx| {
                    this.pending_job_id = None;
                    if scope == this.scope_generation {
                        this.notice = Some(match result {
                            Ok(true) => JobsNotice {
                                text: format!("已请求重试 {count} 个文本块。"),
                                error: false,
                            },
                            // The task moved on (finished, replaced, or no longer a
                            // translation task): there is nothing to requeue. Blocks
                            // that already have a translation are no longer filtered
                            // out here — the walk decides that by content.
                            Ok(false) => JobsNotice {
                                text: "任务状态已变化，未重试。".into(),
                                error: false,
                            },
                            Err(error) => JobsNotice {
                                text: format!("重试文本块失败：{error:#}"),
                                error: true,
                            },
                        });
                    }
                    // Drop the cached list so the statuses are re-derived from
                    // the job log the retry just appended to.
                    this.clear_blocks();
                    this.refresh_generation = this.refresh_generation.wrapping_add(1);
                    this.loading = false;
                    this.refresh(window, cx);
                })
            });
        })
        .detach();
        cx.notify();
    }
}
