//! Helpers shared by every indexing worker: the database hop onto the
//! application I/O runtime, and the small formatting and validation helpers
//! used by the job log.

use super::*;

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, ensure};
use tokio::runtime::Handle;

use crate::{
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics, record_for_database},
    db,
};

pub(crate) fn record_index_event(
    inner: &IndexingInner,
    job: &db::index_jobs::IndexJob,
    event: JobLogEvent,
    mut metrics: JobLogMetrics,
) {
    metrics.run_id = INDEXING_LOG_RUN.try_with(|(id, _)| *id).ok();
    metrics.attempt.get_or_insert(u64::from(job.attempts));
    record_for_database(&inner.db_path, &job.id, event, metrics);
    // 每次事件都是「任务还活着」的证据，静默看门狗据此判断「已领取但无进展」。
    // 放在记录之后：写失败也不影响事件本身已经尽力落盘。
    inner.note_progress(&job.id, Instant::now());
}

pub(crate) fn cursor_without_model(job: &db::index_jobs::IndexJob) -> Result<JobCursor> {
    serde_json::from_str(&job.cursor_json).context("索引任务游标无效")
}

pub(crate) fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(crate) fn is_supported_image_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

pub(crate) fn validated_model(model: String, kind: &str) -> Result<String> {
    ensure!(
        !model.trim().is_empty()
            && model.trim() == model
            && model.chars().count() <= 256
            && !model.chars().any(char::is_control),
        "{kind} model is invalid"
    );
    Ok(model)
}

pub(crate) fn validated_execution_identity(identity: String, kind: &str) -> Result<String> {
    ensure!(
        !identity.trim().is_empty()
            && identity.trim() == identity
            && identity.chars().count() <= 256
            && !identity.chars().any(char::is_control),
        "{kind} execution identity is invalid"
    );
    Ok(identity)
}

pub(crate) fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_secs())
}

pub(crate) fn snapshot_from_row(job: db::index_jobs::IndexJob) -> IndexingJobSnapshot {
    IndexingJobSnapshot {
        id: job.id,
        book_id: job.book_id,
        source_id: job.source_id,
        kind: job.kind,
        status: match job.status {
            db::index_jobs::IndexJobStatus::Queued => IndexingJobStatus::Queued,
            db::index_jobs::IndexJobStatus::Running => IndexingJobStatus::Running,
            db::index_jobs::IndexJobStatus::Paused => IndexingJobStatus::Paused,
            db::index_jobs::IndexJobStatus::Succeeded => IndexingJobStatus::Succeeded,
            db::index_jobs::IndexJobStatus::Failed => IndexingJobStatus::Failed,
            db::index_jobs::IndexJobStatus::Cancelled => IndexingJobStatus::Cancelled,
        },
        attempts: job.attempts,
        cursor_json: job.cursor_json,
        error: job.error,
    }
}

/// Runs one blocking database operation on the ambient Tokio runtime.
///
/// This resolves the runtime with [`Handle::current`], so it may only be
/// awaited by work the coordinator spawned itself: the worker loop and the
/// steps it drives. Anything the UI can await must pass the handle explicitly
/// through [`run_db_on`] instead. GPUI callbacks run on their own executor,
/// not on a Tokio runtime, and resolving the ambient handle there panics in a
/// function that cannot unwind, which aborts the whole application.
pub(crate) async fn run_db<T, F>(db_path: PathBuf, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(rusqlite::Connection) -> Result<T> + Send + 'static,
{
    run_db_on(Handle::current(), db_path, operation).await
}

pub(crate) async fn run_db_on<T, F>(runtime: Handle, db_path: PathBuf, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(rusqlite::Connection) -> Result<T> + Send + 'static,
{
    runtime
        .spawn_blocking(move || operation(db::open_conn(&db_path)?))
        .await
        .context("索引数据库工作线程已停止")?
}

pub(crate) async fn run_db_mut<T, F>(db_path: PathBuf, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(rusqlite::Connection) -> Result<T> + Send + 'static,
{
    run_db(db_path, operation).await
}
