//! Row shapes of the `translations` table and the mapping between them.

use super::*;

/// One persisted, revision-scoped translation of a single block. Rows are keyed
/// by `(book_id, content_unit_id, block_id, target_language)` so the reader can
/// look up a whole chapter in one query, and the background job can replace a
/// block in place when the document revision or model changes.
///
/// `manual_text` carries the reader's own edit of the same block in the same
/// [`StoredTranslation`](crate::ai::translation::StoredTranslation) shape. It lives
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

/// One译文 row including the scope columns the share package needs. The reader
/// only ever asks for one unit of one language, so the narrower [`Translation`]
/// stays the shape of that path; exporting a book needs the whole book, every
/// language, and the unit each row belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranslationRow {
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
    pub manual_text: Option<String>,
}

/// One译文 row found by the text it translates, together with the unit it
/// belongs to.
///
/// The reading window asks for these to translate the labels of its table of
/// contents: a label names a block of the chapter it opens, and the unit keeps
/// the answer tied to that chapter when the same text also appears elsewhere in
/// the book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceTextTranslation {
    pub(crate) content_unit_id: String,
    pub(crate) translation: Translation,
}

pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Translation> {
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
