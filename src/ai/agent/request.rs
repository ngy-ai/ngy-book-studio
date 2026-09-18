//! The requests the agent makes and the records it gets back.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Keyword,
    Semantic,
    #[default]
    Hybrid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    pub book_ids: Vec<String>,
    pub mode: SearchMode,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadPassagesRequest {
    pub passage_ids: Vec<String>,
    pub book_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineRequest {
    pub book_ids: Vec<String>,
    pub max_depth: usize,
    pub max_books: usize,
}

/// A stable search chunk or passage returned by application-owned backends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PassageRecord {
    pub passage_id: String,
    pub book_id: String,
    pub book_title: String,
    pub unit_id: String,
    pub unit_title: String,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub text: String,
    pub locator: DocumentLocator,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineNodeRecord {
    pub title: String,
    pub locator: DocumentLocator,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<OutlineNodeRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookOutlineRecord {
    pub book_id: String,
    pub book_title: String,
    pub nodes: Vec<OutlineNodeRecord>,
}
