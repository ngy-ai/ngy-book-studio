//! Chat message insertion and citation validation.

use super::*;

/// Adds a chat message and all of its citations without exposing a message that
/// has only a partial citation list.
pub(crate) fn insert_chat_message(
    conn: &mut Connection,
    message: &ChatMessage,
    citations: &[ChatCitation],
    thread_updated_at: u64,
) -> Result<()> {
    ensure!(
        citations
            .iter()
            .all(|citation| citation.message_id == message.id),
        "对话引用不属于待写入消息"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动对话消息事务")?;
    let thread = chat_threads::get(&tx, &message.thread_id)?.context("对话不存在")?;
    let scope = serde_json::from_str::<PersistedChatScope>(&thread.scope_json)
        .context("对话授权范围无效")?;
    for citation in citations {
        validate_current_chat_citation(&tx, citation, &scope.book_ids)?;
    }
    chat_messages::insert(&tx, message)?;
    for citation in citations {
        chat_citations::insert(&tx, citation)?;
    }
    if super::super::chat_threads::touch(&tx, &message.thread_id, thread_updated_at)? == 0 {
        bail!("对话不存在");
    }
    tx.commit().context("无法提交对话消息事务")?;
    Ok(())
}

#[derive(serde::Deserialize)]
pub(crate) struct PersistedChatScope {
    pub(crate) book_ids: Vec<String>,
}

pub(crate) fn validate_current_chat_citation(
    conn: &Connection,
    citation: &ChatCitation,
    allowed_book_ids: &[String],
) -> Result<()> {
    // Web-sourced citations carry no book locator and must not be validated
    // against the current content tree.
    if citation.locator_json.is_none() {
        return Ok(());
    }
    let locator =
        serde_json::from_str::<DocumentLocator>(citation.locator_json.as_deref().unwrap())
            .context("对话引用定位信息无效")?;
    locator.validate().context("对话引用定位信息无效")?;
    ensure!(
        !matches!(
            locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        ),
        "Office 增强预览页不能持久化为对话引用"
    );
    ensure!(
        allowed_book_ids
            .iter()
            .any(|book_id| book_id == &locator.book_id),
        "对话引用超出当前授权范围"
    );
    let book = books::get(conn, &locator.book_id)?.context("对话引用图书已不存在")?;
    ensure!(
        book.revision == citation.document_revision,
        "对话引用的文档版本已失效"
    );
    let source = super::super::book_sources::get_revision(conn, &book.id, book.revision)?
        .context("对话引用的文档来源已不存在")?;
    let unit = content_units::get(conn, &locator.unit_id)?.context("对话引用的内容单元已不存在")?;
    ensure!(
        unit.book_id == book.id && unit.source_id == source.id,
        "对话引用的内容单元不属于当前文档版本"
    );
    ensure!(
        unit.revision == citation.unit_revision,
        "对话引用的内容单元版本已失效"
    );
    if let Some(content_unit_id) = citation.content_unit_id.as_deref() {
        ensure!(
            content_unit_id == locator.unit_id,
            "对话引用的内容单元与定位信息不一致"
        );
    }
    let blocks = serde_json::from_str::<BlockDocument>(&unit.block_json)
        .context("对话引用的内容单元结构无效")?;
    if let Some(block_id) = locator.block_id.as_deref() {
        let block = blocks
            .find_block(block_id)
            .context("对话引用的内容块已不存在")?;
        if let Some(range) = locator.text_range {
            let text = block.plain_text();
            let start = usize::try_from(range.start_byte).context("对话引用起始位置超出范围")?;
            let end = usize::try_from(range.end_byte).context("对话引用结束位置超出范围")?;
            ensure!(
                start <= end
                    && end <= text.len()
                    && text.is_char_boundary(start)
                    && text.is_char_boundary(end),
                "对话引用文本范围已失效"
            );
        }
    }
    if let Some(chunk_id) = citation.search_chunk_id.as_deref() {
        let chunk = search_chunks::get(conn, chunk_id)?.context("对话引用的搜索片段已不存在")?;
        let chunk_locator = serde_json::from_str::<DocumentLocator>(&chunk.locator_json)
            .context("对话引用的搜索片段定位信息无效")?;
        ensure!(
            chunk.book_id == book.id
                && chunk.source_id == source.id
                && chunk.content_unit_id == unit.id
                && chunk_locator == locator,
            "对话引用与当前搜索片段不一致"
        );
    }
    Ok(())
}
