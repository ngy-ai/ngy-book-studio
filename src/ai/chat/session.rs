//! A whole restored session.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatSession {
    pub thread: ChatThread,
    pub messages: Vec<StoredChatMessage>,
}
