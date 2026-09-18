//! Visual search chunks: replacement, validation and finishing.

use super::*;

pub(crate) fn validate_index_model(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.trim() == value
            && value.chars().count() <= 256
            && !value.chars().any(char::is_control),
        "{label} 模型标识无效"
    );
    Ok(())
}

pub(crate) fn valid_vision_cursor(
    job: &IndexJob,
    source: &BookSource,
) -> Option<PersistedIndexCursor> {
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).ok()?;
    (job.kind == "vision"
        && job.book_id == source.book_id
        && job.source_id.as_deref() == Some(source.id.as_str())
        && cursor.schema_version == 1
        && cursor.book_id == source.book_id
        && cursor.source_id == source.id
        && cursor.revision == source.revision
        && cursor.kind == "vision")
        .then_some(cursor)
}

/// Atomically replaces every visual chunk from one page onward. Vision output
/// can contain several regions for a page, so publishing them one row at a
/// time would expose a partial page and leave surplus regions from an earlier
/// run. Later-page chunks are intentionally removed as well: the durable page
/// cursor will rebuild them with the same model contract before completion.
pub(crate) fn replace_visual_search_chunks_for_running_vision_job(
    conn: &mut Connection,
    job_id: &str,
    expected_cursor_json: &str,
    book_id: &str,
    source_id: &str,
    first_ordinal: usize,
    chunks: &[SearchChunk],
) -> Result<bool> {
    validate_visual_search_chunks(book_id, source_id, first_ordinal, chunks)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动受保护的视觉页面分块替换事务")?;
    if !running_canonical_vision_job_matches(&tx, job_id, expected_cursor_json, source_id)? {
        return Ok(false);
    }
    validate_current_visual_source(&tx, book_id, source_id)?;
    for chunk in chunks {
        ensure!(
            content_units::get(&tx, &chunk.content_unit_id)?.is_some_and(|unit| {
                unit.book_id == chunk.book_id && unit.source_id == chunk.source_id
            }),
            "视觉分块内容单元与来源不匹配"
        );
    }
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_ordinal)?;
    for chunk in chunks {
        search_chunks::upsert_derived(&tx, chunk)?;
    }
    tx.commit()
        .context("无法提交受保护的视觉页面分块替换事务")?;
    Ok(true)
}

pub(crate) fn replace_visual_search_chunks_from_ordinal(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    first_ordinal: usize,
    chunks: &[SearchChunk],
) -> Result<()> {
    validate_visual_search_chunks(book_id, source_id, first_ordinal, chunks)?;

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面分块替换事务")?;
    validate_current_visual_source(&tx, book_id, source_id)?;
    for chunk in chunks {
        ensure!(
            content_units::get(&tx, &chunk.content_unit_id)?.is_some_and(|unit| {
                unit.book_id == chunk.book_id && unit.source_id == chunk.source_id
            }),
            "视觉分块内容单元与来源不匹配"
        );
    }
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_ordinal)?;
    for chunk in chunks {
        search_chunks::upsert_derived(&tx, chunk)?;
    }
    tx.commit().context("无法提交视觉页面分块替换事务")?;
    Ok(())
}

pub(crate) fn validate_visual_search_chunks(
    book_id: &str,
    source_id: &str,
    first_ordinal: usize,
    chunks: &[SearchChunk],
) -> Result<()> {
    ensure!(!chunks.is_empty(), "视觉页面至少需要一个可索引分块");
    ensure!(
        chunks.iter().all(|chunk| {
            chunk.id.starts_with("vision-chunk-")
                && chunk.book_id == book_id
                && chunk.source_id == source_id
                && chunk.ordinal >= first_ordinal
        }),
        "视觉页面分块身份或序号不匹配"
    );
    ensure!(
        chunks.iter().any(|chunk| chunk.ordinal == first_ordinal),
        "视觉页面分块必须从页面首序号开始"
    );
    ensure!(
        chunks
            .iter()
            .map(|chunk| chunk.id.as_str())
            .collect::<HashSet<_>>()
            .len()
            == chunks.len()
            && chunks
                .iter()
                .map(|chunk| chunk.ordinal)
                .collect::<HashSet<_>>()
                .len()
                == chunks.len(),
        "视觉页面包含重复分块 ID 或序号"
    );
    Ok(())
}

pub(crate) fn validate_current_visual_source(
    conn: &Connection,
    book_id: &str,
    source_id: &str,
) -> Result<()> {
    let source = book_sources::get(conn, source_id)?.context("视觉分块来源不存在")?;
    let book = books::get(conn, book_id)?.context("视觉分块图书不存在")?;
    ensure!(
        source.book_id == book.id
            && book.revision == source.revision
            && book_sources::get_revision(conn, &book.id, book.revision)?
                .is_some_and(|current| current.id == source.id),
        "视觉分块不能写入过期来源"
    );
    Ok(())
}

pub(crate) fn running_canonical_vision_job_matches(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    source_id: &str,
) -> Result<bool> {
    Ok(index_jobs::get(conn, job_id)?.is_some_and(|job| {
        job.id == format!("vision:{source_id}")
            && job.source_id.as_deref() == Some(source_id)
            && job.kind == "vision"
            && job.status == IndexJobStatus::Running
            && !job.pause_requested
            && !job.cancel_requested
            && job.cursor_json == expected_cursor_json
    }))
}

/// Removes descriptions for pages no longer present in the current atomic
/// visual-page set. Their embeddings cascade with the deleted chunks.
pub(crate) fn finish_visual_search_chunks(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    first_stale_ordinal: usize,
) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉搜索收尾事务")?;
    let book = books::get(&tx, book_id)?.context("视觉搜索图书不存在")?;
    let source = book_sources::get(&tx, source_id)?.context("视觉搜索来源不存在")?;
    ensure!(
        source.book_id == book.id && source.revision == book.revision,
        "视觉搜索任务来源已经过期"
    );
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_stale_ordinal)?;
    tx.commit().context("无法提交视觉搜索收尾事务")?;
    Ok(())
}

pub(crate) fn finish_visual_search_chunks_for_running_vision_job(
    conn: &mut Connection,
    job_id: &str,
    expected_cursor_json: &str,
    book_id: &str,
    source_id: &str,
    first_stale_ordinal: usize,
) -> Result<bool> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动受保护的视觉搜索收尾事务")?;
    if !running_canonical_vision_job_matches(&tx, job_id, expected_cursor_json, source_id)? {
        return Ok(false);
    }
    validate_current_visual_source(&tx, book_id, source_id)?;
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_stale_ordinal)?;
    tx.commit().context("无法提交受保护的视觉搜索收尾事务")?;
    Ok(true)
}

pub(crate) fn enqueue_index_job_for_running_vision_job(
    conn: &mut Connection,
    vision_job_id: &str,
    expected_vision_cursor_json: &str,
    source_id: &str,
    job: &IndexJob,
) -> Result<bool> {
    ensure!(
        job.kind == "embedding" && job.source_id.as_deref() == Some(source_id),
        "视觉后续索引任务身份无效"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉后续索引投递事务")?;
    if !running_canonical_vision_job_matches(
        &tx,
        vision_job_id,
        expected_vision_cursor_json,
        source_id,
    )? {
        return Ok(false);
    }
    if index_jobs::insert_if_absent(&tx, job)? == 0 {
        index_jobs::reset_terminal(&tx, &job.id, &job.cursor_json, job.updated_at)?;
    }
    tx.commit().context("无法提交视觉后续索引投递事务")?;
    Ok(true)
}
