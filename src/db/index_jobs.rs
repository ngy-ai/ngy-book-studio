use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const VISION_WAITING_FOR_PAGES_ERROR: &str =
    "视觉页面尚未生成；页面渲染完成后可自动或手动重试";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexJobStatus {
    Queued,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

impl IndexJobStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(rusqlite::Error::InvalidColumnType(
                4,
                "status".to_string(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexJob {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) kind: String,
    pub(crate) status: IndexJobStatus,
    pub(crate) pause_requested: bool,
    pub(crate) cancel_requested: bool,
    pub(crate) attempts: u32,
    pub(crate) cursor_json: String,
    pub(crate) error: Option<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) started_at: Option<u64>,
    pub(crate) finished_at: Option<u64>,
}

const SELECT: &str = "SELECT id, book_id, source_id, kind, status, pause_requested,
    cancel_requested, attempts, cursor_json, error, created_at, updated_at, started_at,
    finished_at FROM index_jobs";

pub(crate) fn get(conn: &Connection, job_id: &str) -> Result<Option<IndexJob>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), [job_id], job_from_row)
        .optional()
        .context("无法读取索引任务")
}

pub(crate) fn list_for_book(conn: &Connection, book_id: &str) -> Result<Vec<IndexJob>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE book_id = ?1 ORDER BY created_at, id"
        ))
        .context("无法准备索引任务查询")?;
    let rows = stmt
        .query_map([book_id], job_from_row)
        .context("无法读取索引任务")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取索引任务记录")
}

pub(crate) fn list_for_source_kind(
    conn: &Connection,
    source_id: &str,
    kind: &str,
) -> Result<Vec<IndexJob>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE source_id = ?1 AND kind = ?2 ORDER BY created_at, id"
        ))
        .context("无法准备来源索引任务查询")?;
    let rows = stmt
        .query_map(params![source_id, kind], job_from_row)
        .context("无法读取来源索引任务")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取来源索引任务记录")
}

/// Returns queued work in a deterministic order. Control flags remain visible
/// so the coordinator can publish a terminal/paused state without claiming
/// the job first.
pub(crate) fn list_queued(conn: &Connection, limit: usize) -> Result<Vec<IndexJob>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE status = 'queued' AND kind IN ('embedding', 'vision', 'translation')
               AND (kind <> 'vision' OR id = 'vision:' || source_id)
             ORDER BY created_at,
                      CASE kind
                          WHEN 'embedding' THEN 0
                          WHEN 'vision' THEN 1
                          WHEN 'translation' THEN 2
                          ELSE 3
                      END,
                      id
             LIMIT ?1"
        ))
        .context("无法准备待执行索引任务查询")?;
    let rows = stmt
        .query_map([limit as i64], job_from_row)
        .context("无法读取待执行索引任务")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取待执行索引任务记录")
}

pub(crate) fn list_queued_ids_for_kind(
    conn: &Connection,
    kind: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM index_jobs
             WHERE kind = ?1 AND status = 'queued'
             ORDER BY created_at, id LIMIT ?2",
        )
        .context("无法准备分类待执行任务查询")?;
    let rows = stmt
        .query_map(params![kind, limit as i64], |row| row.get(0))
        .context("无法读取分类待执行任务")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取分类待执行任务记录")
}

pub(crate) fn list_by_kind(conn: &Connection, kind: &str) -> Result<Vec<IndexJob>> {
    let mut stmt = conn
        .prepare(&format!("{SELECT} WHERE kind = ?1 ORDER BY created_at, id"))
        .context("无法准备同类索引任务查询")?;
    let rows = stmt
        .query_map([kind], job_from_row)
        .context("无法读取同类索引任务")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取同类索引任务记录")
}

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
pub(crate) fn recover_interrupted(conn: &Connection, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs
         SET status = 'queued', updated_at = ?1, finished_at = NULL,
             error = '应用退出时任务仍在执行，已从持久游标恢复'
         WHERE status = 'running' AND kind IN ('embedding', 'vision', 'translation')",
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
         SET status = 'queued', updated_at = ?2, finished_at = NULL,
             error = '应用退出时任务仍在执行，已重新进入持久队列'
         WHERE kind = ?1 AND status = 'running'",
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
/// contract. Control flags and status are intentionally preserved so a pause
/// or cancellation requested before the first claim publishes a bound cursor.
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

pub(crate) fn request_pause(conn: &Connection, job_id: &str, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE index_jobs SET pause_requested = 1, updated_at = ?2
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
/// resuming the task is the normal case. A running job is still refused — the
/// caller must pause it first rather than race the executor's own cursor writes.
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
         WHERE id = ?1 AND status IN ('succeeded', 'failed', 'cancelled', 'paused')",
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

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexJob> {
    let status = row.get::<_, String>(4)?;
    Ok(IndexJob {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        kind: row.get(3)?,
        status: IndexJobStatus::parse(&status)?,
        pause_requested: row.get(5)?,
        cancel_requested: row.get(6)?,
        attempts: row.get(7)?,
        cursor_json: row.get(8)?,
        error: row.get(9)?,
        created_at: row.get::<_, i64>(10)? as u64,
        updated_at: row.get::<_, i64>(11)? as u64,
        started_at: row.get::<_, Option<i64>>(12)?.map(|value| value as u64),
        finished_at: row.get::<_, Option<i64>>(13)?.map(|value| value as u64),
    })
}
