//! AI backend: OpenAI-compatible model access plus the read-only Q&A agent,
//! its conversation store, retrieval/grounding and the durable AI workers.
//!
//! The domain and UI only depend on [`OpenAiCompatibleProvider`].  In
//! particular, nothing in this module assumes that the endpoint is Ollama;
//! Ollama is merely the safe, loopback default.

pub mod agent;
pub mod agent_chat;
pub mod agent_runtime;
pub mod ai_diagnostics;
pub mod chat;
pub mod credentials;
pub mod indexing;
pub mod job_diagnostics;
pub mod search;
pub mod translation;
pub mod web_search;

use std::{
    fmt,
    net::IpAddr,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::{FutureExt as _, Stream, StreamExt as _, future::BoxFuture, stream};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;

use crate::ai::ai_diagnostics::{error_kind, finish_reason_label, safe_label, tool_label};

mod endpoint;

mod error;

mod http;

mod provider;

mod rate_limit;

mod settings;

mod sse;

mod sse_diagnostics;

mod stream_event;

mod validate;

mod wire;

pub use endpoint::*;

pub use error::*;

pub use provider::*;

pub(crate) use provider::chat_request_body;

pub use rate_limit::*;

pub use settings::*;

pub use wire::*;

pub(crate) use http::*;

pub(crate) use sse::*;

pub(crate) use sse_diagnostics::*;

pub(crate) use stream_event::*;

pub(crate) use validate::*;

#[cfg(test)]
mod tests;
pub const DEFAULT_OLLAMA_OPENAI_BASE_URL: &str = "http://127.0.0.1:11434/v1/";
pub const DEFAULT_AI_REQUEST_TIMEOUT_SECS: u64 = 120;
pub const MIN_AI_REQUEST_TIMEOUT_SECS: u64 = 1;
pub const MAX_AI_REQUEST_TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_CHAT_OUTPUT_TOKENS: u32 = 4096;
/// 整本翻译的默认输出上限。与问答分开：一个文本块的输出规模由它的片段数决定，这个
/// 值就是翻译任务一直使用的量（`TranslationGenerationSettings` 的默认值）。
pub const DEFAULT_TRANSLATION_OUTPUT_TOKENS: u32 = 4_096;
/// Whether AI and web-search requests honour the system proxy
/// (`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`) by default. reqwest reads those
/// variables on its own, so "on" preserves the behaviour of a machine that
/// exports them; the AI settings window exposes the switch because a proxy also
/// captures requests to a *local* model endpoint (the default Ollama URL),
/// where it cannot serve them.
pub const DEFAULT_AI_USE_PROXY: bool = true;
/// Deadline for the TCP/TLS handshake only. Every other deadline is expressed
/// through [`ProviderConfig::request_timeout_secs`].
pub(crate) const AI_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Interval of the content-free transport progress line of one streaming reply.
pub(crate) const STREAM_PROGRESS_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_CONTEXT_ERROR_DEPTH: usize = 8;
pub(crate) const MAX_MODELS_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_MODEL_ENTRIES: usize = 4096;
pub(crate) const MAX_MODEL_FIELD_BYTES: usize = 4096;
pub(crate) const MAX_EMBEDDINGS_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_EMBEDDING_COUNT: usize = 2048;
pub const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;
pub(crate) const MAX_CHAT_STREAM_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_CHAT_EVENT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_CHAT_EVENTS: usize = 65_536;
pub(crate) static NEXT_HTTP_ID: AtomicU64 = AtomicU64::new(1);

pub type ChatEventStream = Pin<Box<dyn Stream<Item = Result<ChatStreamEvent>> + Send>>;
