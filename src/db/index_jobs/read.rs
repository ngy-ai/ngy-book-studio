//! Reads of the `index_jobs` table.

use super::*;

use super::model::{SELECT, job_from_row};

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
             ORDER BY COALESCE(
                          json_extract(
                              CASE WHEN json_valid(cursor_json) THEN cursor_json END,
                              '$.priority'
                          ),
                          0
                      ) DESC,
                      created_at,
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
