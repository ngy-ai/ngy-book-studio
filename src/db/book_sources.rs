use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BookSource {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) revision: u64,
    pub(crate) format: String,
    pub(crate) source_kind: String,
    pub(crate) object_key: String,
    pub(crate) source_name: Option<String>,
    pub(crate) created_at: u64,
}

pub(crate) fn get(conn: &Connection, source_id: &str) -> Result<Option<BookSource>> {
    conn.query_row(
        "SELECT id, book_id, revision, format, source_kind, object_key, source_name, created_at
         FROM book_sources WHERE id = ?1",
        [source_id],
        source_from_row,
    )
    .optional()
    .context("无法读取图书来源")
}

pub(crate) fn get_revision(
    conn: &Connection,
    book_id: &str,
    revision: u64,
) -> Result<Option<BookSource>> {
    conn.query_row(
        "SELECT id, book_id, revision, format, source_kind, object_key, source_name, created_at
         FROM book_sources WHERE book_id = ?1 AND revision = ?2",
        params![book_id, revision as i64],
        source_from_row,
    )
    .optional()
    .context("无法读取图书版本来源")
}

pub(crate) fn list_for_book(conn: &Connection, book_id: &str) -> Result<Vec<BookSource>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, book_id, revision, format, source_kind, object_key, source_name, created_at
             FROM book_sources WHERE book_id = ?1 ORDER BY revision",
        )
        .context("无法准备图书来源查询")?;
    let rows = stmt
        .query_map([book_id], source_from_row)
        .context("无法读取图书来源")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取图书来源记录")
}

/// Lists only the source revision currently published by each book. Historical
/// sources stay addressable for exact-original export but must never receive
/// new model-derived index work.
pub(crate) fn list_current(conn: &Connection) -> Result<Vec<BookSource>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.book_id, s.revision, s.format, s.source_kind,
                    s.object_key, s.source_name, s.created_at
             FROM book_sources s
             JOIN books b ON b.id = s.book_id
                         AND b.revision = s.revision
             ORDER BY s.created_at, s.id",
        )
        .context("无法准备当前图书来源查询")?;
    let rows = stmt
        .query_map([], source_from_row)
        .context("无法读取当前图书来源")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取当前图书来源记录")
}

pub(crate) fn insert(conn: &Connection, source: &BookSource) -> Result<usize> {
    conn.execute(
        "INSERT INTO book_sources
         (id, book_id, revision, format, source_kind, object_key, source_name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            source.id,
            source.book_id,
            source.revision as i64,
            source.format,
            source.source_kind,
            source.object_key,
            source.source_name,
            source.created_at as i64,
        ],
    )
    .context("无法写入图书来源")
}

pub(crate) fn delete(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM book_sources WHERE id = ?1", [source_id])
        .context("无法删除图书来源")
}

fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BookSource> {
    Ok(BookSource {
        id: row.get(0)?,
        book_id: row.get(1)?,
        revision: row.get::<_, i64>(2)? as u64,
        format: row.get(3)?,
        source_kind: row.get(4)?,
        object_key: row.get(5)?,
        source_name: row.get(6)?,
        created_at: row.get::<_, i64>(7)? as u64,
    })
}
