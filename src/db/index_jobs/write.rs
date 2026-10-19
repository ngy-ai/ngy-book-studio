//! Writes of the `index_jobs` table: enqueueing, reconfiguration, recovery,
//! guarded claims, control requests, resets, and terminal publication.

use super::*;

use super::model::VISION_WAITING_FOR_PAGES_ERROR;

/// Rewrites one derived job to a caller-supplied execution generation,
/// regardless of its previous state. Used by translation reconfiguration to
/// restart work whose model or target language changed. A worker still holding
/// the old cursor observes the mismatch and abandons its in-flight attempt.
pub(crate) fn reset_reconfigured(
    conn: &Connection,
    job_id: &str,
    status: IndexJobStatus,
    cursor_json: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = ?2, cursor_json = ?3, pause_requested = 0,
         cancel_requested = 0, error = NULL, updated_at = ?4, started_at = NULL,
         finished_at = NULL
         WHERE id = ?1",
        params![job_id, status.as_str(), cursor_json, updated_at as i64],
    )
    .context("无法重置翻译任务")
}

/// Cancels one derived job that no longer matches the active configuration.
pub(crate) fn cancel_reconfigured(
    conn: &Connection,
    job_id: &str,
    error: Option<&str>,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'cancelled', pause_requested = 0,
         cancel_requested = 0, error = ?2, updated_at = ?3, finished_at = ?3
         WHERE id = ?1 AND status IN ('queued', 'running', 'paused', 'failed')",
        params![job_id, error, updated_at as i64],
    )
    .context("无法取消过期翻译任务")
}

pub(crate) fn insert(conn: &Connection, job: &IndexJob) -> Result<usize> {
    conn.execute(
        "INSERT INTO index_jobs
         (id, book_id, source_id, kind, status, pause_requested, cancel_requested,
          attempts, cursor_json, error, created_at, updated_at, started_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            job.id,
            job.book_id,
            job.source_id,
            job.kind,
            job.status.as_str(),
            job.pause_requested,
            job.cancel_requested,
            job.attempts,
            job.cursor_json,
            job.error,
            job.created_at as i64,
            job.updated_at as i64,
            job.started_at.map(|value| value as i64),
            job.finished_at.map(|value| value as i64),
        ],
    )
    .context("无法写入索引任务")
}

pub(crate) fn insert_if_absent(conn: &Connection, job: &IndexJob) -> Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO index_jobs
         (id, book_id, source_id, kind, status, pause_requested, cancel_requested,
          attempts, cursor_json, error, created_at, updated_at, started_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            job.id,
            job.book_id,
            job.source_id,
            job.kind,
            job.status.as_str(),
            job.pause_requested,
            job.cancel_requested,
            job.attempts,
            job.cursor_json,
            job.error,
            job.created_at as i64,
            job.updated_at as i64,
            job.started_at.map(|value| value as i64),
            job.finished_at.map(|value| value as i64),
        ],
    )
    .context("无法投递索引任务")
}

/// Returns interrupted work to the durable queue after a process restart.
/// Its cursor is preserved, so already committed batches are not lost.
///
/// A pause request is consumed here as well: the executor that was supposed
/// to observe it died with the process, and the flag alone would leave the
/// row unclaimable (`claim` requires `pause_requested = 0`) with no worker
/// owning it — the task window would show "正在暂停" forever, restart after
/// restart. Rows an older build left in that state (`queued` carrying the
/// flag) are repaired the same way; a flag on any other status is simply
/// dropped so the real status and its actions become visible again.
pub(crate) fn recover_interrupted(conn: &Connection, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs
         SET status = CASE
                 WHEN pause_requested = 1 AND status IN ('queued', 'running') THEN 'paused'
                 WHEN status = 'running' THEN 'queued'
                 ELSE status
             END,
             pause_requested = 0,
             error = CASE
                 WHEN pause_requested = 1 AND status IN ('queued', 'running') THEN NULL
                 WHEN status = 'running' THEN '应用退出时任务仍在执行，已从持久游标恢复'
                 ELSE error
             END,
             updated_at = ?1,
             finished_at = CASE WHEN status = 'running' THEN NULL ELSE finished_at END
         WHERE kind IN ('embedding', 'vision', 'translation')
           AND (status = 'running' OR pause_requested = 1)",
        [updated_at as i64],
    )
    .context("无法恢复中断的索引任务")
}

pub(crate) fn recover_interrupted_kind(
    conn: &Connection,
    kind: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs
         SET status = CASE
                 WHEN pause_requested = 1 AND status IN ('queued', 'running') THEN 'paused'
                 WHEN status = 'running' THEN 'queued'
                 ELSE status
             END,
             pause_requested = 0,
             error = CASE
                 WHEN pause_requested = 1 AND status IN ('queued', 'running') THEN NULL
                 WHEN status = 'running' THEN '应用退出时任务仍在执行，已重新进入持久队列'
                 ELSE error
             END,
             updated_at = ?2,
             finished_at = CASE WHEN status = 'running' THEN NULL ELSE finished_at END
         WHERE kind = ?1 AND (status = 'running' OR pause_requested = 1)",
        params![kind, updated_at as i64],
    )
    .context("无法恢复分类中断任务")
}

/// Atomically claims one queued job. A concurrent coordinator can never run
/// the same job because only the successful state transition returns one row.
pub(crate) fn claim(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    updated_at: u64,
) -> Result<bool> {
    conn.execute(
        "UPDATE index_jobs
         SET status = 'running', attempts = attempts + 1, error = NULL,
             updated_at = ?2, started_at = COALESCE(started_at, ?2), finished_at = NULL
         WHERE id = ?1 AND status = 'queued'
           AND cursor_json = ?3
           AND (kind <> 'vision' OR id = 'vision:' || source_id)
           AND pause_requested = 0 AND cancel_requested = 0",
        params![job_id, updated_at as i64, expected_cursor_json],
    )
    .context("无法领取索引任务")
    .map(|changed| changed == 1)
}

/// Binds an unclaimed canonical embedding/vision row to the active provider
/// contract. Status and control flags are intentionally preserved so a
/// cancellation requested before the first claim still publishes from the
/// bound cursor. A pause never reaches this path any more: it settles a
/// queued row to `paused` outright (see [`request_pause`]), and this
/// statement's `status = 'queued'` guard then refuses to touch that row.
/// The exact old cursor prevents a stale queue scan from rewriting a newer
/// configuration generation.
pub(crate) fn bind_queued_canonical_execution(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    cursor_json: &str,
    updated_at: u64,
) -> Result<bool> {
    conn.execute(
        "UPDATE index_jobs SET cursor_json = ?3, updated_at = ?4
         WHERE id = ?1 AND status = 'queued'
           AND kind IN ('embedding', 'vision')
           AND id = kind || ':' || source_id AND cursor_json = ?2",
        params![job_id, expected_cursor_json, cursor_json, updated_at as i64],
    )
    .context("无法绑定索引任务执行身份")
    .map(|changed| changed == 1)
}

/// Binds an unclaimed queued translation row to the canonical cursor encoding
/// the executor writes.
///
/// Translation cursors come from two writers: the executor, which serializes
/// the whole block-level cursor, and the reconfiguration path, which persists
/// the smaller [`PersistedIndexCursor`](crate::db::transactions::PersistedIndexCursor) shape. Every executor comparison is
/// JSON string equality, so a job configured by the latter has to be re-encoded
/// before its first claim — otherwise that execution abandons itself and the
/// row stays `running` with no worker. The exact old cursor keeps a stale queue
/// scan from rewriting a newer generation; status and control flags are
/// preserved so a cancellation requested before the claim still wins (a pause
/// settles a queued row to `paused` inside [`request_pause`] and never leaves
/// a flag here for a claim to trip over).
pub(crate) fn bind_queued_translation_cursor(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    cursor_json: &str,
    updated_at: u64,
) -> Result<bool> {
    conn.execute(
        "UPDATE index_jobs SET cursor_json = ?3, updated_at = ?4
         WHERE id = ?1 AND status = 'queued' AND kind = 'translation'
           AND cursor_json = ?2",
        params![job_id, expected_cursor_json, cursor_json, updated_at as i64],
    )
    .context("无法绑定翻译任务游标")
    .map(|changed| changed == 1)
}

/// Cancels exactly the queued execution generation observed by the worker.
/// This prevents a stale post-vision row from remaining at the head of the
/// queue after its embedding or vision input contract has changed.
pub(crate) fn cancel_queued_from_cursor(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    error: &str,
    updated_at: u64,
) -> Result<bool> {
    conn.execute(
        "UPDATE index_jobs
         SET status = 'cancelled', pause_requested = 0, cancel_requested = 0,
             error = ?3, updated_at = ?4, finished_at = ?4
         WHERE id = ?1 AND status = 'queued' AND cursor_json = ?2",
        params![job_id, expected_cursor_json, error, updated_at as i64],
    )
    .context("无法取消过期索引任务代次")
    .map(|changed| changed == 1)
}

pub(crate) fn advance_running_cursor_from(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    cursor_json: &str,
    updated_at: u64,
) -> Result<bool> {
    conn.execute(
        "UPDATE index_jobs SET cursor_json = ?3, updated_at = ?4, error = NULL
         WHERE id = ?1 AND status = 'running' AND cursor_json = ?2
           AND pause_requested = 0 AND cancel_requested = 0",
        params![job_id, expected_cursor_json, cursor_json, updated_at as i64],
    )
    .context("无法推进 embedding 任务游标")
    .map(|changed| changed == 1)
}

/// Requests a pause. A queued job has no executor that could ever observe the
/// flag, so it is settled to `paused` right here: the flag alone would make
/// the row unclaimable (`claim` requires `pause_requested = 0`) while nobody
/// owns it, and the task window would then show "正在暂停" forever with every
/// action hidden (现场 2026-10-17：排队中的整本翻译点了暂停一直显示暂停中，
/// 重启之后仍旧 —— `recover_interrupted` 那时还会把 flag 原样留在行上).
/// A running job keeps the flag so the executor that owns the cursor
/// publishes `paused` from the position it last persisted.
pub(crate) fn request_pause(conn: &Connection, job_id: &str, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs
         SET status = CASE WHEN status = 'queued' THEN 'paused' ELSE status END,
             pause_requested = CASE WHEN status = 'queued' THEN 0 ELSE 1 END,
             error = CASE WHEN status = 'queued' THEN NULL ELSE error END,
             updated_at = ?2
         WHERE id = ?1 AND status IN ('queued', 'running') AND cancel_requested = 0",
        params![job_id, updated_at as i64],
    )
    .context("无法暂停索引任务")
}

pub(crate) fn request_cancel(conn: &Connection, job_id: &str, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs
         SET status = CASE WHEN status = 'paused' THEN 'cancelled' ELSE status END,
             cancel_requested = CASE WHEN status = 'paused' THEN 0 ELSE 1 END,
             pause_requested = 0,
             error = CASE WHEN status = 'paused' THEN NULL ELSE error END,
             updated_at = ?2,
             finished_at = CASE WHEN status = 'paused' THEN ?2 ELSE finished_at END
         WHERE id = ?1 AND status IN ('queued', 'running', 'paused')",
        params![job_id, updated_at as i64],
    )
    .context("无法取消索引任务")
}

/// Cancels source-derived work that can no longer publish results for the
/// current document revision. Completed history is retained for diagnostics.
pub(crate) fn cancel_superseded_for_book(
    conn: &Connection,
    book_id: &str,
    current_source_id: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs
         SET status = 'cancelled', cancel_requested = 0, pause_requested = 0,
             updated_at = ?3, finished_at = ?3
         WHERE book_id = ?1 AND source_id <> ?2
           AND kind IN ('embedding', 'vision', 'visual_render', 'translation')
           AND status IN ('queued', 'running', 'paused')",
        params![book_id, current_source_id, updated_at as i64],
    )
    .context("无法取消已经过期的派生索引任务")
}

pub(crate) fn resume(conn: &Connection, job_id: &str, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'queued', pause_requested = 0,
         cancel_requested = 0, error = NULL, updated_at = ?2, finished_at = NULL
         WHERE id = ?1 AND status = 'paused'
           AND (kind <> 'vision' OR id = 'vision:' || source_id)",
        params![job_id, updated_at as i64],
    )
    .context("无法恢复索引任务")
}

pub(crate) fn retry(conn: &Connection, job_id: &str, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'queued', pause_requested = 0,
         cancel_requested = 0, error = NULL, updated_at = ?2, finished_at = NULL
         WHERE id = ?1 AND status IN ('failed', 'cancelled')
           AND (kind <> 'vision' OR id = 'vision:' || source_id)",
        params![job_id, updated_at as i64],
    )
    .context("无法重试索引任务")
}

/// Retries one translation job from the caller-supplied cursor.
///
/// 「重试」是从断点继续整本，不是重做上一次的收窄：取消或失败可能把一次没跑完的
/// 「立即翻译本章」或逐块「重新翻译」留在游标里（连同 `force_retry`）。沿用它会让已经
/// 有译文的目标块被强制重发，或被锁在那一章里不再碰别的章节。调用方在写回之前清掉
/// 收窄，只保留推进点，并用当前模型/执行身份覆盖游标里的旧值。
pub(crate) fn retry_translation(
    conn: &Connection,
    job_id: &str,
    cursor_json: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'queued', pause_requested = 0,
         cancel_requested = 0, cursor_json = ?2, error = NULL, updated_at = ?3,
         finished_at = NULL
         WHERE id = ?1 AND kind = 'translation' AND status IN ('failed', 'cancelled')",
        params![job_id, cursor_json, updated_at as i64],
    )
    .context("无法重试翻译任务")
}

pub(crate) fn retry_failed_for_source_kind(
    conn: &Connection,
    source_id: &str,
    kind: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'queued', pause_requested = 0,
         cancel_requested = 0, error = NULL, updated_at = ?3, finished_at = NULL
         WHERE source_id = ?1 AND kind = ?2 AND status = 'failed'
           AND (kind <> 'vision' OR (
               id = 'vision:' || source_id AND error = ?4
           ))",
        params![
            source_id,
            kind,
            updated_at as i64,
            VISION_WAITING_FOR_PAGES_ERROR
        ],
    )
    .context("无法重试来源的索引任务")
}

/// Resets a terminal derived job with a caller-supplied execution cursor.
/// Active work is deliberately never overwritten by this generic helper;
/// canonical vision replacement uses its guarded cross-table transaction.
pub(crate) fn reset_terminal(
    conn: &Connection,
    job_id: &str,
    cursor_json: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'queued', pause_requested = 0,
         cancel_requested = 0, cursor_json = ?2, error = NULL, updated_at = ?3,
         started_at = NULL, finished_at = NULL
         WHERE id = ?1 AND status IN ('succeeded', 'failed', 'cancelled')",
        params![job_id, cursor_json, updated_at as i64],
    )
    .context("无法为新模型重新投递索引任务")
}

/// Resets one translation job for a per-block retry.
///
/// Unlike [`reset_terminal`], this also accepts `paused`: new translation jobs
/// start paused (auto-run is off by default), and repairing a failed block before
/// resuming the task is the normal case.
///
/// `queued` is accepted too (2026-10-15): nothing owns the cursor yet, so a retry
/// planned now is exactly what the next claim reads. The update stays guarded
/// by its `WHERE` clause, so a worker that claims the row in the same moment
/// makes this statement match nothing instead of overwriting a running cursor —
/// the caller sees a refusal and can pause the task and ask again. A `running`
/// job is still refused outright: the executor owns the cursor and the caller
/// must pause it first.
pub(crate) fn reset_for_translation_block_retry(
    conn: &Connection,
    job_id: &str,
    cursor_json: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = 'queued', pause_requested = 0,
         cancel_requested = 0, cursor_json = ?2, error = NULL, updated_at = ?3,
         started_at = NULL, finished_at = NULL
         WHERE id = ?1 AND status IN ('succeeded', 'failed', 'cancelled', 'paused', 'queued')",
        params![job_id, cursor_json, updated_at as i64],
    )
    .context("无法为重试文本块重新投递翻译任务")
}

pub(crate) fn update_state(
    conn: &Connection,
    job_id: &str,
    status: IndexJobStatus,
    cursor_json: &str,
    error: Option<&str>,
    updated_at: u64,
    started_at: Option<u64>,
    finished_at: Option<u64>,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = ?2, cursor_json = ?3, error = ?4,
         updated_at = ?5, started_at = COALESCE(?6, started_at), finished_at = ?7,
         attempts = CASE WHEN ?2 = 'running' AND status <> 'running' THEN attempts + 1 ELSE attempts END,
         pause_requested = CASE WHEN ?2 = 'paused' THEN 0 ELSE pause_requested END,
         cancel_requested = CASE WHEN ?2 = 'cancelled' THEN 0 ELSE cancel_requested END
         WHERE id = ?1",
        params![
            job_id,
            status.as_str(),
            cursor_json,
            error,
            updated_at as i64,
            started_at.map(|value| value as i64),
            finished_at.map(|value| value as i64),
        ],
    )
    .context("无法更新索引任务")
}

/// Compare-and-set variant used by background executors. It prevents a late
/// provider response from overwriting a cancellation published by a newer
/// document revision.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_state_from(
    conn: &Connection,
    job_id: &str,
    expected: IndexJobStatus,
    status: IndexJobStatus,
    cursor_json: &str,
    error: Option<&str>,
    updated_at: u64,
    started_at: Option<u64>,
    finished_at: Option<u64>,
) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET status = ?3, cursor_json = ?4, error = ?5,
         updated_at = ?6, started_at = COALESCE(?7, started_at), finished_at = ?8,
         attempts = CASE WHEN ?3 = 'running' AND status <> 'running' THEN attempts + 1 ELSE attempts END,
         pause_requested = CASE WHEN ?3 = 'paused' THEN 0 ELSE pause_requested END,
         cancel_requested = CASE WHEN ?3 = 'cancelled' THEN 0 ELSE cancel_requested END
         WHERE id = ?1 AND status = ?2",
        params![
            job_id,
            expected.as_str(),
            status.as_str(),
            cursor_json,
            error,
            updated_at as i64,
            started_at.map(|value| value as i64),
            finished_at.map(|value| value as i64),
        ],
    )
    .context("无法按当前状态更新索引任务")
}

/// Publishes the outcome of a running worker only while it still owns the
/// exact durable cursor. A pause/cancel request that arrived after the
/// worker's last poll wins atomically over the proposed outcome, and its flag
/// is consumed into the corresponding persisted state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_running_from_cursor(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    status: IndexJobStatus,
    cursor_json: &str,
    error: Option<&str>,
    updated_at: u64,
    finished_at: Option<u64>,
) -> Result<usize> {
    ensure!(
        matches!(
            status,
            IndexJobStatus::Paused
                | IndexJobStatus::Succeeded
                | IndexJobStatus::Failed
                | IndexJobStatus::Cancelled
        ),
        "运行任务终态无效"
    );
    conn.execute(
        "UPDATE index_jobs
         SET status = CASE
                 WHEN cancel_requested = 1 THEN 'cancelled'
                 WHEN pause_requested = 1 THEN 'paused'
                 ELSE ?3
             END,
             cursor_json = ?4,
             error = CASE
                 WHEN cancel_requested = 1 OR pause_requested = 1 THEN NULL
                 ELSE ?5
             END,
             updated_at = ?6,
             finished_at = CASE
                 WHEN cancel_requested = 1 THEN ?6
                 WHEN pause_requested = 1 THEN NULL
                 ELSE ?7
             END,
             pause_requested = 0,
             cancel_requested = 0
         WHERE id = ?1 AND status = 'running' AND cursor_json = ?2",
        params![
            job_id,
            expected_cursor_json,
            status.as_str(),
            cursor_json,
            error,
            updated_at as i64,
            finished_at.map(|value| value as i64),
        ],
    )
    .context("无法提交运行中索引任务结果")
}

pub(crate) fn delete(conn: &Connection, job_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM index_jobs WHERE id = ?1", [job_id])
        .context("无法删除索引任务")
}

pub(crate) fn delete_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM index_jobs WHERE source_id = ?1", [source_id])
        .context("无法删除来源对应的索引任务")
}
