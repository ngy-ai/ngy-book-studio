//! Turning a job snapshot into the labels and actions the UI shows.

use super::*;

pub(crate) fn job_kind_label(kind: &str) -> &'static str {
    match kind {
        "embedding" => "向量索引",
        "vision" => "视觉理解",
        "visual_render" => "逻辑页面渲染",
        "translation" => "图书翻译",
        _ => "未知任务",
    }
}

pub(crate) fn status_presentation(
    job: &BackgroundJobSnapshot,
    theme: Palette,
) -> (&'static str, gpui_kit::Rgba, gpui_kit::Rgba) {
    if job.cancel_requested {
        return ("正在取消", rgb(theme.warning), rgb(theme.warning_soft));
    }
    if job.pause_requested {
        return ("正在暂停", rgb(theme.warning), rgb(theme.warning_soft));
    }
    match job.status {
        BackgroundJobStatus::Queued => ("排队中", rgb(theme.neutral), rgb(theme.neutral_soft)),
        BackgroundJobStatus::Running => {
            ("运行中", rgb(theme.accent_strong), rgb(theme.accent_soft))
        }
        BackgroundJobStatus::Paused => ("已暂停", rgb(theme.warning), rgb(theme.warning_soft)),
        BackgroundJobStatus::Succeeded => ("已完成", rgb(theme.success), rgb(theme.success_soft)),
        BackgroundJobStatus::Failed => ("失败", rgb(theme.danger), rgb(theme.danger_soft)),
        BackgroundJobStatus::Cancelled => ("已取消", rgb(theme.neutral), rgb(theme.neutral_soft)),
    }
}

pub(crate) fn available_actions(job: &BackgroundJobSnapshot) -> Vec<BackgroundJobAction> {
    if job.pause_requested || job.cancel_requested {
        return Vec::new();
    }
    let mut actions = match job.status {
        BackgroundJobStatus::Queued | BackgroundJobStatus::Running => {
            vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
        }
        BackgroundJobStatus::Paused => {
            vec![BackgroundJobAction::Resume, BackgroundJobAction::Cancel]
        }
        BackgroundJobStatus::Failed | BackgroundJobStatus::Cancelled => {
            vec![BackgroundJobAction::Retry, BackgroundJobAction::Delete]
        }
        BackgroundJobStatus::Succeeded => Vec::new(),
    };
    // A completed translation can be re-run from the first block; other kinds
    // have no equivalent that would not discard published derived indexes.
    if job.kind == "translation" {
        actions.push(BackgroundJobAction::Retranslate);
    }
    actions
}

pub(crate) fn action_label(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "暂停",
        BackgroundJobAction::Resume => "恢复",
        BackgroundJobAction::Retry => "重试",
        BackgroundJobAction::Cancel => "取消",
        BackgroundJobAction::Retranslate => "重新翻译",
        BackgroundJobAction::Delete => "删除",
    }
}

pub(crate) fn action_verb(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "暂停",
        BackgroundJobAction::Resume => "恢复",
        BackgroundJobAction::Retry => "重试",
        BackgroundJobAction::Cancel => "取消",
        BackgroundJobAction::Retranslate => "重新翻译",
        BackgroundJobAction::Delete => "删除",
    }
}

pub(crate) fn action_id(action: BackgroundJobAction) -> &'static str {
    match action {
        BackgroundJobAction::Pause => "pause",
        BackgroundJobAction::Resume => "resume",
        BackgroundJobAction::Retry => "retry",
        BackgroundJobAction::Cancel => "cancel",
        BackgroundJobAction::Retranslate => "retranslate",
        BackgroundJobAction::Delete => "delete",
    }
}

pub(crate) fn progress_label(job: &BackgroundJobSnapshot) -> String {
    let unit = match job.kind.as_str() {
        "embedding" => "分块",
        "vision" | "visual_render" => "页面",
        "translation" => "文本块",
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
pub(crate) fn status_database_label(status: BackgroundJobStatus) -> &'static str {
    match status {
        BackgroundJobStatus::Queued => "queued",
        BackgroundJobStatus::Running => "running",
        BackgroundJobStatus::Paused => "paused",
        BackgroundJobStatus::Succeeded => "succeeded",
        BackgroundJobStatus::Failed => "failed",
        BackgroundJobStatus::Cancelled => "cancelled",
    }
}

/// Format a Unix timestamp (seconds) as `YYYY-MM-DD HH:MM:SS` in UTC without depending on `chrono` or `time` crates.
pub(crate) fn format_timestamp(unix_seconds: u64) -> String {
    format_utc_datetime(unix_seconds).unwrap_or_else(|| "—".to_string())
}

pub(crate) fn not_set() -> String {
    "—".to_string()
}

pub(crate) fn format_utc_datetime(unix_seconds: u64) -> Option<String> {
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
pub(crate) fn civil_from_days(days_since_epoch: i64) -> Option<(i32, u32, u32)> {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
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
pub(crate) fn prettify_cursor(cursor: &str) -> String {
    let parsed = match serde_json::from_str::<serde_json::Value>(cursor) {
        Ok(value) => value,
        Err(_) => return cursor.to_string(),
    };
    match serde_json::to_string_pretty(&parsed) {
        Ok(pretty) => pretty,
        Err(_) => cursor.to_string(),
    }
}
