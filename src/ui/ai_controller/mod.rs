//! GPUI adapter for persistent read-only AI conversations.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context as _, Result, bail};
use gpui_kit::{Context, Entity, Task};
use ngy_book_studio::{
    ai::ChatRole,
    ai::agent::{AgentAnswerSourceStatus, AgentCitation, SelectionSnapshot},
    ai::agent_chat::{AgentConversation, ConversationQuestion},
    ai::agent_runtime::{AgentCancellation, AgentRequestCancelled, AgentRunEvent},
    ai::ai_diagnostics::error_kind,
    ai::chat::{ChatCitation, ChatSession, ChatThread, ChatWindowKind},
    document::{BookDocument, Revision, SourceLocator},
    services::AppServices,
};
use tracing::Instrument as _;

use super::ai_sidebar::{
    AiQuestionRequest, AiReferenceHint, AiRestoredMessage, AiRestoredRole, AiSidebar, AiSourceLink,
    AiThreadOption, MAX_SELECTED_REFERENCES,
};

mod app;

mod citation;

mod error;

mod model;

mod restore_io;

mod session;

mod session_io;

mod submit;

pub(crate) use citation::*;

pub(crate) use error::*;

pub(crate) use model::*;

pub(crate) use restore_io::*;

pub(crate) use session_io::*;

#[cfg(test)]
mod tests;
pub(crate) struct AiSidebarController {
    pub(crate) services: Arc<AppServices>,
    pub(crate) conversation: AgentConversation,
    pub(crate) window_kind: ChatWindowKind,
    pub(crate) answer_task: Option<Task<()>>,
    pub(crate) session_task: Option<Task<()>>,
    pub(crate) session_generation: Arc<AtomicU64>,
}
