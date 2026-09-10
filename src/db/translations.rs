use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};

/// One persisted, revision-scoped translation of a single block. Rows are keyed
/// by `(book_id, content_unit_id, block_id, target_language)` so the reader can
/// look up a whole chapter in one query, and the background job can replace a
/// block in place when the document revision or model changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Translation {
    pub block_id: String,
    pub ordinal: u64,
    pub document_revision: u64,
    pub unit_revision: u64,
    pub model: String,
    pub source_text: String,
    pub translated_text: String,
}

/// Insert/update payload. The primary key is derived deterministically from the
/// scope so repeated translations of the same block replace one row instead of
/// accumulating history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewTranslation {
    pub book_id: String,
    pub content_unit_id: String,
    pub block_id: String,
    pub ordinal: u64,
    pub document_revision: u64,
    pub unit_revision: u64,
    pub target_language: String,
    pub source_language: Option<String>,
    pub model: String,
    pub source_text: String,
    pub translated_text: String,
    pub created_at: u64,
    pub updated_at: u64,
}

const SELECT: &str = "SELECT block_id, ordinal, document_revision, unit_revision,
    model, source_text, translated_text FROM translations";

/// Stable surrogate key for one translation scope. Derived so an upsert can
/// reuse the canonical row without churning the primary key.
fn translation_id(
    book_id: &str,
    content_unit_id: &str,
    block_id: &str,
    target_language: &str,
) -> String {
    format!(
        "translation-{}",
        blake3::hash(
            format!("{book_id}\0{content_unit_id}\0{block_id}\0{target_language}").as_bytes()
        )
        .to_hex()
    )
}

pub(crate) fn list_for_unit(
    conn: &Connection,
    content_unit_id: &str,
    target_language: &str,
) -> Result<Vec<Translation>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE content_unit_id = ?1 AND target_language = ?2 ORDER BY ordinal, block_id"
        ))
        .context("无法准备章节译文查询")?;
    stmt.query_map(params![content_unit_id, target_language], from_row)
        .context("无法读取章节译文")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("无法解析章节译文记录")
}

pub(crate) fn upsert(conn: &Connection, translation: &NewTranslation) -> Result<()> {
    let id = translation_id(
        &translation.book_id,
        &translation.content_unit_id,
        &translation.block_id,
        &translation.target_language,
    );
    conn.execute(
        "INSERT INTO translations (id, book_id, content_unit_id, block_id, ordinal,
            document_revision, unit_revision, target_language, source_language, model,
            source_text, translated_text, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(book_id, content_unit_id, block_id, target_language) DO UPDATE SET
            ordinal = excluded.ordinal,
            document_revision = excluded.document_revision,
            unit_revision = excluded.unit_revision,
            source_language = excluded.source_language,
            model = excluded.model,
            source_text = excluded.source_text,
            translated_text = excluded.translated_text,
            updated_at = excluded.updated_at",
        params![
            id,
            translation.book_id,
            translation.content_unit_id,
            translation.block_id,
            translation.ordinal as i64,
            translation.document_revision as i64,
            translation.unit_revision as i64,
            translation.target_language,
            translation.source_language,
            translation.model,
            translation.source_text,
            translation.translated_text,
            translation.created_at as i64,
            translation.updated_at as i64,
        ],
    )
    .context("无法保存译文")?;
    Ok(())
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Translation> {
    Ok(Translation {
        block_id: row.get(0)?,
        ordinal: row.get::<_, i64>(1)? as u64,
        document_revision: row.get::<_, i64>(2)? as u64,
        unit_revision: row.get::<_, i64>(3)? as u64,
        model: row.get(4)?,
        source_text: row.get(5)?,
        translated_text: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, DATABASE_FILE};

    fn fixture() -> (tempfile::TempDir, Connection) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = db::open_or_recreate(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'source-hash', 1);
             INSERT INTO books(id, title, author, format, revision, source_object_key,
                               added_at, updated_at)
                 VALUES ('book', 'Book', '', 'epub', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'epub', 'original', 'objects/source', 1);
             INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                 source_locator_json, block_json, revision, created_at, updated_at)
                 VALUES ('unit', 'book', 'source', 0, 'chapter', '{}',
                         '{\"schema_version\":1,\"blocks\":[]}', 1, 1, 1);",
        )
        .unwrap();
        (temp, conn)
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM translations", [], |row| row.get(0))
            .unwrap()
    }

    fn sample(block_id: &str, ordinal: u64, translated: &str) -> NewTranslation {
        NewTranslation {
            book_id: "book".into(),
            content_unit_id: "unit".into(),
            block_id: block_id.into(),
            ordinal,
            document_revision: 1,
            unit_revision: 1,
            target_language: "zh-Hans".into(),
            source_language: Some("en".into()),
            model: "chat-model".into(),
            source_text: block_id.to_string(),
            translated_text: translated.into(),
            created_at: 10,
            updated_at: 10,
        }
    }

    #[test]
    fn upsert_replaces_same_scope_and_lists_in_ordinal_order() {
        let (_temp, conn) = fixture();
        upsert(&conn, &sample("block-b", 1, "乙")).unwrap();
        upsert(&conn, &sample("block-a", 0, "甲")).unwrap();

        let rows = list_for_unit(&conn, "unit", "zh-Hans").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].block_id, "block-a");
        assert_eq!(rows[0].translated_text, "甲");
        assert_eq!(rows[1].block_id, "block-b");

        let mut updated = sample("block-a", 0, "甲二");
        updated.updated_at = 20;
        upsert(&conn, &updated).unwrap();
        let rows = list_for_unit(&conn, "unit", "zh-Hans").unwrap();
        assert_eq!(rows.len(), 2, "same scope must replace in place");
        assert_eq!(rows[0].translated_text, "甲二");
        assert_eq!(row_count(&conn), 2);
    }

    #[test]
    fn different_language_is_a_separate_scope() {
        let (_temp, conn) = fixture();
        upsert(&conn, &sample("block-a", 0, "甲")).unwrap();
        let mut japanese = sample("block-a", 0, "甲の日本語");
        japanese.target_language = "ja".into();
        upsert(&conn, &japanese).unwrap();

        assert_eq!(list_for_unit(&conn, "unit", "zh-Hans").unwrap().len(), 1);
        assert_eq!(list_for_unit(&conn, "unit", "ja").unwrap().len(), 1);
        conn.execute("DELETE FROM translations WHERE target_language = 'ja'", [])
            .unwrap();
        assert_eq!(list_for_unit(&conn, "unit", "zh-Hans").unwrap().len(), 1);
        assert!(list_for_unit(&conn, "unit", "ja").unwrap().is_empty());
    }

    #[test]
    fn deleting_the_book_cascades_translations() {
        let (_temp, mut conn) = fixture();
        upsert(&conn, &sample("block-a", 0, "甲")).unwrap();
        db::transactions::delete_document(&mut conn, "book").unwrap();
        assert_eq!(row_count(&conn), 0);
    }
}
