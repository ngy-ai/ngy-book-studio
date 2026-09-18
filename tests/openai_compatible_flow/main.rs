use std::{
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::Result;
use futures_util::{FutureExt as _, StreamExt as _, future::BoxFuture};
use ngy_book_studio::{
    ai::agent::{
        AgentAnswerSourceStatus, AgentLimits, BookBackend, BookOutlineRecord, OutlineRequest,
        PassageRecord, ReadPassagesRequest, SearchBackend, SearchMode, SearchRequest,
    },
    ai::agent_chat::{AgentConversation, ConversationQuestion},
    ai::agent_runtime::{AgentCancellation, AgentQuestion, AgentRunEvent, AgentRuntime},
    ai::chat::ChatWindowKind,
    ai::credentials::MemoryCredentialStore,
    ai::{
        ChatGenerationSettings, ChatMessage, ChatRequest, ChatRole, EmbeddingRequest,
        OpenAiCompatibleProvider, OpenAiHttpProvider, ProviderConfig,
    },
    document::{DocumentLocator, Revision},
    services::{ApiKeyUpdate, AppServices, ProviderSettings},
};
use tracing::instrument::WithSubscriber as _;

mod backend;

mod diagnostics;

mod server;

mod a_test;

mod b_test;

mod c_test;

mod d_test;

mod e_test;

pub(crate) use backend::*;

pub(crate) use diagnostics::*;

pub(crate) use server::*;
