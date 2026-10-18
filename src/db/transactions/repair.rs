//! The in-place repair of a leaked `<head>/<title>` first block.
//!
//! Old parsers rendered every chapter's `<head><title>` as its first body
//! block, so each chapter showed its title twice. The repair removes that block
//! from the stored units without republishing the document: notes and paid-for
//! 译文 stay anchored to the same revision.
//!
//! One transaction performs all four writes — cleaned unit rows, repointed
//! translation rows, shifted/dropped notes, and the replaced canonical search
//! chunks — so a failure anywhere leaves the previous state intact. FTS rows
//! follow through triggers and embeddings cascade with the deleted chunks;
//! visual derived chunks (`vision-chunk-…`) belong to rendered pages and are
//! deliberately preserved.

use super::*;

/// One unit whose leaked first block was removed.
pub(crate) struct RepairedUnit {
    /// The cleaned unit row. [`content_units::update_content`] writes it back
    /// verbatim; the caller keeps `revision` unchanged so notes and whole-book
    /// 译文 remain anchored to the same document revision.
    pub(crate) unit: ContentUnit,
    /// Old block id -> (new block id, new ordinal) for every block that
    /// survived the removal.
    pub(crate) block_remap: Vec<(String, String, u64)>,
    /// UTF-16 length of the removed first block; notes shift back by this much.
    pub(crate) removed_utf16_len: u64,
}

/// Applies the repair of one book and returns how many notes were dropped
/// because their anchor started inside the removed text.
///
/// `search_chunks` are the canonical chunks the caller rebuilt from the cleaned
/// document; they replace the source's canonical chunks inside the same
/// transaction. `source_id` must be the book's currently published source.
pub(crate) fn repair_leaked_head_titles(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    repaired: &[RepairedUnit],
    search_chunks: &[SearchChunk],
    now: u64,
) -> Result<u64> {
    ensure!(!repaired.is_empty(), "没有需要修复的内容单元");
    ensure!(
        search_chunks
            .iter()
            .all(|chunk| chunk.book_id == book_id && chunk.source_id == source_id),
        "搜索分块不属于该书当前来源"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动泄漏标题修复事务")?;
    let mut annotations_deleted = 0u64;
    for entry in repaired {
        // Ownership first: the row must belong to this book and to the source
        // whose chunks are about to be rebuilt, otherwise a stale unit id would
        // silently rewrite another book's text.
        let existing =
            super::super::content_units::get(&tx, &entry.unit.id)?.context("内容单元不存在")?;
        ensure!(
            existing.book_id == book_id && existing.source_id == source_id,
            "内容单元 {} 不属于该书当前来源",
            entry.unit.id
        );
        let mut unit = entry.unit.clone();
        unit.updated_at = now;
        super::super::content_units::update_content(&tx, &unit)?;
        for (old_block_id, new_block_id, ordinal) in &entry.block_remap {
            super::super::translations::repoint_block(
                &tx,
                book_id,
                &unit.id,
                old_block_id,
                new_block_id,
                *ordinal,
            )?;
        }
        if entry.removed_utf16_len > 0 {
            let removed =
                i64::try_from(entry.removed_utf16_len).context("被删文本长度超出支持范围")?;
            super::super::annotations::shift_offsets_after_removal(
                &tx, book_id, &unit.id, removed,
            )?;
            let dropped = super::super::annotations::delete_inside_removed_range(
                &tx, book_id, &unit.id, removed,
            )? as u64;
            annotations_deleted = annotations_deleted.saturating_add(dropped);
        }
    }
    search_chunks::delete_canonical_for_source(&tx, source_id)?;
    for chunk in search_chunks {
        search_chunks::insert(&tx, chunk)?;
    }
    tx.commit().context("无法提交泄漏标题修复事务")?;
    Ok(annotations_deleted)
}
