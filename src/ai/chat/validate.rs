//! Validating an appended message and its citations.

use super::*;

pub(crate) fn validate_message(message: &NewChatMessage) -> Result<()> {
    ensure!(
        !message.content.trim().is_empty(),
        "chat message cannot be empty"
    );
    ensure!(
        message.content.len() <= MAX_MESSAGE_BYTES,
        "chat message exceeds {MAX_MESSAGE_BYTES} bytes"
    );
    if let Some(parent_id) = message.parent_id.as_deref() {
        validate_id("parent message ID", parent_id)?;
    }
    if let Some(model) = message.model.as_deref() {
        ensure!(
            !model.trim().is_empty() && !model.chars().any(char::is_control),
            "chat model is invalid"
        );
    }
    ensure!(
        message.citations.len() <= MAX_CITATIONS,
        "chat message exceeds {MAX_CITATIONS} citations"
    );
    Ok(())
}

pub(crate) fn validate_citation(citation: &NewChatCitation, scope: &ChatScope) -> Result<()> {
    if citation.source_kind.is_web() {
        ensure!(
            citation.content_unit_id.is_none() && citation.search_chunk_id.is_none(),
            "web citation must not reference a content unit or search chunk"
        );
        ensure!(
            citation
                .url
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty()),
            "web citation must carry a source URL"
        );
        ensure!(
            !citation.quote.trim().is_empty(),
            "citation quote cannot be empty"
        );
        ensure!(
            citation.quote.len() <= MAX_QUOTE_BYTES,
            "citation quote exceeds {MAX_QUOTE_BYTES} bytes"
        );
        return Ok(());
    }
    citation
        .locator
        .validate()
        .context("citation locator is invalid")?;
    ensure!(
        scope.contains(&citation.locator.book_id),
        "citation is outside the persisted chat scope"
    );
    ensure!(
        !matches!(
            citation.locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        ),
        "preview-only Office rendered pages cannot be persisted as citations"
    );
    ensure!(
        !citation.quote.trim().is_empty(),
        "citation quote cannot be empty"
    );
    ensure!(
        citation.quote.len() <= MAX_QUOTE_BYTES,
        "citation quote exceeds {MAX_QUOTE_BYTES} bytes"
    );
    if let Some(content_unit_id) = citation.content_unit_id.as_deref() {
        validate_id("citation content unit ID", content_unit_id)?;
        ensure!(
            content_unit_id == citation.locator.unit_id,
            "citation content unit does not match its locator"
        );
    }
    if let Some(search_chunk_id) = citation.search_chunk_id.as_deref() {
        validate_id("citation search chunk ID", search_chunk_id)?;
    }
    Ok(())
}

pub(crate) fn validate_current_citation(
    conn: &rusqlite::Connection,
    citation: &NewChatCitation,
    scope: &ChatScope,
) -> Result<()> {
    // Web citations have no book identity and skip the current-content checks.
    if citation.source_kind.is_web() {
        return validate_citation(citation, scope);
    }
    validate_citation(citation, scope)?;
    let book = db::books::get(conn, &citation.locator.book_id)?
        .context("citation book no longer exists")?;
    ensure!(
        book.revision == citation.document_revision.get(),
        "citation document revision is stale"
    );
    let source = db::book_sources::get_revision(conn, &book.id, book.revision)?
        .context("citation document source no longer exists")?;
    let unit = db::content_units::get(conn, &citation.locator.unit_id)?
        .context("citation content unit no longer exists")?;
    ensure!(
        unit.book_id == book.id && unit.source_id == source.id,
        "citation content unit is outside the current document revision"
    );
    ensure!(
        unit.revision == citation.unit_revision.get(),
        "citation content unit revision is stale"
    );
    if let Some(chunk_id) = citation.search_chunk_id.as_deref() {
        let chunk = db::search_chunks::get(conn, chunk_id)?
            .context("citation search passage no longer exists")?;
        ensure!(
            chunk.book_id == book.id
                && chunk.source_id == source.id
                && chunk.content_unit_id == unit.id,
            "citation search passage does not match its current book and content unit"
        );
        let chunk_locator = serde_json::from_str::<DocumentLocator>(&chunk.locator_json)
            .context("citation search passage locator is invalid")?;
        ensure!(
            chunk_locator == citation.locator,
            "citation locator does not match its search passage"
        );
    }
    Ok(())
}
