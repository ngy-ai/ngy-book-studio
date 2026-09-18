//! OpenAI-compatible read-only agent execution.
//!
//! This module is the orchestration layer between the streaming provider and
//! the three host-authorized tools from [`crate::ai::agent`].  It never accepts a
//! database handle or a wider scope from the model.

pub(super) use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

pub(super) use anyhow::{Context as _, Result, bail, ensure};
pub(super) use futures_util::{FutureExt as _, StreamExt as _, future::BoxFuture};
pub(super) use tokio::sync::{Notify, mpsc};
pub(super) use tracing::Instrument as _;

pub(super) use crate::{
    ai::agent::{
        AgentAnswer, AgentAnswerSourceStatus, AgentCitation, AgentLimits, AgentStreamAccumulator,
        AllowedBookScope, BookBackend, BookOutlineRecord, CitationRegistry, NO_SOURCE_MARKER,
        OutlineNodeRecord, OutlineRequest, PassageRecord, ReadOnlyAgent, ReadPassagesRequest,
        SearchBackend, SelectionSnapshot, WebSearchBackend, WebSearchRequest,
        agent_tool_definitions,
    },
    ai::ai_diagnostics::{
        ToolArgumentsSummary, error_kind, finish_reason_label, safe_label, tool_label,
    },
    ai::{
        ChatEventStream, ChatGenerationSettings, ChatMessage, ChatRequest, ChatRole,
        ContextWindowExceeded, IncompleteToolArguments, MessageContent, OpenAiCompatibleProvider,
        ReasoningEffort,
    },
    db,
    document::{DocumentLocator, Revision, SourceLocator},
};

mod answer;

mod answer_events;

mod app;

mod backend;

mod budget;

mod cancellation;

mod citation;

mod event;

mod history;

mod projection;

mod provider;

mod request;

pub use backend::SqliteBookBackend;

pub use cancellation::{AgentCancellation, AgentRequestCancelled};

pub use event::{AgentQuestion, AgentRunEvent};

pub(crate) use answer_events::*;

pub(crate) use budget::*;

pub(crate) use citation::*;

pub(crate) use history::*;

pub(crate) use projection::*;

pub(crate) use request::*;

#[cfg(test)]
mod tests;
#[derive(Clone)]
pub struct AgentRuntime {
    pub(crate) provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) search: Arc<dyn SearchBackend>,
    pub(crate) books: Arc<dyn BookBackend>,
    /// Host-performed internet retrieval. `None` disables the web fallback and
    /// leaves the model answering from its own knowledge when the authorized
    /// books cannot ground an answer.
    pub(crate) web: Option<Arc<dyn WebSearchBackend>>,
    pub(crate) chat_model: String,
    pub(crate) chat_generation: ChatGenerationSettings,
    pub(crate) limits: AgentLimits,
}
