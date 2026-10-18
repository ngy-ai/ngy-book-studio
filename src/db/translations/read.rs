//! Reads of the `translations` table.

use super::*;

use super::model::SourceTextTranslation;

const SELECT: &str = "SELECT block_id, ordinal, document_revision, unit_revision,
    model, source_text, translated_text, manual_text FROM translations";

/// How many source texts one statement asks for. Labels are short and few, and
/// keeping the list well below SQLite's default parameter limit means the query
/// stays one prepared statement per chunk instead of a temporary table.
const SOURCE_TEXT_CHUNK: usize = 128;

/// Every译文 of the requested texts that is still current: the row belongs to
/// the requested document revision and to a unit that still carries the revision
/// the row was written for. A label that names no block of a current chapter
/// simply has no row, which is the caller's "keep the book's own label".
pub(crate) fn list_with_source_texts(
    conn: &Connection,
    book_id: &str,
    target_language: &str,
    document_revision: u64,
    texts: &[String],
) -> Result<Vec<SourceTextTranslation>> {
    let mut found = Vec::new();
    for chunk in texts.chunks(SOURCE_TEXT_CHUNK) {
        let placeholders = (1..=chunk.len())
            .map(|parameter| format!("?{}", parameter + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT t.content_unit_id, t.block_id, t.ordinal, t.document_revision,
                    t.unit_revision, t.model, t.source_text, t.translated_text, t.manual_text
             FROM translations t
             JOIN content_units u
               ON u.id = t.content_unit_id AND u.revision = t.unit_revision
             WHERE t.book_id = ?1 AND t.target_language = ?2 AND t.document_revision = ?3
               AND t.source_text IN ({placeholders})
             ORDER BY t.ordinal, t.block_id"
        );
        let values = std::iter::once(Value::Text(book_id.to_string()))
            .chain(std::iter::once(Value::Text(target_language.to_string())))
            .chain(std::iter::once(Value::Integer(document_revision as i64)))
            .chain(chunk.iter().cloned().map(Value::Text))
            .collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql).context("无法准备目录译文查询")?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(SourceTextTranslation {
                    content_unit_id: row.get(0)?,
                    translation: Translation {
                        block_id: row.get(1)?,
                        ordinal: row.get::<_, i64>(2)? as u64,
                        document_revision: row.get::<_, i64>(3)? as u64,
                        unit_revision: row.get::<_, i64>(4)? as u64,
                        model: row.get(5)?,
                        source_text: row.get(6)?,
                        translated_text: row.get(7)?,
                        manual_text: row.get(8)?,
                    },
                })
            })
            .context("无法读取目录译文")?;
        found.extend(
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("无法解析目录译文记录")?,
        );
    }
    Ok(found)
}

/// Every译文 row of one book, newest scope first. The share export walks these
/// to write one portable section entry per row.
pub(crate) fn list_for_book(conn: &Connection, book_id: &str) -> Result<Vec<TranslationRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT content_unit_id, block_id, ordinal, document_revision, unit_revision,
                    target_language, source_language, model, source_text, translated_text,
                    manual_text
             FROM translations WHERE book_id = ?1
             ORDER BY content_unit_id, target_language, ordinal, block_id",
        )
        .context("无法准备整本图书译文查询")?;
    stmt.query_map(params![book_id], |row| {
        Ok(TranslationRow {
            content_unit_id: row.get(0)?,
            block_id: row.get(1)?,
            ordinal: row.get::<_, i64>(2)? as u64,
            document_revision: row.get::<_, i64>(3)? as u64,
            unit_revision: row.get::<_, i64>(4)? as u64,
            target_language: row.get(5)?,
            source_language: row.get(6)?,
            model: row.get(7)?,
            source_text: row.get(8)?,
            translated_text: row.get(9)?,
            manual_text: row.get(10)?,
        })
    })
    .context("无法读取整本图书译文")?
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("无法解析整本图书译文记录")
}

/// Every译文 row of one book and language at the given document revision.
/// Used by a per-block retry to tell which of the requested ordinals still have
/// no usable translation: those are the only ones worth re-sending, while an
/// already translated (or hand-edited) block must keep its row.
pub(crate) fn list_for_source(
    conn: &Connection,
    book_id: &str,
    target_language: &str,
    document_revision: u64,
) -> Result<Vec<Translation>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE book_id = ?1 AND target_language = ?2 AND document_revision = ?3
             ORDER BY ordinal, block_id"
        ))
        .context("无法准备整本译文查询")?;
    stmt.query_map(
        params![book_id, target_language, document_revision as i64],
        super::model::from_row,
    )
    .context("无法读取整本译文")?
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("无法解析整本译文记录")
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
    stmt.query_map(
        params![content_unit_id, target_language],
        super::model::from_row,
    )
    .context("无法读取章节译文")?
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("无法解析章节译文记录")
}
