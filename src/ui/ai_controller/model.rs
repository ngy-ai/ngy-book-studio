//! The size budget and the messages the controller hands to the UI.

use super::*;

pub(crate) const MAX_REFERENCE_TOTAL_BYTES: usize = 96 * 1024;

pub(crate) enum UiAgentMessage {
    Delta(String),
    Reset,
    Committed,
    Sessions {
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
    },
    Completed(Box<std::result::Result<UiConversationAnswer, String>>),
}

pub(crate) struct UiConversationAnswer {
    pub(crate) answer: ngy_book_studio::ai::agent_chat::ConversationAnswer,
    pub(crate) sources: Vec<AiSourceLink>,
    pub(crate) source_status: AgentAnswerSourceStatus,
}

pub(crate) struct UiSessionState {
    pub(crate) active_thread_id: Option<String>,
    pub(crate) threads: Vec<AiThreadOption>,
    pub(crate) messages: Vec<AiRestoredMessage>,
}
