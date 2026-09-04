use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReadingProgress {
    pub(crate) book_id: String,
    pub(crate) source_revision: u64,
    pub(crate) content_unit_id: Option<String>,
    pub(crate) spine_index: usize,
    pub(crate) locator_json: String,
    pub(crate) fraction: f64,
    pub(crate) updated_at: u64,
}

pub(crate) fn get(conn: &Connection, book_id: &str) -> Result<Option<ReadingProgress>> {
    conn.query_row(
        "SELECT book_id, source_revision, content_unit_id, spine_index, locator_json, fraction, updated_at
         FROM progress WHERE book_id = ?1",
        [book_id],
        progress_from_row,
    )
    .optional()
    .context("无法读取阅读进度")
}

pub(crate) fn upsert(conn: &Connection, progress: &ReadingProgress) -> Result<usize> {
    conn.execute(
        "INSERT INTO progress
         (book_id, source_revision, content_unit_id, spine_index, locator_json, fraction, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(book_id) DO UPDATE SET
             source_revision = excluded.source_revision,
             content_unit_id = excluded.content_unit_id,
             spine_index = excluded.spine_index,
             locator_json = excluded.locator_json,
             fraction = excluded.fraction,
             updated_at = excluded.updated_at",
        params![
            progress.book_id,
            progress.source_revision as i64,
            progress.content_unit_id,
            progress.spine_index as i64,
            progress.locator_json,
            progress.fraction,
            progress.updated_at as i64,
        ],
    )
    .context("无法保存阅读进度")
}

pub(crate) fn delete(conn: &Connection, book_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM progress WHERE book_id = ?1", [book_id])
        .context("无法删除阅读进度")
}

fn progress_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReadingProgress> {
    Ok(ReadingProgress {
        book_id: row.get(0)?,
        source_revision: row.get::<_, i64>(1)? as u64,
        content_unit_id: row.get(2)?,
        spine_index: row.get::<_, i64>(3)? as usize,
        locator_json: row.get(4)?,
        fraction: row.get(5)?,
        updated_at: row.get::<_, i64>(6)? as u64,
    })
}
