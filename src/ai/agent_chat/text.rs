//! Deriving the session title and the bounded history window.

use super::*;

pub(crate) fn question_title(question: &str) -> String {
    let title = question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect::<String>();
    if title.is_empty() {
        "新对话".to_string()
    } else {
        title
    }
}

pub(crate) fn history_messages(messages: &[StoredChatMessage]) -> Vec<ChatMessage> {
    let mut selected = Vec::new();
    let mut bytes = 0usize;
    for message in messages.iter().rev() {
        if selected.len() >= HISTORY_MESSAGES
            || bytes.saturating_add(message.content.len()) > HISTORY_BYTES
        {
            break;
        }
        if matches!(message.role, ChatRole::User | ChatRole::Assistant) {
            bytes += message.content.len();
            selected.push(ChatMessage::text(message.role, message.content.clone()));
        }
    }
    selected.reverse();
    selected
}
