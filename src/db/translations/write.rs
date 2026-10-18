//! Writes of the `translations` table: the machine upsert, the reader's manual
//! edit, and the scope-wide maintenance paths.

use super::*;

use super::model::NewTranslation;

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

    let protocol = crate::ai::translation::execution_identity_protocol(execution_identity);
    let mut stored = Vec::with_capacity(rows.len());
    for (id, translated_text) in rows {
        let Ok(mut translation) =
            serde_json::from_str::<crate::ai::translation::StoredTranslation>(&translated_text)
        else {
            return Ok(false);
        };
        if crate::ai::translation::execution_identity_protocol(&translation.execution_identity)
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

/// Repoints one译文 row at a re-indexed block. An in-place repair that removed
/// a leaked first block re-indexes the surviving blocks, so their stable block
/// ids — and with them the row key — change; `block_id` and `ordinal` move
/// together. The text itself is untouched, which is the whole point of the
/// repair: paid-for译文 survives without being re-requested.
pub(crate) fn repoint_block(
    conn: &Connection,
    book_id: &str,
    content_unit_id: &str,
    old_block_id: &str,
    new_block_id: &str,
    ordinal: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE translations SET block_id = ?1, ordinal = ?2
         WHERE book_id = ?3 AND content_unit_id = ?4 AND block_id = ?5",
        params![
            new_block_id,
            ordinal as i64,
            book_id,
            content_unit_id,
            old_block_id
        ],
    )
    .context("无法重映射译文块标识")
}
