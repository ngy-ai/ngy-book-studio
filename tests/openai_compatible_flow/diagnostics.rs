//! Log capture and the diagnostic assertions built on it.

use super::*;

#[derive(Clone, Default)]
pub(crate) struct AiLogCapture(Arc<Mutex<Vec<u8>>>);

impl AiLogCapture {
    pub(super) fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        let writer = self.clone();
        tracing_subscriber::fmt()
            .with_env_filter("off,ngy_ai=debug")
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish()
    }

    pub(super) fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for AiLogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn diagnostic_u64_field(line: &str, name: &str) -> Option<u64> {
    line.split_once(&format!("{name}="))?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

pub(crate) fn diagnostic_test_question() -> AgentQuestion {
    AgentQuestion {
        question: "PRIVATE_QUESTION".to_string(),
        allowed_book_ids: vec!["book-1".to_string()],
        book_titles: vec![("book-1".to_string(), "PRIVATE_BOOK_TITLE".to_string())],
        history: vec![
            ChatMessage::text(ChatRole::User, "PRIVATE_PREVIOUS_QUESTION"),
            ChatMessage::text(ChatRole::Assistant, "PRIVATE_BOOK_TEXT"),
        ],
        snapshots: Vec::new(),
    }
}

pub(crate) fn diagnostic_test_runtime(base_url: String, search: &RecordingSearch) -> AgentRuntime {
    AgentRuntime::new(
        Arc::new(
            OpenAiHttpProvider::new(ProviderConfig {
                base_url,
                api_key: Some("PRIVATE_API_KEY".to_string()),
                request_timeout_secs: 10,
                ..ProviderConfig::default()
            })
            .unwrap(),
        ),
        Arc::new(search.clone()),
        Arc::new(EmptyBooks),
        "chat-model",
        AgentLimits::default(),
    )
    .unwrap()
}

pub(crate) async fn assert_tool_rejection_stops(
    question: AgentQuestion,
    errors: Vec<String>,
    expected_error: &str,
) {
    let expected_requests = errors.len();
    let (base_url, requests, server) = scripted_chat_http_server(
        errors
            .into_iter()
            .map(|body| ("500 Internal Server Error", "application/json", body))
            .collect(),
    );
    let search = RecordingSearch::default();
    let runtime = context_test_runtime(base_url, &search);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let error = runtime
        .answer(question, Some(events_tx), AgentCancellation::default())
        .await
        .unwrap_err();
    server.join().unwrap();
    let error = format!("{error:#}");
    assert!(error.contains(expected_error), "{error}");
    assert_eq!(requests.lock().unwrap().len(), expected_requests);
    assert!(search.requests.lock().unwrap().is_empty());
    assert!(events_rx.recv().await.is_none());
}

pub(crate) fn incomplete_tool_arguments_error(tool: &str) -> String {
    serde_json::json!({
        "error": {
            "message": format!(
                "llama-server returned invalid tool call arguments for \"{tool}\": unexpected end of JSON input"
            ),
            "type": "api_error",
            "param": null,
            "code": null
        }
    })
    .to_string()
}

pub(crate) fn tool_sse(id: &str, name: &str, arguments: &str) -> String {
    let event = serde_json::json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }]},
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}
