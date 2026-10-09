//! The question, the answer and the prepared request handed to the agent.

use super::*;

#[derive(Clone, Debug)]
pub struct ConversationQuestion {
    pub request_id: u64,
    pub question: String,
    pub allowed_book_ids: Vec<String>,
    pub book_titles: Vec<(String, String)>,
    pub snapshots: Vec<SelectionSnapshot>,
    /// Persisted message ID the stored thread must be rewound to before this
    /// question is appended. `None` asks normally; `Some` replaces the turn
    /// that starts at that message (the UI's edit/regenerate).
    pub truncate_from: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ConversationAnswer {
    pub thread_id: String,
    pub answer: AgentAnswer,
    pub stored_message: StoredChatMessage,
    /// Persisted ID of the user question that produced this answer, so the UI
    /// can anchor a later edit or regeneration on the exact stored turn.
    pub user_message_id: String,
}

/// A host-owned request lease registered before any asynchronous preparation.
///
/// Keeping this value alive makes the request visible to [`AgentConversation::cancel`]
/// while editor references are still being frozen. Dropping it cancels and
/// unregisters only this exact request generation. Model routes and sampling
/// controls are frozen so settings changes affect only subsequent requests.
#[must_use = "a prepared AI request must be executed or explicitly dropped"]
pub struct PreparedAgentRequest {
    pub(crate) request_id: u64,
    pub(crate) cancellation: AgentCancellation,
    pub(crate) ai_snapshot: AiRequestSnapshot,
    pub(crate) active: Arc<Mutex<HashMap<u64, AgentCancellation>>>,
}

impl PreparedAgentRequest {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn cancellation(&self) -> AgentCancellation {
        self.cancellation.clone()
    }
}

impl std::fmt::Debug for PreparedAgentRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedAgentRequest")
            .field("request_id", &self.request_id)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Drop for PreparedAgentRequest {
    fn drop(&mut self) {
        tracing::debug!(
            target: "ngy_ai",
            trace_id = self.cancellation.trace_id(),
            request_id = self.request_id,
            cancelled = self.cancellation.is_cancelled(),
            "AI request lease released"
        );
        self.cancellation.cancel();
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if active
            .get(&self.request_id)
            .is_some_and(|token| token.same_request(&self.cancellation))
        {
            active.remove(&self.request_id);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConversationSelection {
    /// No restoration decision has been made for this window yet.
    Uninitialized,
    /// The user explicitly requested a clean conversation.
    Fresh,
    /// This window owns the persisted thread through `ConversationLease`.
    Selected(String),
}
