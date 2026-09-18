//! Persistent per-window conversations backed by the read-only agent runtime.

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context as _, Result, bail, ensure};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tracing::Instrument as _;

use crate::{
    ai::agent::{AgentAnswer, SearchBackend, SelectionSnapshot},
    ai::agent_runtime::{
        AgentCancellation, AgentQuestion, AgentRequestCancelled, AgentRunEvent, AgentRuntime,
    },
    ai::ai_diagnostics::error_kind,
    ai::chat::{
        ChatRepository, ChatScope, ChatSession, ChatThread, ChatWindowKind, NewChatCitation,
        NewChatMessage, NewChatThread, StoredChatMessage,
    },
    ai::{ChatMessage, ChatRole},
    document::{DocumentLocator, SourceLocator},
    library::LibraryStore,
    services::{AiRequestSnapshot, AppServices},
};

mod app;

mod ask;

mod ask_inner;

mod authorize;

mod cancel;

mod lease;

mod request;

mod session;

mod stage;

mod text;

pub use request::*;

pub(crate) use authorize::*;

pub(crate) use lease::*;

pub(crate) use session::*;

pub(crate) use stage::*;

pub(crate) use text::*;

#[cfg(test)]
mod tests;
pub(crate) const HISTORY_MESSAGES: usize = 96;
pub(crate) const HISTORY_BYTES: usize = 384 * 1024;
pub(crate) const CREATE_CLAIM_ATTEMPTS: usize = 8;

pub(crate) static NEXT_CONVERSATION_OWNER: AtomicU64 = AtomicU64::new(1);
pub(crate) static THREAD_CLAIMS: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, u64>>>> =
    OnceLock::new();

/// State owned by one Library, Reader or Editor window.
///
/// Clones refer to the same persisted thread and cancellation set. The host
/// computes `allowed_book_ids` from current window state on every submit.
#[derive(Clone)]
pub struct AgentConversation {
    pub(crate) services: Arc<AppServices>,
    pub(crate) window_kind: ChatWindowKind,
    pub(crate) primary_book_id: Option<String>,
    pub(crate) selection: Arc<AsyncMutex<ConversationSelection>>,
    pub(crate) lease: Arc<ConversationLease>,
    pub(crate) active: Arc<Mutex<HashMap<u64, AgentCancellation>>>,
    pub(crate) session_transition: Arc<AtomicBool>,
}
