use super::*;

use moye_epub_editor::services::{
    AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BackgroundJobBook {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug)]
struct JobsNotice {
    text: String,
    error: bool,
}

pub(super) struct BackgroundJobsWindow {
    services: Arc<AppServices>,
    books: Vec<BackgroundJobBook>,
    scope_label: String,
    jobs: Vec<BackgroundJobSnapshot>,
    refresh_generation: u64,
    loading: bool,
    pending_job_id: Option<String>,
    /// Job whose details panel is currently expanded. Only one panel is shown
    /// at a time so the list stays readable; clicking the same header again
    /// collapses it.
    expanded_job_id: Option<String>,
    notice: Option<JobsNotice>,
}

impl BackgroundJobsWindow {
    fn new(services: Arc<AppServices>, books: Vec<BackgroundJobBook>, scope_label: String) -> Self {
        Self {
            services,
            books,
            scope_label,
            jobs: Vec::new(),
            refresh_generation: 0,
            loading: false,
            pending_job_id: None,
            expanded_job_id: None,
            notice: None,
        }
    }

    fn toggle_expanded(&mut self, job_id: String, cx: &mut Context<Self>) {
        self.expanded_job_id = if self.expanded_job_id.as_deref() == Some(job_id.as_str()) {
            None
        } else {
            Some(job_id)
        };
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        let generation = self.refresh_generation;
        self.loading = true;
        let services = Arc::clone(&self.services);
        let book_ids = self.books.iter().map(|book| book.id.clone()).collect();
        cx.spawn_in(window, async move |view, cx| {
            let result = services.background_jobs_for_books(book_ids).await;
            let _ = view.update(cx, |this, cx| {
                if generation != this.refresh_generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(jobs) => {
                        this.jobs = jobs;
                    }
                    Err(error) => {
                        this.notice = Some(JobsNotice {
                            text: format!("无法刷新后台任务：{error:#}"),
                            error: true,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_action(
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
        self.notice = Some(JobsNotice {
            text: format!("正在{}任务…", action_verb(action)),
            error: false,
        });
        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            let result = services
                .control_background_job(job_id.clone(), action)
                .await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.pending_job_id = None;
                    match result {
                        Ok(true) => {
                            this.notice = Some(JobsNotice {
                                text: format!("已请求{}任务。", action_verb(action)),
                                error: false,
                            });
                        }
                        Ok(false) => {
                            this.notice = Some(JobsNotice {
                                text: "任务状态已发生变化，未重复执行操作。".to_string(),
                                error: false,
                            });
                        }
                        Err(error) => {
                            this.notice = Some(JobsNotice {
                                text: format!("{}任务失败：{error:#}", action_verb(action)),
                                error: true,
                            });
                        }
                    }
                    this.refresh(window, cx);
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        div()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_4()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_0p5()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .text_color(rgb(INK))
                            .child("后台任务"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(format!(
                                "{} · {} 本图书",
                                self.scope_label,
                                self.books.len()
                            )),
                    ),
            )
            .child(
                Button::new("background-jobs-refresh")
                    .outline()
                    .icon(IconName::Redo2)
                    .label(if self.loading {
                        "正在刷新…"
                    } else {
                        "刷新"
                    })
                    .disabled(self.loading || self.pending_job_id.is_some())
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.notice = None;
                            this.refresh(window, cx);
                        });
                    }),
            )
            .into_any_element()
    }

    fn render_summary(&self) -> gpui::AnyElement {
        let running = self
            .jobs
            .iter()
            .filter(|job| job.status == BackgroundJobStatus::Running)
            .count();
        let queued = self
            .jobs
            .iter()
            .filter(|job| job.status == BackgroundJobStatus::Queued)
            .count();
        let failed = self
            .jobs
            .iter()
            .filter(|job| job.status == BackgroundJobStatus::Failed)
            .count();
        div()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PAPER))
            .text_sm()
            .child(
                div()
                    .font_semibold()
                    .text_color(rgb(INK))
                    .child(format!("{} 个任务", self.jobs.len())),
            )
            .child(
                div()
                    .h_flex()
                    .gap_4()
                    .text_color(rgb(MUTED))
                    .child(format!("排队 {queued} · 运行 {running} · 失败 {failed}")),
            )
            .into_any_element()
    }

    fn render_notice(&self) -> Option<gpui::AnyElement> {
        let notice = self.notice.as_ref()?;
        let (background, foreground, icon) = if notice.error {
            (rgb(0xf8e2de), rgb(DANGER), IconName::TriangleAlert)
        } else {
            (rgb(ACCENT_SOFT), rgb(ACCENT_DARK), IconName::Info)
        };
        Some(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .mx_6()
                .mt_4()
                .px_3()
                .py_2()
                .rounded(px(8.))
                .bg(background)
                .text_sm()
                .text_color(foreground)
                .child(Icon::new(icon).small())
                .child(notice.text.clone())
                .into_any_element(),
        )
    }

    fn render_book_section(
        &self,
        book: &BackgroundJobBook,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let jobs = self
            .jobs
            .iter()
            .filter(|job| job.book_id == book.id)
            .collect::<Vec<_>>();
        let mut body = div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w(px(0.))
                            .font_semibold()
                            .text_color(rgb(INK))
                            .truncate()
                            .child(book.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("{} 个任务", jobs.len())),
                    ),
            );
        if jobs.is_empty() {
            body = body.child(
                div()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("当前图书没有后台任务。"),
            );
        } else {
            for job in jobs {
                body = body.child(self.render_job(job, cx));
            }
        }
        body.into_any_element()
    }

    fn render_job(&self, job: &BackgroundJobSnapshot, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (status_text, status_color, status_background) = status_presentation(job);
        let actions = available_actions(job);
        let has_actions = !actions.is_empty();
        let pending = self.pending_job_id.as_deref() == Some(job.id.as_str());
        let any_pending = self.pending_job_id.is_some();
        let view = cx.entity().clone();
        let mut action_row = div().h_flex().flex_none().gap_2();
        for action in actions {
            let action_view = view.clone();
            let job_id = job.id.clone();
            action_row = action_row.child(
                Button::new(SharedString::from(format!(
                    "background-job-{}-{}",
                    job.id,
                    action_id(action)
                )))
                .small()
                .outline()
                .label(action_label(action))
                .disabled(any_pending || self.loading)
                .on_click(move |_, window, cx| {
                    action_view.update(cx, |this, cx| {
                        this.apply_action(job_id.clone(), action, window, cx)
                    });
                }),
            );
        }

        let expanded = self.expanded_job_id.as_deref() == Some(job.id.as_str());
        let toggle_view = view.clone();
        let toggle_job_id = job.id.clone();
        let details_toggle = Button::new(SharedString::from(format!(
            "background-job-{}-details",
            job.id
        )))
        .small()
        .ghost()
        .icon(if expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .label(if expanded {
            "收起详情"
        } else {
            "查看详情"
        })
        .disabled(self.loading)
        .on_click(move |_, _, cx| {
            toggle_view.update(cx, |this, cx| {
                this.toggle_expanded(toggle_job_id.clone(), cx);
            });
        });

        let mut card = div()
            .v_flex()
            .gap_2()
            .p_3()
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PAPER))
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .gap_1()
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(rgb(INK))
                                            .child(job_kind_label(&job.kind)),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(status_background)
                                            .text_xs()
                                            .text_color(status_color)
                                            .child(status_text),
                                    ),
                            )
                            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{} · 已尝试 {} 次",
                                progress_label(job),
                                job.attempts
                            ))),
                    )
                    .child(
                        div()
                            .v_flex()
                            .flex_none()
                            .items_end()
                            .gap_2()
                            .when(has_actions, |this| this.child(action_row))
                            .child(details_toggle),
                    ),
            )
            .when_some(job.error.as_ref(), |this, error| {
                this.child(
                    div()
                        .px_2()
                        .py_1p5()
                        .rounded(px(6.))
                        .bg(rgb(0xf8e2de))
                        .text_xs()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(DANGER))
                        .child(error.clone()),
                )
            })
            .when(pending, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(ACCENT_DARK))
                        .child("正在提交操作，请稍候…"),
                )
            });
        if expanded {
            card = card.child(self.render_job_details(job));
        }
        card.into_any_element()
    }

    fn render_job_details(&self, job: &BackgroundJobSnapshot) -> gpui::AnyElement {
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        rows.push(("任务 ID", truncate_middle(&job.id, 28)));
        rows.push(("图书 ID", truncate_middle(&job.book_id, 28)));
        if let Some(source_id) = job.source_id.as_deref() {
            rows.push(("来源 ID", truncate_middle(source_id, 28)));
        }
        rows.push(("任务类型", job_kind_label(&job.kind).to_string()));
        rows.push(("数据库状态", status_database_label(job.status).to_string()));
        rows.push(("已尝试次数", job.attempts.to_string()));
        rows.push((
            "进度",
            match job.progress.total {
                Some(total) => format!(
                    "{} / {}（已完成 {}）",
                    job.progress.completed.min(total),
                    total,
                    job.progress.completed
                ),
                None => format!("已处理 {}", job.progress.completed),
            },
        ));
        rows.push(("创建时间", format_timestamp(job.created_at)));
        rows.push(("更新时间", format_timestamp(job.updated_at)));
        rows.push((
            "开始时间",
            job.started_at.map(format_timestamp).unwrap_or_else(not_set),
        ));
        rows.push((
            "结束时间",
            job.finished_at
                .map(format_timestamp)
                .unwrap_or_else(not_set),
        ));
        rows.push((
            "挂起请求",
            if job.cancel_requested {
                "已请求取消".to_string()
            } else if job.pause_requested {
                "已请求暂停".to_string()
            } else {
                "无".to_string()
            },
        ));

        let mut body = div()
            .v_flex()
            .gap_1p5()
            .p_3()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .text_sm();
        for (label, value) in rows {
            body = body.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .flex_none()
                            .w(px(96.))
                            .text_color(rgb(MUTED))
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .text_color(rgb(INK))
                            .child(value),
                    ),
            );
        }
        if let Some(cursor) = job.cursor_json.as_deref() {
            body = body.child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_none()
                            .w(px(96.))
                            .text_color(rgb(MUTED))
                            .child("游标"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1p5()
                            .rounded(px(6.))
                            .bg(rgb(SIDEBAR))
                            .text_xs()
                            .line_height(gpui::relative(1.5))
                            .text_color(rgb(INK))
                            .overflow_x_scrollbar()
                            .child(prettify_cursor(cursor)),
                    ),
            );
        }
        body.into_any_element()
    }
}

impl Render for BackgroundJobsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_4()
            .p_6()
            .overflow_y_scrollbar();
        if self.books.is_empty() {
            list = list.child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(rgb(MUTED))
                    .child(Icon::new(IconName::Inbox))
                    .child("当前范围没有图书。"),
            );
        } else {
            for book in &self.books {
                list = list.child(self.render_book_section(book, cx));
            }
        }

        div()
            .v_flex()
            .size_full()
            .bg(rgb(PAPER))
            .child(self.render_header(cx))
            .child(self.render_summary())
            .when_some(self.render_notice(), |this, notice| this.child(notice))
            .child(list)
    }
}

pub(super) fn open_background_jobs_window(
    services: Arc<AppServices>,
    books: Vec<BackgroundJobBook>,
    scope_label: String,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    let bounds = Bounds::centered(None, size(px(820.), px(720.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(640.), px(520.))),
            titlebar: Some(TitlebarOptions {
                title: Some("墨页 · 后台任务".into()),
                ..Default::default()
            }),
            app_id: Some("dev.moye.epub-editor.background-jobs".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let jobs =
                cx.new(|_| BackgroundJobsWindow::new(Arc::clone(&services), books, scope_label));
            jobs.update(cx, |jobs, cx| jobs.refresh(window, cx));
            on_window_close(window, cx, |_, _| true);
            cx.new(|cx| Root::new(jobs, window, cx))
        },
    )
    .context("无法创建后台任务窗口")?;
    Ok(())
}

fn job_kind_label(kind: &str) -> &'static str {
    match kind {
        "embedding" => "向量索引",
        "vision" => "视觉理解",
        "visual_render" => "逻辑页面渲染",
        _ => "未知任务",
    }
}

fn status_presentation(job: &BackgroundJobSnapshot) -> (&'static str, gpui::Rgba, gpui::Rgba) {
    if job.cancel_requested {
        return ("正在取消", rgb(0x8a5a20), rgb(0xf4e8d5));
    }
    if job.pause_requested {
        return ("正在暂停", rgb(0x8a5a20), rgb(0xf4e8d5));
    }
    match job.status {
        BackgroundJobStatus::Queued => ("排队中", rgb(0x5e6572), rgb(0xe7e9ed)),
        BackgroundJobStatus::Running => ("运行中", rgb(ACCENT_DARK), rgb(ACCENT_SOFT)),
        BackgroundJobStatus::Paused => ("已暂停", rgb(0x8a5a20), rgb(0xf4e8d5)),
        BackgroundJobStatus::Succeeded => ("已完成", rgb(0x376441), rgb(0xe3efe5)),
        BackgroundJobStatus::Failed => ("失败", rgb(DANGER), rgb(0xf8e2de)),
        BackgroundJobStatus::Cancelled => ("已取消", rgb(0x5e6572), rgb(0xe7e9ed)),
    }
}

fn available_actions(job: &BackgroundJobSnapshot) -> Vec<BackgroundJobAction> {
    if job.pause_requested || job.cancel_requested {
        return Vec::new();
    }
    match job.status {
        BackgroundJobStatus::Queued | BackgroundJobStatus::Running => {
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        }
        BackgroundJobStatus::Paused => {
            vec![BackgroundJobAction::Resume, BackgroundJobAction::Cancel]
        }
        BackgroundJobStatus::Failed | BackgroundJobStatus::Cancelled => {
            vec![BackgroundJobAction::Retry]
        }
        BackgroundJobStatus::Succeeded => Vec::new(),
    }
}

fn action_label(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "暂停",
        BackgroundJobAction::Resume => "恢复",
        BackgroundJobAction::Retry => "重试",
        BackgroundJobAction::Cancel => "取消",
    }
}

fn action_verb(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "暂停",
        BackgroundJobAction::Resume => "恢复",
        BackgroundJobAction::Retry => "重试",
        BackgroundJobAction::Cancel => "取消",
    }
}

fn action_id(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "pause",
        BackgroundJobAction::Resume => "resume",
        BackgroundJobAction::Retry => "retry",
        BackgroundJobAction::Cancel => "cancel",
    }
}

fn progress_label(job: &BackgroundJobSnapshot) -> String {
    let unit = match job.kind.as_str() {
        "embedding" => "分块",
        "vision" | "visual_render" => "页面",
        _ => "项",
    };
    match job.progress.total {
        Some(total) => format!(
            "进度 {} / {} {unit}",
            job.progress.completed.min(total),
            total
        ),
        None => format!("已处理 {} {unit}", job.progress.completed),
    }
}

/// Database-level status label (ignoring transient pause/cancel requests).
/// Mirrors the strings used by `index_jobs::IndexJobStatus` so users see the
/// actual durable state alongside the softer presentation label.
fn status_database_label(status: BackgroundJobStatus) -> &'static str {
    match status {
        BackgroundJobStatus::Queued => "queued",
        BackgroundJobStatus::Running => "running",
        BackgroundJobStatus::Paused => "paused",
        BackgroundJobStatus::Succeeded => "succeeded",
        BackgroundJobStatus::Failed => "failed",
        BackgroundJobStatus::Cancelled => "cancelled",
    }
}

/// Truncate an opaque identifier to `max_chars` characters, keeping both ends
/// visible. UUIDs and source IDs are easier to copy when the prefix/suffix are
/// still readable; full IDs are still available in the `cursor` row of the
/// details panel when the service exposes them.
fn truncate_middle(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars || max_chars <= 4 {
        return value.to_string();
    }
    let head = max_chars / 2 - 1;
    let tail = max_chars - head - 3;
    let prefix: String = chars.iter().take(head).collect();
    let suffix: String = chars
        .iter()
        .rev()
        .take(tail)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

/// Format a Unix timestamp (seconds) as `YYYY-MM-DD HH:MM:SS` in the local
/// time zone without depending on `chrono` or `time` crates.
fn format_timestamp(unix_seconds: u64) -> String {
    format_local_datetime(unix_seconds).unwrap_or_else(|| "—".to_string())
}

fn not_set() -> String {
    "—".to_string()
}

fn format_local_datetime(unix_seconds: u64) -> Option<String> {
    let secs = i64::try_from(unix_seconds).ok()?;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (year, month, day) = civil_from_days(days)?;
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    ))
}

/// Howard Hinnant's date algorithm: convert Unix days since 1970-01-01 into a
/// proleptic Gregorian (year, month, day). Pure integer math, no leap-second
/// or calendar library required.
fn civil_from_days(days_since_epoch: i64) -> Option<(i32, u32, u32)> {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as i64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y } as i32;
    Some((year, m, d))
}

/// Pretty-print a small JSON cursor for inspection in the details panel.
/// Falls back to the raw string for non-object payloads or when the parser
/// itself can't make the result more readable.
fn prettify_cursor(cursor: &str) -> String {
    let parsed = match serde_json::from_str::<serde_json::Value>(cursor) {
        Ok(value) => value,
        Err(_) => return cursor.to_string(),
    };
    match serde_json::to_string_pretty(&parsed) {
        Ok(pretty) => pretty,
        Err(_) => cursor.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moye_epub_editor::services::BackgroundJobProgress;

    fn job(status: BackgroundJobStatus) -> BackgroundJobSnapshot {
        BackgroundJobSnapshot {
            id: "job-1".to_string(),
            book_id: "book-1".to_string(),
            source_id: None,
            kind: "embedding".to_string(),
            status,
            pause_requested: false,
            cancel_requested: false,
            attempts: 2,
            progress: BackgroundJobProgress {
                completed: 3,
                total: Some(10),
            },
            error: None,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_500,
            started_at: Some(1_700_000_100),
            finished_at: None,
            cursor_json: None,
        }
    }

    #[test]
    fn actions_follow_the_durable_job_state_machine() {
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Queued)),
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Running)),
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Paused)),
            vec![BackgroundJobAction::Resume, BackgroundJobAction::Cancel]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Failed)),
            vec![BackgroundJobAction::Retry]
        );
        assert_eq!(
            available_actions(&job(BackgroundJobStatus::Cancelled)),
            vec![BackgroundJobAction::Retry]
        );
        assert!(available_actions(&job(BackgroundJobStatus::Succeeded)).is_empty());
    }

    #[test]
    fn pending_control_hides_duplicate_actions_and_updates_status() {
        let mut pending = job(BackgroundJobStatus::Running);
        pending.pause_requested = true;
        assert!(available_actions(&pending).is_empty());
        assert_eq!(status_presentation(&pending).0, "正在暂停");

        pending.pause_requested = false;
        pending.cancel_requested = true;
        assert!(available_actions(&pending).is_empty());
        assert_eq!(status_presentation(&pending).0, "正在取消");
    }

    #[test]
    fn progress_is_bounded_by_the_latest_total() {
        let mut snapshot = job(BackgroundJobStatus::Running);
        assert_eq!(progress_label(&snapshot), "进度 3 / 10 分块");
        snapshot.progress.completed = 12;
        assert_eq!(progress_label(&snapshot), "进度 10 / 10 分块");
        snapshot.progress.total = None;
        assert_eq!(progress_label(&snapshot), "已处理 12 分块");
    }

    #[test]
    fn status_database_label_matches_durable_state() {
        assert_eq!(status_database_label(BackgroundJobStatus::Queued), "queued");
        assert_eq!(
            status_database_label(BackgroundJobStatus::Running),
            "running"
        );
        assert_eq!(status_database_label(BackgroundJobStatus::Paused), "paused");
        assert_eq!(
            status_database_label(BackgroundJobStatus::Succeeded),
            "succeeded"
        );
        assert_eq!(status_database_label(BackgroundJobStatus::Failed), "failed");
        assert_eq!(
            status_database_label(BackgroundJobStatus::Cancelled),
            "cancelled"
        );
    }

    #[test]
    fn timestamp_round_trips_through_civil_from_days() {
        // 1700000000 == 2023-11-14 22:13:20 UTC; the local-zone display only
        // affects HH:MM:SS so the date must be stable across machines.
        let formatted = format_timestamp(1_700_000_000);
        assert!(formatted.starts_with("2023-11-14 "));
        assert_eq!(formatted.len(), "YYYY-MM-DD HH:MM:SS".len());
    }

    #[test]
    fn truncate_middle_keeps_both_ends_visible() {
        let long = "0123456789abcdef0123456789abcdef";
        let truncated = truncate_middle(long, 12);
        assert!(truncated.starts_with("0123"));
        assert!(truncated.ends_with("cdef"));
        assert!(truncated.contains('…'));
        // Short inputs pass through unchanged.
        assert_eq!(truncate_middle("short", 12), "short");
    }

    #[test]
    fn prettify_cursor_falls_back_for_invalid_json() {
        assert_eq!(prettify_cursor("not json"), "not json");
        let pretty = prettify_cursor(r#"{"next_ordinal":42,"model":"qwen"}"#);
        assert!(pretty.contains("\"next_ordinal\""));
        assert!(pretty.contains("\"model\""));
    }
}
