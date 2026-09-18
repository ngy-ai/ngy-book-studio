//! The question a caller asks and the events the runtime publishes.

pub(super) use super::*;

#[derive(Clone, Debug)]
pub struct AgentQuestion {
    pub question: String,
    pub allowed_book_ids: Vec<String>,
    /// (id, title) pairs for the host-authorized books. Included in the system
    /// prompt so the model can determine whether a question is related to the
    /// books before calling search tools.
    pub book_titles: Vec<(String, String)>,
    /// Previous persisted user/assistant turns. System and tool messages are
    /// deliberately rebuilt by the host for every question.
    pub history: Vec<ChatMessage>,
    /// Byte-exact unsaved editor selections frozen at submit time.
    pub snapshots: Vec<SelectionSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunEvent {
    ToolStarted {
        name: String,
    },
    ToolFinished {
        name: String,
        citation_count: usize,
    },
    /// Marker-free final-answer text. Text emitted by an intermediate
    /// tool-selection turn and the private citation syntax are never retained.
    /// Deltas are provisional until [`Self::AnswerCommitted`].
    AnswerDelta(String),
    /// Discards all provisional answer deltas for the current question. This
    /// is emitted when a response that began with text later becomes a tool
    /// call turn.
    AnswerReset,
    /// The streamed, marker-free answer passed citation/no-source validation.
    /// Absence of this event means consumers must discard prior deltas.
    AnswerCommitted,
}
