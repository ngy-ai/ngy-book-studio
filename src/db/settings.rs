use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// Settings key of one book's reading-time translation display choice. The
/// `settings` table has no foreign key, so whoever deletes a book must delete
/// this key with it.
pub(crate) fn translation_display_book_key(book_id: &str) -> String {
    format!("translation.display.book.v1.{book_id}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setting {
    pub(crate) key: String,
    pub(crate) value_json: String,
    pub(crate) updated_at: u64,
}

pub(crate) fn get(conn: &Connection, key: &str) -> Result<Option<Setting>> {
    conn.query_row(
        "SELECT key, value_json, updated_at FROM settings WHERE key = ?1",
        [key],
        setting_from_row,
    )
    .optional()
    .context("无法读取设置")
}

pub(crate) fn list(conn: &Connection) -> Result<Vec<Setting>> {
    let mut stmt = conn
        .prepare("SELECT key, value_json, updated_at FROM settings ORDER BY key")
        .context("无法准备设置查询")?;
    let rows = stmt
        .query_map([], setting_from_row)
        .context("无法读取设置")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取设置记录")
}

pub(crate) fn upsert(conn: &Connection, setting: &Setting) -> Result<usize> {
    conn.execute(
        "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET
             value_json = excluded.value_json,
             updated_at = excluded.updated_at",
        params![setting.key, setting.value_json, setting.updated_at as i64],
    )
    .context("无法保存设置")
}

pub(crate) fn delete(conn: &Connection, key: &str) -> Result<usize> {
    conn.execute("DELETE FROM settings WHERE key = ?1", [key])
        .context("无法删除设置")
}

fn setting_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Setting> {
    Ok(Setting {
        key: row.get(0)?,
        value_json: row.get(1)?,
        updated_at: row.get::<_, i64>(2)? as u64,
    })
}
