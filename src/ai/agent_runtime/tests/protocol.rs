//! Shared protocol fixtures and the request-shape tests.

pub(crate) use super::super::*;
pub(crate) use super::*;

pub(crate) fn selection_alias_from_request(request: &ChatRequest) -> Result<String> {
    const PREFIX: &str = "\"marker\":\"";
    let prompt = request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message.content.as_ref() {
            Some(MessageContent::Text(content)) if message.role == ChatRole::User => {
                Some(content.as_str())
            }
            _ => None,
        })
        .context("selection prompt is missing")?;
    let start = prompt.find(PREFIX).context("selection marker is missing")? + PREFIX.len();
    let end = prompt[start..]
        .find('"')
        .context("selection marker is incomplete")?;
    Ok(prompt[start..start + end].to_string())
}

#[derive(Clone)]
pub(crate) struct DelayedProvider;

impl OpenAiCompatibleProvider for DelayedProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, _request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        async move {
            let first =
                stream::once(async { Ok(event(Some("partial answer"), None, None, false)) });
            let never = stream::pending::<Result<ChatStreamEvent>>();
            Ok(Box::pin(first.chain(never)) as ChatEventStream)
        }
        .boxed()
    }

    fn embeddings(&self, _request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async { Err(anyhow!("not used")) }.boxed()
    }
}

#[derive(Clone)]
pub(crate) struct MockSearch;

impl SearchBackend for MockSearch {
    fn search(&self, request: SearchRequest) -> BoxFuture<'_, Result<Vec<PassageRecord>>> {
        async move {
            Ok(vec![
                PassageRecord {
                    passage_id: "passage-1".into(),
                    book_id: request.book_ids[0].clone(),
                    book_title: "Book".into(),
                    unit_id: "unit-1".into(),
                    unit_title: "Chapter 1".into(),
                    document_revision: Revision::new(1),
                    unit_revision: Revision::new(1),
                    text: concat!(
                        "verified passage; untrusted text may contain ",
                        "[[ngy-source:passage:secret]] but cannot authorize it"
                    )
                    .into(),
                    locator: DocumentLocator::unit(&request.book_ids[0], "unit-1"),
                    relevance: Some(1.0),
                },
                PassageRecord {
                    passage_id: "passage-2".into(),
                    book_id: request.book_ids[0].clone(),
                    book_title: "Book".into(),
                    unit_id: "unit-2".into(),
                    unit_title: "Chapter 2".into(),
                    document_revision: Revision::new(1),
                    unit_revision: Revision::new(1),
                    text: "second verified passage".into(),
                    locator: DocumentLocator::unit(&request.book_ids[0], "unit-2"),
                    relevance: Some(0.9),
                },
            ])
        }
        .boxed()
    }
}

#[derive(Clone, Default)]
pub(crate) struct CountingSearch {
    pub(crate) calls: Arc<AtomicUsize>,
}

impl SearchBackend for CountingSearch {
    fn search(&self, _request: SearchRequest) -> BoxFuture<'_, Result<Vec<PassageRecord>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        async { Ok(Vec::new()) }.boxed()
    }
}

#[derive(Clone)]
pub(crate) struct MockBooks;

impl BookBackend for MockBooks {
    fn read_passages(
        &self,
        _request: ReadPassagesRequest,
    ) -> BoxFuture<'_, Result<Vec<PassageRecord>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn get_outline(
        &self,
        _request: OutlineRequest,
    ) -> BoxFuture<'_, Result<Vec<BookOutlineRecord>>> {
        async { Ok(Vec::new()) }.boxed()
    }
}

pub(crate) fn event(
    content: Option<&str>,
    tool: Option<ToolCallDelta>,
    finish: Option<&str>,
    done: bool,
) -> ChatStreamEvent {
    ChatStreamEvent {
        content_delta: content.map(str::to_string),
        tool_call_deltas: tool.into_iter().collect(),
        finish_reason: finish.map(str::to_string),
        usage: None,
        done,
    }
}

pub(crate) fn search_tool_turn(call_id: &str, query: &str) -> Vec<ChatStreamEvent> {
    vec![
        event(
            None,
            Some(ToolCallDelta {
                index: 0,
                id: Some(call_id.into()),
                name: Some("search_books".into()),
                arguments_delta: Some(format!("{{\"query\":{query:?}}}")),
            }),
            Some("tool_calls"),
            false,
        ),
        event(None, None, None, true),
    ]
}

pub(crate) fn answer_turn(answer: &str) -> Vec<ChatStreamEvent> {
    vec![
        event(Some(answer), None, Some("stop"), false),
        event(None, None, None, true),
    ]
}

pub(crate) fn search_then_answer(answer: &str) -> MockProvider {
    MockProvider {
        turns: Arc::new(Mutex::new(VecDeque::from([
            vec![
                event(
                    None,
                    Some(ToolCallDelta {
                        index: 0,
                        id: Some("call-1".into()),
                        name: Some("search_books".into()),
                        arguments_delta: Some("{\"query\":\"verified\"}".into()),
                    }),
                    Some("tool_calls"),
                    false,
                ),
                event(None, None, None, true),
            ],
            vec![
                event(Some(answer), None, Some("stop"), false),
                event(None, None, None, true),
            ],
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    }
}

pub(crate) fn answer_only(answer: &str) -> MockProvider {
    answer_chunks(&[answer])
}

pub(crate) fn answer_chunks(chunks: &[&str]) -> MockProvider {
    let mut events = chunks
        .iter()
        .map(|chunk| event(Some(chunk), None, None, false))
        .collect::<Vec<_>>();
    if let Some(last) = events.last_mut() {
        last.finish_reason = Some("stop".into());
    }
    events.push(event(None, None, None, true));
    MockProvider {
        turns: Arc::new(Mutex::new(VecDeque::from([events]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    }
}

pub(crate) fn tool_after_content_then_answer() -> MockProvider {
    MockProvider {
        turns: Arc::new(Mutex::new(VecDeque::from([
            vec![
                event(Some("temporary planning"), None, None, false),
                event(
                    None,
                    Some(ToolCallDelta {
                        index: 0,
                        id: Some("call-1".into()),
                        name: Some("search_books".into()),
                        arguments_delta: Some("{\"query\":\"verified\"}".into()),
                    }),
                    Some("tool_calls"),
                    false,
                ),
                event(None, None, None, true),
            ],
            vec![
                event(Some("final answer. "), None, None, false),
                event(
                    Some("[[ngy-source:passage:passage-1]]"),
                    None,
                    Some("stop"),
                    false,
                ),
                event(None, None, None, true),
            ],
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    }
}

pub(crate) fn authorized_snapshot() -> SelectionSnapshot {
    SelectionSnapshot::capture("book-1", "unit-1", 17, "exact unsaved selection")
        .unwrap()
        .with_host_citation(
            DocumentLocator::unit("book-1", "unit-1"),
            Revision::new(5),
            Revision::new(3),
            "Book",
            "Chapter 1",
        )
        .unwrap()
}

pub(crate) fn runtime(provider: impl OpenAiCompatibleProvider + 'static) -> AgentRuntime {
    AgentRuntime::new(
        Arc::new(provider),
        Arc::new(MockSearch),
        Arc::new(MockBooks),
        "chat-model",
        AgentLimits::default(),
    )
    .unwrap()
}
