use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContentUnit {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) ordinal: usize,
    pub(crate) kind: String,
    pub(crate) href: Option<String>,
    pub(crate) source_locator_json: String,
    pub(crate) title: Option<String>,
    pub(crate) media_type: Option<String>,
    pub(crate) source_text: Option<String>,
    pub(crate) block_json: String,
    pub(crate) revision: u64,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
}

const SELECT: &str = "SELECT id, book_id, source_id, parent_id, ordinal, kind,
    href, source_locator_json, title, media_type, source_text, block_json, revision, created_at, updated_at
    FROM content_units";

pub(crate) fn get(conn: &Connection, unit_id: &str) -> Result<Option<ContentUnit>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), [unit_id], unit_from_row)
        .optional()
        .context("无法读取内容单元")
}

pub(crate) fn list_for_source(conn: &Connection, source_id: &str) -> Result<Vec<ContentUnit>> {
    let mut stmt = conn
        .prepare(&format!("{SELECT} WHERE source_id = ?1 ORDER BY ordinal"))
        .context("无法准备内容单元查询")?;
    let rows = stmt
        .query_map([source_id], unit_from_row)
        .context("无法读取内容单元")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取内容单元记录")
}

pub(crate) fn insert(conn: &Connection, unit: &ContentUnit) -> Result<usize> {
    conn.execute(
        "INSERT INTO content_units
         (id, book_id, source_id, parent_id, ordinal, kind, href,
          source_locator_json, title, media_type, source_text, block_json, revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            unit.id,
            unit.book_id,
            unit.source_id,
            unit.parent_id,
            unit.ordinal as i64,
            unit.kind,
            unit.href,
            unit.source_locator_json,
            unit.title,
            unit.media_type,
            unit.source_text,
            unit.block_json,
            unit.revision as i64,
            unit.created_at as i64,
            unit.updated_at as i64,
        ],
    )
    .context("无法写入内容单元")
}

pub(crate) fn update_content(conn: &Connection, unit: &ContentUnit) -> Result<usize> {
    conn.execute(
        "UPDATE content_units SET parent_id = ?2, ordinal = ?3, kind = ?4,
         href = ?5, source_locator_json = ?6, title = ?7, media_type = ?8,
         source_text = ?9, block_json = ?10, revision = ?11, updated_at = ?12 WHERE id = ?1",
        params![
            unit.id,
            unit.parent_id,
            unit.ordinal as i64,
            unit.kind,
            unit.href,
            unit.source_locator_json,
            unit.title,
            unit.media_type,
            unit.source_text,
            unit.block_json,
            unit.revision as i64,
            unit.updated_at as i64,
        ],
    )
    .context("无法更新内容单元")
}

pub(crate) fn delete_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM content_units WHERE source_id = ?1",
        [source_id],
    )
    .context("无法删除来源内容单元")
}

fn unit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContentUnit> {
    Ok(ContentUnit {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        parent_id: row.get(3)?,
        ordinal: row.get::<_, i64>(4)? as usize,
        kind: row.get(5)?,
        href: row.get(6)?,
        source_locator_json: row.get(7)?,
        title: row.get(8)?,
        media_type: row.get(9)?,
        source_text: row.get(10)?,
        block_json: row.get(11)?,
        revision: row.get::<_, i64>(12)? as u64,
        created_at: row.get::<_, i64>(13)? as u64,
        updated_at: row.get::<_, i64>(14)? as u64,
    })
}
