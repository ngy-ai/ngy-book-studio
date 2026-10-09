//! The messages the sidebar receives and the events it emits.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiRestoredRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiRestoredMessage {
    pub role: AiRestoredRole,
    pub content: String,
    pub sources: Vec<AiSourceLink>,
    pub source_status: Option<AgentAnswerSourceStatus>,
    /// Persisted `chat_messages.id` of this restored message. Editing or
    /// regenerating anchors the stored thread on it.
    pub db_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiQuestionRequest {
    pub request_id: u64,
    pub question: String,
    pub book_ids: Vec<String>,
    pub book_titles: Vec<(String, String)>,
    pub reference_hints: Vec<AiReferenceHint>,
    /// Compatibility bridge for the Editor's exact WebView snapshot barrier.
    /// The controller only lets this replace the matching item in
    /// `reference_hints`; it never widens the request scope.
    pub reference: Option<AiReferenceHint>,
    /// Persisted message ID the stored thread is rewound to before this
    /// question is appended. `None` is an ordinary follow-up question; `Some`
    /// is the user editing a question or regenerating an answer, which
    /// replaces the stored turn instead of adding a second copy of it.
    pub truncate_from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiSidebarEvent {
    Submit(AiQuestionRequest),
    Cancel {
        request_id: u64,
    },
    NewSession,
    SwitchSession {
        thread_id: String,
    },
    DeleteSession {
        thread_id: String,
    },
    /// Deletes one persisted question together with its answer. `user_db_id`
    /// is the recorded `chat_messages.id` of that question.
    DeleteTurn {
        user_db_id: String,
    },
    ScopeChanged {
        book_ids: Vec<String>,
    },
    OpenSource(AiSourceLink),
}

/// Published once for an accepted current request after the backend has
/// committed the final answer to the conversation store. Consumers must match
/// `request_id` against their own frozen action before creating derived data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiAnswerCompleted {
    pub request_id: u64,
    pub markdown: String,
}

/// Binds an explanation's frozen selection to its exact request generation.
/// Emitted immediately before the ordinary `AiSidebarEvent::Submit` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiExplanationSubmitted {
    pub request_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiExplanationFailed {
    /// `None` means the fresh explanation session failed before submission.
    pub request_id: Option<u64>,
    pub error: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiMessageRole {
    User,
    Assistant,
    Status,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiMessage {
    pub(crate) id: u64,
    pub(crate) role: AiMessageRole,
    pub(crate) content: String,
    pub(crate) sources: Vec<AiSourceLink>,
    pub(crate) source_status: Option<AgentAnswerSourceStatus>,
    pub(crate) request_id: Option<u64>,
    /// Persisted `chat_messages.id` of this message in the stored thread.
    ///
    /// Restored sessions fill it immediately; a live answer fills it once the
    /// backend reports the stored question and answer. While it is `None`
    /// (a request that never reached persistence, or a failure after the
    /// question was stored) editing and regenerating cannot anchor the stored
    /// thread and must not touch it.
    pub(crate) db_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestState {
    Idle,
    Sending { request_id: u64 },
}
