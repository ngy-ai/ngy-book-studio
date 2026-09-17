//! Pause/cancel observation and outcome publication: the single place where a
//! finished run becomes durable rows and a terminal job status.

use super::*;

use std::{future::Future, pin::Pin, sync::Arc, time::Instant};

use anyhow::{Result, ensure};

use crate::{
    ai::job_diagnostics::{JobLogErrorKind, JobLogEvent, JobLogMetrics},
    db,
};

pub(crate) enum Controlled<T> {
    Value(T),
    Interrupted(RunOutcome),
}

/// The control state of one in-flight translation block.
///
/// The comparison is against the JSON this run last persisted, never against a
/// copied cursor: the walk keeps committing later blocks while this request waits,
/// so the durable row moves on with it. Only a row that no longer carries the
/// run's own JSON — replaced by a per-block retry reset, by a fresh execution, or
/// gone — means the request is no longer wanted.
pub(crate) async fn block_stop_request(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    committed_json: &str,
) -> Result<Option<StopRequest>> {
    let job_id = job.id.clone();
    let db_path = inner.db_path.clone();
    let current = run_db(db_path, move |conn| db::index_jobs::get(&conn, &job_id)).await?;
    let Some(current) = current else {
        return Ok(Some(StopRequest::Abandoned));
    };
    if current.status != db::index_jobs::IndexJobStatus::Running {
        return Ok(Some(StopRequest::Abandoned));
    }
    if current.cursor_json != committed_json {
        return Ok(Some(StopRequest::Abandoned));
    }
    if current.cancel_requested {
        return Ok(Some(StopRequest::Cancel));
    }
    if current.pause_requested {
        return Ok(Some(StopRequest::Pause));
    }
    Ok(None)
}

pub(crate) async fn await_provider_step<T>(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
    future: Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<Controlled<T>> {
    let started = Instant::now();
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result.map(Controlled::Value),
            _ = tokio::time::sleep(CONTROL_POLL_INTERVAL) => {
                if let Some(outcome) = requested_outcome(inner, job, cursor).await? {
                    return Ok(Controlled::Interrupted(outcome));
                }
                ensure!(started.elapsed() < PROVIDER_STEP_TIMEOUT, "模型请求超时");
            }
        }
    }
}

pub(crate) async fn requested_outcome(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
) -> Result<Option<RunOutcome>> {
    let job_id = job.id.clone();
    let db_path = inner.db_path.clone();
    let current = run_db(db_path, move |conn| db::index_jobs::get(&conn, &job_id)).await?;
    let Some(current) = current else {
        return Ok(Some(RunOutcome::Abandoned));
    };
    if current.status != db::index_jobs::IndexJobStatus::Running {
        return Ok(Some(RunOutcome::Abandoned));
    }
    if current.cursor_json != cursor.encode()? {
        return Ok(Some(RunOutcome::Abandoned));
    }
    if current.cancel_requested {
        return Ok(Some(RunOutcome::Cancelled(cursor.clone(), None)));
    }
    if current.pause_requested {
        return Ok(Some(RunOutcome::Paused(cursor.clone())));
    }
    Ok(None)
}

/// A guarded derived-index publication can be rejected because a newer execution
/// replaced its row, because pause/cancel arrived after the last poll, or
/// because the durable cursor unexpectedly lost synchronization. Settle an
/// extant running row atomically: control flags win, otherwise fail closed so
/// no workerless `running` row can remain. A replaced/non-running row belongs
/// to the newer transition and is left untouched.
pub(crate) async fn settle_interrupted_or_abandoned(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
) -> Result<RunOutcome> {
    let job_id = job.id.clone();
    let db_path = inner.db_path.clone();
    run_db(db_path, move |conn| {
        let Some(current) = db::index_jobs::get(&conn, &job_id)? else {
            return Ok(RunOutcome::Abandoned);
        };
        if current.status != db::index_jobs::IndexJobStatus::Running {
            return Ok(RunOutcome::Abandoned);
        }
        let now = unix_timestamp()?;
        let changed = db::index_jobs::finalize_running_from_cursor(
            &conn,
            &job_id,
            &current.cursor_json,
            db::index_jobs::IndexJobStatus::Failed,
            &current.cursor_json,
            Some("索引任务执行游标失去同步，请重试"),
            now,
            Some(now),
        )?;
        ensure!(changed == 1, "索引任务状态在收敛时再次变化");
        Ok(RunOutcome::Abandoned)
    })
    .await
}

pub(crate) async fn source_is_current(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    revision: u64,
) -> Result<bool> {
    let Some(source_id) = job.source_id.clone() else {
        return Ok(false);
    };
    let book_id = job.book_id.clone();
    let db_path = inner.db_path.clone();
    run_db(db_path, move |conn| {
        let Some(book) = db::books::get(&conn, &book_id)? else {
            return Ok(false);
        };
        let Some(source) = db::book_sources::get(&conn, &source_id)? else {
            return Ok(false);
        };
        Ok(book.revision == revision
            && source.revision == revision
            && source.book_id == book_id
            && db::book_sources::get_revision(&conn, &book_id, revision)?
                .is_some_and(|current| current.id == source_id))
    })
    .await
}

pub(crate) async fn persist_running_cursor(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
) -> Result<()> {
    let job_id = job.id.clone();
    let cursor_json = cursor.encode()?;
    let db_path = inner.db_path.clone();
    let now = unix_timestamp()?;
    let changed = run_db(db_path, move |conn| {
        db::index_jobs::update_state_from(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Running,
            db::index_jobs::IndexJobStatus::Running,
            &cursor_json,
            None,
            now,
            None,
            None,
        )
    })
    .await?;
    ensure!(changed == 1, "索引任务在保存进度前消失");
    Ok(())
}

pub(crate) async fn current_cursor(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
) -> Result<String> {
    let job_id = job.id.clone();
    let db_path = inner.db_path.clone();
    run_db(db_path, move |conn| {
        db::index_jobs::get(&conn, &job_id)?.context("索引任务不存在")
    })
    .await
    .map(|job| job.cursor_json)
}

pub(crate) async fn publish_outcome(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    outcome: RunOutcome,
    expected: db::index_jobs::IndexJobStatus,
) -> Result<()> {
    let waiting_for_pages = matches!(&outcome, RunOutcome::WaitingForPages(_));
    let (status, cursor, error, error_kind, finished) = match outcome {
        RunOutcome::Succeeded(cursor) => (
            db::index_jobs::IndexJobStatus::Succeeded,
            cursor,
            None,
            None,
            true,
        ),
        RunOutcome::Paused(cursor) => (
            db::index_jobs::IndexJobStatus::Paused,
            cursor,
            None,
            None,
            false,
        ),
        RunOutcome::Cancelled(cursor, error) => (
            db::index_jobs::IndexJobStatus::Cancelled,
            cursor,
            error,
            None,
            true,
        ),
        RunOutcome::Failed(cursor, error, error_kind) => (
            db::index_jobs::IndexJobStatus::Failed,
            cursor,
            Some(error),
            error_kind,
            true,
        ),
        RunOutcome::WaitingForPages(cursor) => (
            db::index_jobs::IndexJobStatus::Failed,
            cursor,
            Some(db::index_jobs::VISION_WAITING_FOR_PAGES_ERROR.to_string()),
            None,
            true,
        ),
        RunOutcome::Abandoned => {
            record_index_event(
                inner,
                job,
                JobLogEvent::Superseded,
                JobLogMetrics::default(),
            );
            return Ok(());
        }
    };
    let job_id = job.id.clone();
    let cursor_json = cursor.encode()?;
    let error = error.map(|error| truncate_chars(&error, MAX_PERSISTED_ERROR_CHARS));
    let db_path = inner.db_path.clone();
    let now = unix_timestamp()?;
    let attempt = job.attempts;
    let (changed, published_status) = run_db(db_path, move |conn| {
        let changed = if expected == db::index_jobs::IndexJobStatus::Running {
            db::index_jobs::finalize_running_from_cursor(
                &conn,
                &job_id,
                &cursor_json,
                status,
                &cursor_json,
                error.as_deref(),
                now,
                finished.then_some(now),
            )?
        } else {
            db::index_jobs::update_state_from(
                &conn,
                &job_id,
                expected,
                status,
                &cursor_json,
                error.as_deref(),
                now,
                None,
                finished.then_some(now),
            )?
        };
        // Finalization gives a concurrently accepted pause/cancel precedence.
        // Report the committed state, never claim success merely because the
        // executor proposed it. Diagnostic reads cannot fail the operation.
        let published_status = (changed == 1)
            .then(|| db::index_jobs::get(&conn, &job_id).ok().flatten())
            .flatten()
            .filter(|current| current.cursor_json == cursor_json && current.attempts == attempt)
            .map(|current| current.status);
        Ok((changed, published_status))
    })
    .await?;
    if let Some(status) = published_status {
        let event = match status {
            db::index_jobs::IndexJobStatus::Succeeded => JobLogEvent::RunSucceeded,
            db::index_jobs::IndexJobStatus::Failed if waiting_for_pages => {
                JobLogEvent::WaitingForPages
            }
            db::index_jobs::IndexJobStatus::Failed => JobLogEvent::RunFailed,
            db::index_jobs::IndexJobStatus::Paused => JobLogEvent::Paused,
            db::index_jobs::IndexJobStatus::Cancelled => JobLogEvent::Cancelled,
            _ => JobLogEvent::Superseded,
        };
        record_index_event(
            inner,
            job,
            event,
            JobLogMetrics {
                ordinal: Some(cursor.next_ordinal as u64),
                duration_ms: INDEXING_LOG_RUN
                    .try_with(|(_, started)| started.elapsed().as_millis() as u64)
                    .ok(),
                // The headline failure line must name the reason on its own:
                // the per-block lines above it are pruned first under the log
                // retention limit and may already be gone.
                error_kind: error_kind.or_else(|| {
                    matches!(status, db::index_jobs::IndexJobStatus::Failed)
                        .then_some(JobLogErrorKind::Unknown)
                }),
                ..JobLogMetrics::default()
            },
        );
    } else if changed == 0 {
        record_index_event(
            inner,
            job,
            JobLogEvent::Superseded,
            JobLogMetrics::default(),
        );
    }
    // Close the race where page publication happened after the vision worker's
    // empty read but before the sink tried to requeue the still-running job.
    if changed == 1
        && waiting_for_pages
        && visual_pages_exist(inner, job.source_id.as_deref().unwrap_or_default()).await?
    {
        let source_id = job.source_id.as_deref().unwrap_or_default().to_string();
        let db_path = inner.db_path.clone();
        let now = unix_timestamp()?;
        run_db(db_path, move |conn| {
            db::index_jobs::retry_failed_for_source_kind(&conn, &source_id, "vision", now)
        })
        .await?;
        inner.wake.notify_one();
    }
    Ok(())
}
