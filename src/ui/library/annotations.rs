//! Locating the unit an annotation belongs to.

use super::*;

pub(crate) type AnnotationOpenError = Rc<dyn Fn(String, &mut App)>;

pub(crate) fn current_annotation_unit_index(
    note: &Annotation,
    document: &BookDocument,
) -> Result<usize> {
    ensure!(note.book_id == document.id, "笔记不属于当前图书");
    ensure!(
        !note.stale && note.document_revision == document.revision.0,
        "笔记对应的正文已更新，请刷新笔记列表"
    );
    let index = document
        .units
        .iter()
        .position(|unit| unit.id == note.content_unit_id)
        .context("笔记所在章节已删除")?;
    ensure!(
        document.units[index].revision.0 == note.unit_revision,
        "笔记所在章节已更新"
    );
    Ok(index)
}
