//! Tests for generation settings and the web helpers.

pub(crate) use super::super::*;
pub(crate) use super::*;

pub(crate) fn custom_generation() -> ChatGenerationSettings {
    ChatGenerationSettings {
        temperature: Some(0.7),
        top_p: Some(0.8),
        max_output_tokens: 128,
        presence_penalty: Some(0.3),
        frequency_penalty: Some(-0.4),
    }
}

pub(crate) fn assert_custom_generation(request: &ChatRequest) {
    let expected = custom_generation();
    assert_eq!(request.temperature, expected.temperature);
    assert_eq!(request.top_p, expected.top_p);
    assert_eq!(request.presence_penalty, expected.presence_penalty);
    assert_eq!(request.frequency_penalty, expected.frequency_penalty);
    assert_eq!(request.max_tokens, Some(expected.max_output_tokens));
}

#[test]
pub(crate) fn runtime_rejects_invalid_generation_before_calling_the_provider() {
    let provider = answer_only("unused");
    let requests = Arc::clone(&provider.requests);
    let settings = ChatGenerationSettings {
        max_output_tokens: 0,
        ..Default::default()
    };
    assert!(runtime(provider).with_chat_generation(settings).is_err());
    assert!(requests.lock().unwrap().is_empty());
}

pub(crate) async fn ask(runtime: &AgentRuntime) -> Result<AgentAnswer> {
    runtime
        .answer(
            AgentQuestion {
                question: "What is verified?".into(),
                allowed_book_ids: vec!["book-1".into()],
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
            },
            None,
            AgentCancellation::default(),
        )
        .await
}

/// Web backend returning a fixed result set.
#[derive(Clone)]
pub(crate) struct MockWeb {
    results: Vec<WebSearchResult>,
}

impl WebSearchBackend for MockWeb {
    fn web_search(
        &self,
        _request: WebSearchRequest,
    ) -> BoxFuture<'_, Result<Vec<WebSearchResult>>> {
        let results = self.results.clone();
        async move { Ok(results) }.boxed()
    }
}

pub(crate) fn web_backend(results: Vec<WebSearchResult>) -> Arc<dyn WebSearchBackend> {
    Arc::new(MockWeb { results })
}

pub(crate) fn web_result(title: &str, url: &str, snippet: &str) -> WebSearchResult {
    WebSearchResult {
        title: title.into(),
        url: url.into(),
        snippet: snippet.into(),
    }
}

/// Two turns: an ungrounded book answer, then a web-grounded answer.
pub(crate) fn ungrounded_then_web_answer(second: &str) -> MockProvider {
    MockProvider {
        turns: Arc::new(Mutex::new(VecDeque::from([
            answer_turn("I could not find that in the books."),
            answer_turn(second),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    }
}
