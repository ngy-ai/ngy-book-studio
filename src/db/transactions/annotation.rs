//! Annotation insertion, mark deletion and their scope checks.

use super::*;

/// The text was checked against immutable reader bytes by the caller. Recheck
/// the owning book and unit revisions under the write lock before publishing.
pub(crate) fn insert_annotation(
    conn: &mut Connection,
    note: &crate::annotations::Annotation,
) -> Result<crate::annotations::Annotation> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动笔记保存事务")?;
    validate_current_annotation_scope(
        &tx,
        &note.book_id,
        &note.content_unit_id,
        note.document_revision,
        note.unit_revision,
    )?;
    let mut saved = note.clone();
    if note.kind.is_mark()
        && let Some(mut existing) = super::super::annotations::mark_at_anchor(
            &tx,
            &note.book_id,
            &note.content_unit_id,
            note.document_revision,
            note.unit_revision,
            &note.anchor,
        )?
    {
        if existing.kind != note.kind {
            existing.kind = note.kind;
            existing.updated_at = existing.updated_at.max(note.updated_at);
            ensure!(
                super::super::annotations::update_mark_kind(&tx, &existing)? == 1,
                "选区划线已不存在"
            );
        }
        saved = existing;
    } else {
        super::super::annotations::insert(&tx, note)?;
    }
    tx.commit().context("无法提交笔记保存事务")?;
    Ok(saved)
}

pub(crate) fn delete_annotation_marks(
    conn: &mut Connection,
    book_id: &str,
    content_unit_id: &str,
    document_revision: u64,
    unit_revision: u64,
    anchor: &crate::annotations::TextAnchor,
) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动删除划线事务")?;
    validate_current_annotation_scope(
        &tx,
        book_id,
        content_unit_id,
        document_revision,
        unit_revision,
    )?;
    super::super::annotations::delete_marks_at_anchor(
        &tx,
        book_id,
        content_unit_id,
        document_revision,
        unit_revision,
        anchor,
    )?;
    tx.commit().context("无法提交删除划线事务")
}

pub(crate) fn validate_current_annotation_scope(
    conn: &Connection,
    book_id: &str,
    content_unit_id: &str,
    document_revision: u64,
    unit_revision: u64,
) -> Result<()> {
    let book = books::get(conn, book_id)?.context("图书不存在")?;
    let unit = content_units::get(conn, content_unit_id)?.context("笔记章节不存在")?;
    let source =
        book_sources::get_revision(conn, &book.id, book.revision)?.context("当前图书来源不存在")?;
    ensure!(
        unit.book_id == book.id && unit.source_id == source.id,
        "笔记章节不属于当前图书"
    );
    ensure!(
        book.revision == document_revision && unit.revision == unit_revision,
        "图书已更新，请重新打开章节后操作笔记"
    );
    Ok(())
}
