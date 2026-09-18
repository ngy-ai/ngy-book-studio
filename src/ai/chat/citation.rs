//! Stored citations and the shape used to record one.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatCitation {
    pub id: String,
    pub content_unit_id: Option<String>,
    pub search_chunk_id: Option<String>,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub quote: String,
    pub locator: DocumentLocator,
    pub source_kind: AgentCitationSourceKind,
    pub url: Option<String>,
    pub source_title: Option<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatCitation {
    pub content_unit_id: Option<String>,
    pub search_chunk_id: Option<String>,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub quote: String,
    pub locator: DocumentLocator,
    pub source_kind: AgentCitationSourceKind,
    pub url: Option<String>,
    pub source_title: Option<String>,
}
