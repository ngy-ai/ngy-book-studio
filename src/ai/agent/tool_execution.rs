//! What one tool call produced.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolExecution {
    pub tool_call_id: String,
    pub name: String,
    /// Compact JSON whose byte count has already been charged to the session.
    pub content: String,
    pub citations: Vec<AgentCitation>,
}

impl ToolExecution {
    pub fn as_chat_message(&self) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Tool,
            content: Some(self.content.clone().into()),
            name: Some(self.name.clone()),
            tool_call_id: Some(self.tool_call_id.clone()),
            tool_calls: Vec::new(),
        }
    }
}
