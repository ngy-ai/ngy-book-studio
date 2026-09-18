//! Turning agent and chat citations into sidebar source links.

use super::*;

pub(crate) fn source_link_from_agent_citation(citation: AgentCitation) -> Option<AiSourceLink> {
    if citation.is_web() {
        return Some(AiSourceLink {
            citation_id: citation.citation_id,
            book_id: String::new(),
            unit_id: String::new(),
            unit_index: None,
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            locator: None,
            label: citation
                .source_title
                .clone()
                .unwrap_or_else(|| citation.url.clone().unwrap_or_default()),
            quote: Some(citation.quote),
            selection_snapshot: false,
            stale: false,
            url: citation.url.clone(),
        });
    }
    if citation.locator.book_id != citation.book_id
        || citation.locator.unit_id != citation.unit_id
        || citation.locator.validate().is_err()
        || matches!(
            citation.locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        )
    {
        tracing::warn!(
            target: "ngy_ai",
            stage = "convert_agent_citation",
            error_kind = "invalid_locator",
            "discarding AI citation with an invalid or mismatched locator"
        );
        return None;
    }
    let selection_snapshot = citation.citation_id.starts_with("selection:");
    Some(AiSourceLink {
        citation_id: citation.citation_id,
        book_id: citation.book_id,
        unit_id: citation.unit_id,
        unit_index: None,
        document_revision: citation.document_revision,
        unit_revision: citation.unit_revision,
        locator: Some(citation.locator),
        label: format!("{} · {}", citation.book_title, citation.unit_title),
        quote: Some(citation.quote),
        selection_snapshot,
        stale: false,
        url: None,
    })
}

pub(crate) fn source_link_from_chat_citation(citation: ChatCitation) -> Option<AiSourceLink> {
    if citation.source_kind.is_web() {
        return Some(AiSourceLink {
            citation_id: citation.id,
            book_id: String::new(),
            unit_id: String::new(),
            unit_index: None,
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            locator: None,
            label: citation
                .source_title
                .clone()
                .unwrap_or_else(|| citation.url.clone().unwrap_or_default()),
            quote: Some(citation.quote),
            selection_snapshot: false,
            stale: false,
            url: citation.url.clone(),
        });
    }
    if citation.locator.validate().is_err()
        || matches!(
            citation.locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        )
        || citation
            .content_unit_id
            .as_deref()
            .is_some_and(|unit_id| unit_id != citation.locator.unit_id)
    {
        tracing::warn!(
            target: "ngy_ai",
            stage = "convert_stored_citation",
            error_kind = "invalid_locator",
            "discarding persisted AI citation with an invalid or mismatched locator"
        );
        return None;
    }
    let selection_snapshot = citation.search_chunk_id.is_none();
    Some(AiSourceLink {
        citation_id: citation.id,
        book_id: citation.locator.book_id.clone(),
        unit_id: citation.locator.unit_id.clone(),
        unit_index: None,
        document_revision: citation.document_revision,
        unit_revision: citation.unit_revision,
        locator: Some(citation.locator),
        label: "已保存的来源".to_string(),
        quote: Some(citation.quote),
        selection_snapshot,
        stale: false,
        url: None,
    })
}
