use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    /// Thread-local so parallel tests cannot consume another test's injected
    /// failure. This targets only `get`, which used to be called after a
    /// document transaction had already committed.
    static FAIL_GET_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Catalog projection for the current document revision. Binary payloads are
/// addressed by object key and must be loaded through `LocalBlobStore`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookRecord {
    pub id: String,
    pub title: String,
    pub author: String,
    pub language: Option<String>,
    pub description: Option<String>,
    pub format: String,
    pub revision: u64,
    pub source_object_key: String,
    pub cover_asset_id: Option<String>,
    pub cover_object_key: Option<String>,
    pub cover_mime: Option<String>,
    pub added_at: u64,
    pub updated_at: u64,
    pub last_spine: usize,
    /// A book belongs to at most one group, and may belong to none at all.
    pub group_id: Option<String>,
}

pub(crate) struct BookCatalogUpdate<'a> {
    pub(crate) title: &'a str,
    pub(crate) author: &'a str,
    pub(crate) language: Option<&'a str>,
    pub(crate) description: Option<&'a str>,
    pub(crate) updated_at: u64,
}

const SELECT: &str =
    "SELECT b.id, b.title, b.author, b.language, b.description, b.format, b.revision,
    b.source_object_key, b.cover_asset_id, cover.object_key, cover.media_type,
    b.added_at, b.updated_at, COALESCE(p.spine_index, 0), b.group_id
    FROM books b
    LEFT JOIN progress p ON p.book_id = b.id
    LEFT JOIN assets cover ON cover.id = b.cover_asset_id";

pub(crate) fn list(conn: &Connection) -> Result<Vec<BookRecord>> {
    let mut stmt = conn
        .prepare(&format!("{SELECT} ORDER BY b.added_at DESC, b.id"))
        .context("无法准备图书查询")?;
    let rows = stmt
        .query_map([], book_record_from_row)
        .context("无法读取图书")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取图书记录")
}

pub(crate) fn get(conn: &Connection, book_id: &str) -> Result<Option<BookRecord>> {
    #[cfg(test)]
    if FAIL_GET_AFTER.with(|countdown| match countdown.get() {
        Some(0) => {
            countdown.set(None);
            true
        }
        Some(remaining) => {
            countdown.set(Some(remaining - 1));
            false
        }
        None => false,
    }) {
        anyhow::bail!("测试注入的图书记录读取失败");
    }
    conn.query_row(
        &format!("{SELECT} WHERE b.id = ?1"),
        [book_id],
        book_record_from_row,
    )
    .optional()
    .context("无法读取图书记录")
}

#[cfg(test)]
pub(crate) fn fail_get_after_for_test(successful_gets: usize) {
    FAIL_GET_AFTER.set(Some(successful_gets));
}

pub(crate) fn get_by_source_object_key(
    conn: &Connection,
    object_key: &str,
) -> Result<Option<BookRecord>> {
    conn.query_row(
        &format!("{SELECT} WHERE b.source_object_key = ?1"),
        [object_key],
        book_record_from_row,
    )
    .optional()
    .context("无法按来源对象读取图书")
}

pub(crate) fn insert(conn: &Connection, record: &BookRecord) -> Result<usize> {
    conn.execute(
        "INSERT INTO books
         (id, title, author, language, description, format, revision, source_object_key,
          cover_asset_id, added_at, updated_at, group_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.id,
            record.title,
            record.author,
            record.language,
            record.description,
            record.format,
            record.revision as i64,
            record.source_object_key,
            record.cover_asset_id,
            record.added_at as i64,
            record.updated_at as i64,
            record.group_id,
        ],
    )
    .context("无法将图书写入数据库")
}

pub(crate) fn update_catalog(
    conn: &Connection,
    book_id: &str,
    update: &BookCatalogUpdate<'_>,
) -> Result<usize> {
    conn.execute(
        "UPDATE books SET title = ?2, author = ?3, language = ?4, description = ?5,
         updated_at = ?6 WHERE id = ?1",
        params![
            book_id,
            update.title,
            update.author,
            update.language,
            update.description,
            update.updated_at as i64
        ],
    )
    .context("无法保存图书元数据")
}

/// Fills in a language an earlier import dropped, for the startup backfill.
/// It never touches `revision` or `updated_at`: a repair must not invalidate
/// derived indexes, translations or annotations, and must not reshuffle
/// "recently updated" ordering. Guarded on a blank language so it can never
/// overwrite a value a newer importer, the reader or a future UI already set.
pub(crate) fn backfill_language(conn: &Connection, book_id: &str, language: &str) -> Result<usize> {
    conn.execute(
        "UPDATE books SET language = ?2
         WHERE id = ?1 AND (language IS NULL OR trim(language) = '')",
        params![book_id, language],
    )
    .context("无法回填图书语言")
}

pub(crate) fn set_current_source(
    conn: &Connection,
    book_id: &str,
    format: &str,
    revision: u64,
    source_object_key: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE books SET format = ?2, revision = ?3, source_object_key = ?4,
         updated_at = ?5 WHERE id = ?1",
        params![
            book_id,
            format,
            revision as i64,
            source_object_key,
            updated_at as i64,
        ],
    )
    .context("无法切换图书当前来源")
}

pub(crate) fn set_cover_asset(
    conn: &Connection,
    book_id: &str,
    cover_asset_id: Option<&str>,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE books SET cover_asset_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![book_id, cover_asset_id, updated_at as i64],
    )
    .context("无法更新图书封面")
}

pub(crate) fn set_group(conn: &Connection, book_id: &str, group_id: Option<&str>) -> Result<usize> {
    conn.execute(
        "UPDATE books SET group_id = ?1
         WHERE id = ?2
           AND (?1 IS NULL OR EXISTS (SELECT 1 FROM groups WHERE id = ?1))",
        params![group_id, book_id],
    )
    .context("无法移动图书")
}

pub(crate) fn exists(conn: &Connection, book_id: &str) -> Result<bool> {
    conn.query_row("SELECT 1 FROM books WHERE id = ?1", [book_id], |_| Ok(()))
        .optional()
        .context("无法确认图书是否存在")
        .map(|row| row.is_some())
}

pub(crate) fn clear_groups_in_subtree(conn: &Connection, root_group_id: &str) -> Result<usize> {
    conn.execute(
        "WITH RECURSIVE subtree(id) AS (
           SELECT id FROM groups WHERE id = ?1
           UNION SELECT groups.id FROM groups JOIN subtree ON groups.parent_id = subtree.id
         )
         UPDATE books SET group_id = NULL WHERE group_id IN (SELECT id FROM subtree)",
        [root_group_id],
    )
    .context("无法移出待删除分组中的图书")
}

pub(crate) fn delete(conn: &Connection, book_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM books WHERE id = ?1", [book_id])
        .context("无法从数据库删除图书")
}

fn book_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BookRecord> {
    Ok(BookRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        author: row.get(2)?,
        language: row.get(3)?,
        description: row.get(4)?,
        format: row.get(5)?,
        revision: row.get::<_, i64>(6)? as u64,
        source_object_key: row.get(7)?,
        cover_asset_id: row.get(8)?,
        cover_object_key: row.get(9)?,
        cover_mime: row.get(10)?,
        added_at: row.get::<_, i64>(11)? as u64,
        updated_at: row.get::<_, i64>(12)? as u64,
        last_spine: row.get::<_, i64>(13)? as usize,
        group_id: row.get(14)?,
    })
}
