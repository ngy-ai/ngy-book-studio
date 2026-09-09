use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::annotations::{Annotation, AnnotationKind, AnnotationOverview, TextAnchor};

const SELECT: &str = "SELECT id, book_id, content_unit_id, document_revision,
    unit_revision, quote, start_offset, end_offset, kind, comment, created_at, updated_at
    FROM annotations";

pub(crate) fn list(
    conn: &Connection,
    book_id: &str,
    unit_id: Option<&str>,
) -> Result<Vec<Annotation>> {
    let mut stmt = conn.prepare(&format!("{SELECT} WHERE book_id = ?1 AND (?2 IS NULL OR content_unit_id = ?2) ORDER BY created_at, id"))
        .context("无法准备笔记查询")?;
    stmt.query_map(params![book_id, unit_id], from_row)
        .context("无法读取笔记")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("无法解析笔记记录")
}

pub(crate) fn overview(
    conn: &Connection,
    book_id: Option<&str>,
) -> Result<Vec<AnnotationOverview>> {
    let mut stmt = conn
        .prepare(
            "SELECT a.id, a.book_id, a.content_unit_id, a.document_revision,
                    a.unit_revision, a.quote, a.start_offset, a.end_offset,
                    a.kind, a.comment, a.created_at, a.updated_at,
                    b.title, b.revision, u.title, u.ordinal, u.revision
             FROM annotations a
             JOIN books b ON b.id = a.book_id
             LEFT JOIN book_sources s ON s.book_id = b.id AND s.revision = b.revision
             LEFT JOIN content_units u ON u.id = a.content_unit_id
                                       AND u.book_id = b.id AND u.source_id = s.id
             WHERE (?1 IS NULL OR a.book_id = ?1)
             ORDER BY a.updated_at DESC, a.id",
        )
        .context("无法准备笔记总览查询")?;
    stmt.query_map([book_id], |row| {
        let mut annotation = from_row(row)?;
        let book_revision = row.get::<_, u64>(13)?;
        let unit_revision = row.get::<_, Option<u64>>(16)?;
        annotation.stale = annotation.document_revision != book_revision
            || unit_revision != Some(annotation.unit_revision);
        Ok(AnnotationOverview {
            annotation,
            book_title: row.get(12)?,
            chapter_title: row.get(14)?,
            chapter_index: row.get(15)?,
        })
    })
    .context("无法读取笔记总览")?
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("无法解析笔记总览记录")
}

pub(crate) fn insert(conn: &Connection, note: &Annotation) -> Result<()> {
    conn.execute("INSERT INTO annotations (id, book_id, content_unit_id, document_revision, unit_revision, quote, start_offset, end_offset, kind, comment, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![note.id, note.book_id, note.content_unit_id, note.document_revision as i64, note.unit_revision as i64, note.anchor.quote,
            note.anchor.start, note.anchor.end, note.kind.as_str(), note.comment, note.created_at as i64, note.updated_at as i64])
        .context("无法保存笔记")?;
    Ok(())
}

pub(crate) fn mark_at_anchor(
    conn: &Connection,
    book_id: &str,
    content_unit_id: &str,
    document_revision: u64,
    unit_revision: u64,
    anchor: &TextAnchor,
) -> Result<Option<Annotation>> {
    conn.query_row(
        &format!(
            "{SELECT} WHERE book_id = ?1 AND content_unit_id = ?2
            AND document_revision = ?3 AND unit_revision = ?4
            AND start_offset = ?5 AND end_offset = ?6
            AND kind IN ('highlight', 'wavy', 'underline')"
        ),
        params![
            book_id,
            content_unit_id,
            document_revision as i64,
            unit_revision as i64,
            anchor.start,
            anchor.end
        ],
        from_row,
    )
    .optional()
    .context("无法读取选区划线笔记")
}

pub(crate) fn update_mark_kind(conn: &Connection, note: &Annotation) -> Result<usize> {
    conn.execute(
        "UPDATE annotations SET kind = ?3, updated_at = ?4 WHERE book_id = ?1 AND id = ?2
            AND kind IN ('highlight', 'wavy', 'underline')",
        params![
            note.book_id,
            note.id,
            note.kind.as_str(),
            note.updated_at as i64
        ],
    )
    .context("无法更换选区划线样式")
}

pub(crate) fn delete_marks_at_anchor(
    conn: &Connection,
    book_id: &str,
    content_unit_id: &str,
    document_revision: u64,
    unit_revision: u64,
    anchor: &TextAnchor,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM annotations WHERE book_id = ?1 AND content_unit_id = ?2
            AND document_revision = ?3 AND unit_revision = ?4
            AND start_offset = ?5 AND end_offset = ?6
            AND kind IN ('highlight', 'wavy', 'underline')",
        params![
            book_id,
            content_unit_id,
            document_revision as i64,
            unit_revision as i64,
            anchor.start,
            anchor.end
        ],
    )
    .context("无法删除选区划线")
}

pub(crate) fn update_human_comment(
    conn: &Connection,
    book_id: &str,
    id: &str,
    comment: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute("UPDATE annotations SET comment = ?3, updated_at = ?4 WHERE book_id = ?1 AND id = ?2 AND kind = 'human_comment'",
        params![book_id, id, comment, updated_at as i64]).context("无法更新人工想法")
}

pub(crate) fn delete(conn: &Connection, book_id: &str, id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM annotations WHERE book_id = ?1 AND id = ?2",
        params![book_id, id],
    )
    .context("无法删除笔记")
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Annotation> {
    let kind = match row.get::<_, String>(8)?.as_str() {
        "highlight" => AnnotationKind::Highlight,
        "wavy" => AnnotationKind::Wavy,
        "underline" => AnnotationKind::Underline,
        "human_comment" => AnnotationKind::HumanComment,
        "ai_comment" => AnnotationKind::AiComment,
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                8,
                "kind".into(),
                rusqlite::types::Type::Text,
            ));
        }
    };
    Ok(Annotation {
        id: row.get(0)?,
        book_id: row.get(1)?,
        content_unit_id: row.get(2)?,
        document_revision: row.get::<_, i64>(3)? as u64,
        unit_revision: row.get::<_, i64>(4)? as u64,
        anchor: TextAnchor {
            quote: row.get(5)?,
            start: row.get(6)?,
            end: row.get(7)?,
        },
        kind,
        comment: row.get(9)?,
        created_at: row.get::<_, i64>(10)? as u64,
        updated_at: row.get::<_, i64>(11)? as u64,
        stale: false,
    })
}
