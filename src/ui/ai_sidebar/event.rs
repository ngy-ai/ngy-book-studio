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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiSidebarEvent {
    Submit(AiQuestionRequest),
    Cancel { request_id: u64 },
    NewSession,
    SwitchSession { thread_id: String },
    DeleteSession { thread_id: String },
    ScopeChanged { book_ids: Vec<String> },
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestState {
    Idle,
    Sending { request_id: u64 },
}
