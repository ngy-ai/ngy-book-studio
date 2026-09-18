//! Tests for history reduction.

pub(crate) use super::super::*;
pub(crate) use super::*;

#[test]
pub(crate) fn history_reduction_preserves_current_question_and_appended_web_sources() {
    let mut request = ChatRequest {
        model: "chat-model".into(),
        messages: vec![
            ChatMessage::text(ChatRole::System, "policy"),
            ChatMessage::text(ChatRole::Assistant, "leading orphan"),
            ChatMessage::text(ChatRole::User, "old question"),
            ChatMessage::text(ChatRole::Assistant, "old answer"),
            ChatMessage::text(ChatRole::User, "current question"),
            ChatMessage::text(ChatRole::User, "host web sources"),
        ],
        tools: Vec::new(),
        temperature: None,
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        max_tokens: Some(4096),
        reasoning_effort: None,
    };
    let original = request.clone();
    let mut context = RequestContext {
        round: 1,
        history_messages: 3,
        max_output_tokens: 4096,
        tool_arguments_retried: false,
    };
    assert!(reduce_request_history(
        &mut request,
        &mut context,
        &ContextWindowExceeded {
            prompt_tokens: Some(4155),
            context_tokens: Some(4096)
        },
        MAX_CONTEXT_RETRIES - 1,
    ));
    assert_eq!(
        request.messages,
        vec![
            original.messages[0].clone(),
            original.messages[4].clone(),
            original.messages[5].clone()
        ]
    );
    assert_eq!(context.history_messages, 0);
    let reduced = request.clone();
    assert!(!reduce_request_history(
        &mut request,
        &mut context,
        &ContextWindowExceeded {
            prompt_tokens: Some(4155),
            context_tokens: Some(4096)
        },
        0,
    ));
    assert_eq!(request, reduced);
}

#[derive(Clone)]
pub(crate) struct SelectionAliasProvider {
    split_marker: bool,
    pub(crate) requests: Arc<Mutex<Vec<ChatRequest>>>,
}

impl SelectionAliasProvider {
    pub(crate) fn new(split_marker: bool) -> Self {
        Self {
            split_marker,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl OpenAiCompatibleProvider for SelectionAliasProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        let result = selection_alias_from_request(&request).map(|alias| {
            let marker = format!("[[ngy-source:{alias}]]");
            let chunks = if self.split_marker {
                let split = marker.len() / 2;
                vec![
                    "first ".to_string(),
                    "second ".to_string(),
                    marker[..split].to_string(),
                    marker[split..].to_string(),
                    " done".to_string(),
                ]
            } else {
                vec![format!("From the selection. {marker}")]
            };
            let mut events = chunks
                .into_iter()
                .map(|chunk| event(Some(&chunk), None, None, false))
                .collect::<Vec<_>>();
            events.last_mut().unwrap().finish_reason = Some("stop".to_string());
            events.push(event(None, None, None, true));
            events
        });
        self.requests.lock().unwrap().push(request);
        async move {
            let events = result?;
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as ChatEventStream)
        }
        .boxed()
    }

    fn embeddings(&self, _request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async { Err(anyhow!("not used")) }.boxed()
    }
}
