use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TocEntry {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) ordinal: usize,
    pub(crate) depth: usize,
    pub(crate) label: String,
    pub(crate) href: Option<String>,
    pub(crate) content_unit_id: String,
    pub(crate) target_block_id: Option<String>,
    pub(crate) locator_json: String,
    pub(crate) created_at: u64,
}

const SELECT: &str = "SELECT id, book_id, source_id, parent_id, ordinal, depth, label, href,
    content_unit_id, target_block_id, locator_json, created_at FROM toc_entries";

pub(crate) fn get(conn: &Connection, entry_id: &str) -> Result<Option<TocEntry>> {
    conn.query_row(
        &format!("{SELECT} WHERE id = ?1"),
        [entry_id],
        entry_from_row,
    )
    .optional()
    .context("无法读取目录项")
}

pub(crate) fn list_for_source(conn: &Connection, source_id: &str) -> Result<Vec<TocEntry>> {
    let mut stmt = conn
        .prepare(&format!("{SELECT} WHERE source_id = ?1 ORDER BY ordinal"))
        .context("无法准备目录查询")?;
    let rows = stmt
        .query_map([source_id], entry_from_row)
        .context("无法读取目录")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取目录记录")
}

pub(crate) fn insert(conn: &Connection, entry: &TocEntry) -> Result<usize> {
    conn.execute(
        "INSERT INTO toc_entries
         (id, book_id, source_id, parent_id, ordinal, depth, label, href,
          content_unit_id, target_block_id, locator_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            entry.id,
            entry.book_id,
            entry.source_id,
            entry.parent_id,
            entry.ordinal as i64,
            entry.depth as i64,
            entry.label,
            entry.href,
            entry.content_unit_id,
            entry.target_block_id,
            entry.locator_json,
            entry.created_at as i64,
        ],
    )
    .context("无法写入目录项")
}

pub(crate) fn delete_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM toc_entries WHERE source_id = ?1", [source_id])
        .context("无法删除来源目录")
}

fn entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TocEntry> {
    Ok(TocEntry {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        parent_id: row.get(3)?,
        ordinal: row.get::<_, i64>(4)? as usize,
        depth: row.get::<_, i64>(5)? as usize,
        label: row.get(6)?,
        href: row.get(7)?,
        content_unit_id: row.get(8)?,
        target_block_id: row.get(9)?,
        locator_json: row.get(10)?,
        created_at: row.get::<_, i64>(11)? as u64,
    })
}
