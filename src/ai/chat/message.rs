//! Stored messages and the shape used to append one.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredChatMessage {
    pub id: String,
    pub thread_id: String,
    pub parent_id: Option<String>,
    pub ordinal: usize,
    pub role: ChatRole,
    pub content: String,
    pub model: Option<String>,
    pub created_at: u64,
    pub citations: Vec<ChatCitation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatMessage {
    pub parent_id: Option<String>,
    pub role: ChatRole,
    pub content: String,
    pub model: Option<String>,
    pub citations: Vec<NewChatCitation>,
}

impl NewChatMessage {
    pub fn text(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            parent_id: None,
            role,
            content: content.into(),
            model: None,
            citations: Vec::new(),
        }
    }
}
