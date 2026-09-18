//! Translating rows to and from the stored shapes.

use super::*;

pub(crate) fn thread_to_db(thread: &ChatThread) -> Result<db::chat_threads::ChatThread> {
    Ok(db::chat_threads::ChatThread {
        id: thread.id.clone(),
        book_id: thread.primary_book_id.clone(),
        title: thread.title.clone(),
        scope_json: serde_json::to_string(&thread.scope)
            .context("failed to serialize chat scope")?,
        window_kind: thread.window_kind.as_str().to_string(),
        created_at: thread.created_at,
        updated_at: thread.updated_at,
    })
}

pub(crate) fn thread_from_db(row: db::chat_threads::ChatThread) -> Result<ChatThread> {
    let scope = serde_json::from_str::<ChatScope>(&row.scope_json)
        .context("stored chat scope is invalid")?
        .normalized()?;
    let window_kind = ChatWindowKind::parse(&row.window_kind)?;
    let thread = ChatThread {
        id: row.id,
        primary_book_id: row.book_id,
        title: row.title,
        scope,
        window_kind,
        created_at: row.created_at,
        updated_at: row.updated_at,
    };
    NewChatThread {
        primary_book_id: thread.primary_book_id.clone(),
        title: thread.title.clone(),
        scope: thread.scope.clone(),
        window_kind: thread.window_kind,
    }
    .validate()?;
    Ok(thread)
}

pub(crate) fn citation_from_db(row: db::chat_citations::ChatCitation) -> Result<ChatCitation> {
    let source_kind = parse_citation_source_kind(&row.source_kind);
    // Web citations carry no book locator; only book citations are validated
    // against the current content tree.
    let locator = if source_kind.is_web() {
        DocumentLocator::unit("", "")
    } else {
        let json = match row.locator_json.as_deref() {
            Some(json) if !json.trim().is_empty() => json,
            _ => "{}",
        };
        let locator = serde_json::from_str::<DocumentLocator>(json)
            .context("stored citation locator is invalid")?;
        locator
            .validate()
            .context("stored citation locator is invalid")?;
        locator
    };
    Ok(ChatCitation {
        id: row.id,
        content_unit_id: row.content_unit_id,
        search_chunk_id: row.search_chunk_id,
        document_revision: Revision::new(row.document_revision),
        unit_revision: Revision::new(row.unit_revision),
        quote: row.quote,
        locator,
        source_kind,
        url: row.url,
        source_title: row.source_title,
        created_at: row.created_at,
    })
}

pub(crate) fn parse_citation_source_kind(value: &str) -> AgentCitationSourceKind {
    match value {
        "web" => AgentCitationSourceKind::Web,
        _ => AgentCitationSourceKind::Book,
    }
}

pub(crate) fn message_from_db(
    row: db::chat_messages::ChatMessage,
    citations: Vec<ChatCitation>,
) -> Result<StoredChatMessage> {
    Ok(StoredChatMessage {
        id: row.id,
        thread_id: row.thread_id,
        parent_id: row.parent_id,
        ordinal: row.ordinal,
        role: parse_role(&row.role)?,
        content: row.content,
        model: row.model,
        created_at: row.created_at,
        citations,
    })
}
