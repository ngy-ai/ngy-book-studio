use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OfficeEnhancement {
    pub(crate) book_id: String,
    pub(crate) enabled: bool,
    pub(crate) updated_at: u64,
}

pub(crate) fn get(conn: &Connection, book_id: &str) -> Result<Option<OfficeEnhancement>> {
    conn.query_row(
        "SELECT book_id, enabled, updated_at
         FROM office_enhancements WHERE book_id = ?1",
        [book_id],
        |row| {
            Ok(OfficeEnhancement {
                book_id: row.get(0)?,
                enabled: row.get(1)?,
                updated_at: row.get::<_, i64>(2)? as u64,
            })
        },
    )
    .optional()
    .context("无法读取 Office 增强预览设置")
}

pub(crate) fn is_enabled(conn: &Connection, book_id: &str) -> Result<bool> {
    Ok(get(conn, book_id)?.is_some_and(|setting| setting.enabled))
}

pub(crate) fn upsert(
    conn: &Connection,
    book_id: &str,
    enabled: bool,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "INSERT INTO office_enhancements (book_id, enabled, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(book_id) DO UPDATE SET
             enabled = excluded.enabled,
             updated_at = excluded.updated_at",
        params![book_id, enabled, updated_at as i64],
    )
    .context("无法保存 Office 增强预览设置")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn setting_is_scoped_to_a_book_and_cascades_on_delete() {
        let temp = tempfile::tempdir().unwrap();
        let conn = db::open_or_recreate(&temp.path().join("office-setting.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO blobs (object_key, media_type, byte_len, hash, created_at)
             VALUES ('objects/source', 'application/octet-stream', 1, 'hash', 1);
             INSERT INTO books
             (id, title, author, format, revision, source_object_key, added_at, updated_at)
             VALUES ('book-1', 'Office', 'Author', 'pptx', 0, 'objects/source', 1, 1);",
        )
        .unwrap();

        assert!(!is_enabled(&conn, "book-1").unwrap());
        upsert(&conn, "book-1", true, 2).unwrap();
        assert!(is_enabled(&conn, "book-1").unwrap());
        assert_eq!(get(&conn, "book-1").unwrap().unwrap().updated_at, 2);

        conn.execute("DELETE FROM books WHERE id = 'book-1'", [])
            .unwrap();
        assert!(get(&conn, "book-1").unwrap().is_none());
    }
}
