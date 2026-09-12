use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};

/// One persisted, revision-scoped translation of a single block. Rows are keyed
/// by `(book_id, content_unit_id, block_id, target_language)` so the reader can
/// look up a whole chapter in one query, and the background job can replace a
/// block in place when the document revision or model changes.
///
/// `manual_text` carries the reader's own edit of the same block in the same
/// [`StoredTranslation`](crate::translation::StoredTranslation) shape. It lives
/// beside the machine text instead of replacing it, so the job never has to
/// know about it and "恢复机器译文" is a plain `NULL` write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Translation {
    pub block_id: String,
    pub ordinal: u64,
    pub document_revision: u64,
    pub unit_revision: u64,
    pub model: String,
    pub source_text: String,
    pub translated_text: String,
    pub manual_text: Option<String>,
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
    model, source_text, translated_text, manual_text FROM translations";

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

/// Machine path of the translation job. A conflicting row is replaced in place
/// and deliberately keeps `manual_text`: the reader's own edit of that block is
/// never a casualty of a later model run (or of a cache-hit republish).
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

/// Stores the reader's own translation of one block, or clears it with `None`
/// so the block falls back to the machine text.
///
/// The row must still describe the block being read: the update matches the
/// document and unit revisions the caller validated, and returns `false`
/// without writing when no row matches exactly (the chapter changed, the block
/// left the document, or the row was deleted by a re-translation).
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_manual_text(
    conn: &Connection,
    book_id: &str,
    content_unit_id: &str,
    block_id: &str,
    target_language: &str,
    document_revision: u64,
    unit_revision: u64,
    manual_text: Option<&str>,
    updated_at: u64,
) -> Result<bool> {
    let updated = conn
        .execute(
            "UPDATE translations SET manual_text = ?7, updated_at = MAX(?8, created_at)
             WHERE book_id = ?1 AND content_unit_id = ?2 AND block_id = ?3
               AND target_language = ?4 AND document_revision = ?5 AND unit_revision = ?6",
            params![
                book_id,
                content_unit_id,
                block_id,
                target_language,
                document_revision as i64,
                unit_revision as i64,
                manual_text,
                updated_at as i64,
            ],
        )
        .context("无法保存手工译文")?;
    Ok(updated == 1)
}

/// Re-stamps every persisted row of one book and language with the active
/// model and execution identity, so a changed chat model or endpoint keeps the
/// text that was already translated and only has to translate what is missing.
///
/// Returns `false` — without writing anything — when at least one row was
/// written by another protocol version or cannot be read back, because those
/// rows must not be reused; the caller then discards them and translates the
/// book again.
pub(crate) fn adopt_execution_identity(
    conn: &Connection,
    book_id: &str,
    target_language: &str,
    model: &str,
    execution_identity: &str,
) -> Result<bool> {
    let mut stmt = conn
        .prepare(
            "SELECT id, translated_text FROM translations
             WHERE book_id = ?1 AND target_language = ?2 ORDER BY ordinal, block_id",
        )
        .context("无法准备译文身份查询")?;
    let rows = stmt
        .query_map(params![book_id, target_language], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("无法读取译文身份")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("无法解析译文身份记录")?;
    drop(stmt);

    let protocol = crate::translation::execution_identity_protocol(execution_identity);
    let mut stored = Vec::with_capacity(rows.len());
    for (id, translated_text) in rows {
        let Ok(mut translation) =
            serde_json::from_str::<crate::translation::StoredTranslation>(&translated_text)
        else {
            return Ok(false);
        };
        if crate::translation::execution_identity_protocol(&translation.execution_identity)
            != protocol
        {
            return Ok(false);
        }
        translation.execution_identity = execution_identity.to_string();
        stored.push((id, serde_json::to_string(&translation)?));
    }
    for (id, translated_text) in stored {
        conn.execute(
            "UPDATE translations SET model = ?2, translated_text = ?3 WHERE id = ?1",
            params![id, model, translated_text],
        )
        .context("无法更新译文执行身份")?;
    }
    Ok(true)
}

/// Marks the still-current rows of one book as belonging to the newly published
/// document revision. Only units that kept their revision are touched: an
/// edited chapter's rows stay behind at their old revisions and are translated
/// again, while every other chapter keeps its译文 readable.
pub(crate) fn refresh_document_revision(
    conn: &Connection,
    book_id: &str,
    document_revision: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE translations SET document_revision = ?2
         WHERE book_id = ?1
           AND EXISTS (
               SELECT 1 FROM content_units
               WHERE content_units.id = translations.content_unit_id
                 AND content_units.revision = translations.unit_revision
           )",
        params![book_id, document_revision as i64],
    )
    .context("无法更新译文文档版本")
}

/// Removes the machine text of one book and language before it is translated
/// again, so blocks that no longer exist cannot linger.
///
/// A row whose manual translation can still be displayed — the reader's own text,
/// for a block of the current document and chapter revision — is kept, because a
/// background re-translation may never delete text the reader typed. Such a row
/// is still a cache entry for the run that follows, so only the blocks that lost
/// their row are translated again.
pub(crate) fn delete_for_retranslation(
    conn: &Connection,
    book_id: &str,
    target_language: &str,
    document_revision: u64,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM translations
         WHERE book_id = ?1 AND target_language = ?2
           AND (manual_text IS NULL
                OR document_revision <> ?3
                OR unit_revision IS NOT
                   (SELECT revision FROM content_units WHERE id = translations.content_unit_id))",
        params![book_id, target_language, document_revision as i64],
    )
    .context("无法删除指定语言的译文")
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
        manual_text: row.get(7)?,
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
    fn a_manual_edit_survives_the_job_and_can_be_restored() {
        let (_temp, conn) = fixture();
        upsert(&conn, &sample("block-a", 0, "机器译文")).unwrap();
        assert!(
            set_manual_text(
                &conn,
                "book",
                "unit",
                "block-a",
                "zh-Hans",
                1,
                1,
                Some("人工译文"),
                20
            )
            .unwrap()
        );
        let row = &list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0];
        assert_eq!(row.manual_text.as_deref(), Some("人工译文"));
        assert_eq!(
            row.translated_text, "机器译文",
            "the model text stays behind"
        );
        assert_eq!(updated_at(&conn), 20);

        // The background job replaces the machine text in place but must never
        // drop the reader's own edit of the same block.
        upsert(&conn, &sample("block-a", 0, "机器译文二")).unwrap();
        let row = &list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0];
        assert_eq!(row.translated_text, "机器译文二");
        assert_eq!(row.manual_text.as_deref(), Some("人工译文"));

        assert!(
            // A clock that reports an earlier instant must not violate the row's
            // `updated_at >= created_at` contract.
            set_manual_text(&conn, "book", "unit", "block-a", "zh-Hans", 1, 1, None, 1).unwrap()
        );
        assert_eq!(
            list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0].manual_text,
            None
        );
        assert_eq!(updated_at(&conn), 10);
    }

    fn updated_at(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT updated_at FROM translations WHERE block_id = 'block-a'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_manual_edit_only_matches_the_current_block() {
        let (_temp, conn) = fixture();
        upsert(&conn, &sample("block-a", 0, "机器译文")).unwrap();
        for (book, unit, block, language, document, revision) in [
            ("other-book", "unit", "block-a", "zh-Hans", 1, 1),
            ("book", "other-unit", "block-a", "zh-Hans", 1, 1),
            ("book", "unit", "block-b", "zh-Hans", 1, 1),
            ("book", "unit", "block-a", "ja", 1, 1),
            ("book", "unit", "block-a", "zh-Hans", 2, 1),
            ("book", "unit", "block-a", "zh-Hans", 1, 2),
        ] {
            assert!(
                !set_manual_text(
                    &conn,
                    book,
                    unit,
                    block,
                    language,
                    document,
                    revision,
                    Some("人工译文"),
                    20
                )
                .unwrap(),
                "a stale or foreign scope must not be written"
            );
        }
        assert_eq!(
            list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0].manual_text,
            None
        );
    }

    #[test]
    fn a_re_translation_drops_machine_rows_but_keeps_manual_edits_that_still_show() {
        let (_temp, conn) = fixture();
        upsert(&conn, &sample("block-a", 0, "机器译文")).unwrap();
        set_manual_text(
            &conn,
            "book",
            "unit",
            "block-a",
            "zh-Hans",
            1,
            1,
            Some("人工译文"),
            20,
        )
        .unwrap();
        upsert(&conn, &sample("block-b", 1, "机器译文")).unwrap();
        upsert(&conn, &sample("block-c", 2, "机器译文")).unwrap();
        set_manual_text(
            &conn,
            "book",
            "unit",
            "block-c",
            "zh-Hans",
            1,
            1,
            Some("人工译文"),
            20,
        )
        .unwrap();
        // A manual edit of a chapter version that is already gone cannot be shown
        // again, so it leaves with the machine rows.
        conn.execute(
            "UPDATE translations SET unit_revision = 2 WHERE block_id = 'block-c'",
            [],
        )
        .unwrap();

        assert_eq!(
            delete_for_retranslation(&conn, "book", "zh-Hans", 1).unwrap(),
            2,
            "the machine row and the undisplayable manual row go"
        );
        let rows = list_for_unit(&conn, "unit", "zh-Hans").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].block_id, "block-a");
        assert_eq!(rows[0].manual_text.as_deref(), Some("人工译文"));
        assert_eq!(row_count(&conn), 1);
    }

    #[test]
    fn adopting_a_new_engine_keeps_text_and_rejects_another_protocol() {
        let (_temp, conn) = fixture();
        let stored = crate::translation::StoredTranslation {
            execution_identity: "translation-v2:old".into(),
            segments: vec![crate::translation::TranslationSegment {
                source: "block-a".into(),
                translated: "甲".into(),
            }],
        };
        upsert(
            &conn,
            &sample("block-a", 0, &serde_json::to_string(&stored).unwrap()),
        )
        .unwrap();

        assert!(
            adopt_execution_identity(&conn, "book", "zh-Hans", "new-model", "translation-v2:new")
                .unwrap()
        );
        let rows = list_for_unit(&conn, "unit", "zh-Hans").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "new-model");
        let adopted: crate::translation::StoredTranslation =
            serde_json::from_str(&rows[0].translated_text).unwrap();
        assert_eq!(adopted.execution_identity, "translation-v2:new");
        assert_eq!(adopted.segments[0].translated, "甲");

        // Another protocol version cannot read the stored segments, and a
        // refused adoption must leave every row untouched.
        assert!(
            !adopt_execution_identity(&conn, "book", "zh-Hans", "new-model", "translation-v3:new")
                .unwrap()
        );
        let untouched: crate::translation::StoredTranslation = serde_json::from_str(
            &list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0].translated_text,
        )
        .unwrap();
        assert_eq!(untouched.execution_identity, "translation-v2:new");

        // A row that cannot be read back is refused as well.
        conn.execute(
            "UPDATE translations SET translated_text = 'legacy plain text'",
            [],
        )
        .unwrap();
        assert!(
            !adopt_execution_identity(&conn, "book", "zh-Hans", "new-model", "translation-v2:new")
                .unwrap()
        );
    }

    #[test]
    fn refreshing_the_document_revision_only_touches_current_units() {
        let (_temp, conn) = fixture();
        upsert(&conn, &sample("block-a", 0, "甲")).unwrap();
        assert_eq!(refresh_document_revision(&conn, "book", 7).unwrap(), 1);
        assert_eq!(
            list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0].document_revision,
            7
        );

        // A unit whose content changed keeps its stale row: the row's own unit
        // revision no longer matches, so the reader never shows it as current.
        conn.execute(
            "UPDATE content_units SET revision = 8 WHERE id = 'unit'",
            [],
        )
        .unwrap();
        assert_eq!(refresh_document_revision(&conn, "book", 9).unwrap(), 0);
        assert_eq!(
            list_for_unit(&conn, "unit", "zh-Hans").unwrap()[0].document_revision,
            7
        );
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
