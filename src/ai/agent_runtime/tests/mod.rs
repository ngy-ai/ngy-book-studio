//! Shared fixtures for the agent runtime tests.

use std::{
    collections::VecDeque,
    sync::{Mutex, atomic::AtomicUsize},
};

use anyhow::anyhow;
use futures_util::{FutureExt as _, stream};

use super::*;
use crate::{
    ai::agent::{SearchRequest, WebSearchResult},
    ai::{
        ChatEventStream, ChatStreamEvent, EmbeddingBatch, EmbeddingRequest, ModelInfo,
        ToolCallDelta,
    },
};

#[derive(Clone)]
pub(crate) struct MockProvider {
    turns: Arc<Mutex<VecDeque<Vec<ChatStreamEvent>>>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}

impl OpenAiCompatibleProvider for MockProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        self.requests.lock().unwrap().push(request);
        let turn = self.turns.lock().unwrap().pop_front();
        async move {
            let events = turn.context("unexpected provider call")?;
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as ChatEventStream)
        }
        .boxed()
    }

    fn embeddings(&self, _request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async { Err(anyhow!("not used")) }.boxed()
    }
}

#[derive(Clone, Default)]
pub(crate) struct RejectContextProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    cancel: Option<AgentCancellation>,
    incomplete_tool_at: Option<usize>,
}

impl OpenAiCompatibleProvider for RejectContextProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        async move {
            let count = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                requests.len()
            };
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
            if self.incomplete_tool_at == Some(count) {
                return Err(IncompleteToolArguments {
                    tool_name: "read_passages".into(),
                }
                .into());
            }
            Err(ContextWindowExceeded {
                prompt_tokens: None,
                context_tokens: None,
            }
            .into())
        }
        .boxed()
    }

    fn embeddings(&self, _request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async { Err(anyhow!("not used")) }.boxed()
    }
}

pub(crate) fn question_with_history() -> AgentQuestion {
    AgentQuestion {
        question: "Keep this current question intact.".into(),
        allowed_book_ids: vec!["book-1".into()],
        book_titles: Vec::new(),
        history: (0..40)
            .flat_map(|index| {
                [
                    ChatMessage::text(ChatRole::User, format!("old question {index}")),
                    ChatMessage::text(ChatRole::Assistant, "old answer ".repeat(100)),
                ]
            })
            .collect(),
        snapshots: vec![authorized_snapshot()],
    }
}

#[tokio::test]
pub(crate) async fn context_retries_are_bounded_and_preserve_policy_question_and_frozen_selection()
{
    let provider = RejectContextProvider::default();
    let error = runtime(provider.clone())
        .answer(question_with_history(), None, AgentCancellation::default())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<ContextWindowExceeded>().is_some());
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), MAX_CONTEXT_RETRIES + 1);
    for pair in requests.windows(2) {
        assert!(pair[1].messages.len() < pair[0].messages.len());
        assert_eq!(pair[1].messages[0], pair[0].messages[0]);
        assert_eq!(pair[1].messages.last(), pair[0].messages.last());
        assert_eq!(pair[1].tools, pair[0].tools);
        assert_eq!(pair[1].messages[1].role, ChatRole::User);
    }
    assert_eq!(requests.last().unwrap().messages.len(), 2);
    assert_eq!(requests.last().unwrap().max_tokens, Some(1024));
    let prompt = serde_json::to_string(requests.last().unwrap().messages.last().unwrap()).unwrap();
    assert!(prompt.contains("Keep this current question intact."));
    assert!(prompt.contains("exact unsaved selection"));
}

#[tokio::test]
pub(crate) async fn cancellation_between_context_retries_stops_before_another_provider_call() {
    let cancellation = AgentCancellation::default();
    let provider = RejectContextProvider {
        cancel: Some(cancellation.clone()),
        ..Default::default()
    };
    let error = runtime(provider.clone())
        .answer(question_with_history(), None, cancellation)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
pub(crate) async fn cancellation_between_tool_argument_retries_stops_before_another_provider_call()
{
    let cancellation = AgentCancellation::default();
    let provider = RejectContextProvider {
        cancel: Some(cancellation.clone()),
        incomplete_tool_at: Some(1),
        ..Default::default()
    };
    let error = runtime(provider.clone())
        .answer(question_with_history(), None, cancellation)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
pub(crate) async fn tool_recovery_does_not_reset_context_retry_budget_or_change_frozen_sources() {
    let provider = RejectContextProvider {
        incomplete_tool_at: Some(2),
        ..Default::default()
    };
    let error = runtime(provider.clone())
        .answer(question_with_history(), None, AgentCancellation::default())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<ContextWindowExceeded>().is_some());
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), MAX_CONTEXT_RETRIES + 2);
    assert_eq!(&requests[2].messages[1..], &requests[1].messages[1..]);
    assert_eq!(requests[2].max_tokens, requests[1].max_tokens);
    assert_eq!(requests[2].temperature, Some(0.0));
    for request in requests.iter() {
        assert_eq!(request.tools, requests[0].tools);
        assert_eq!(request.messages.last(), requests[0].messages.last());
    }
    let Some(MessageContent::Text(original)) = &requests[0].messages[0].content else {
        panic!("system policy must be text")
    };
    for request in &requests[2..] {
        let Some(MessageContent::Text(policy)) = &request.messages[0].content else {
            panic!("system policy must be text")
        };
        assert_eq!(
            policy,
            &format!("{original}{TOOL_ARGUMENT_RECOVERY_GUIDANCE}")
        );
    }
}

mod citations_test;

mod generation_test;

mod history_test;

mod protocol;

mod streaming_test;

mod tools_test;

mod web_test;
pub(crate) use generation_test::*;
pub(crate) use history_test::*;
pub(crate) use protocol::*;
