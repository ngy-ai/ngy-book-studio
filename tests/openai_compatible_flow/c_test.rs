//! Tool-call scope and context-retry flows.

use super::*;

#[tokio::test]
pub(crate) async fn http_tool_call_cannot_expand_the_host_book_scope() {
    let tool_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"search_books\",\"arguments\":\"{\\\"query\\\":\\\"秘密\\\",\\\"book_ids\\\":[\\\"book-secret\\\"]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (base_url, _requests, server) = scripted_chat_server(vec![tool_turn.to_string()]);
    let provider = Arc::new(
        OpenAiHttpProvider::new(ProviderConfig {
            base_url,
            api_key: None,
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            request_timeout_secs: 10,
        })
        .unwrap(),
    );
    let search = RecordingSearch::default();
    let runtime = AgentRuntime::new(
        provider,
        Arc::new(search.clone()),
        Arc::new(EmptyBooks),
        "chat-model",
        AgentLimits::default(),
    )
    .unwrap();

    let error = runtime
        .answer(
            AgentQuestion {
                question: "读取秘密".to_string(),
                allowed_book_ids: vec!["book-1".to_string()],
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
            },
            None,
            AgentCancellation::default(),
        )
        .await
        .unwrap_err();
    server.join().unwrap();
    assert!(
        error
            .to_string()
            .contains("outside the host-authorized scope")
    );
    assert!(search.requests.lock().unwrap().is_empty());
}

#[tokio::test]
pub(crate) async fn http_nested_context_error_retries_with_complete_older_turns_removed() {
    let (base_url, requests, server) = scripted_chat_http_server(vec![
        (
            "400 Bad Request",
            "application/json",
            nested_context_error(),
        ),
        ("200 OK", "text/event-stream", answer_sse("恢复回答。")),
    ]);
    let search = RecordingSearch::default();
    let settings = custom_chat_generation();
    let runtime = context_test_runtime(base_url, &search)
        .with_chat_generation(settings.clone())
        .unwrap();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let answer = runtime
        .answer(
            context_test_question(),
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(answer.markdown, "恢复回答。");
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    assert_eq!(
        events_rx.recv().await,
        Some(AgentRunEvent::AnswerDelta("恢复回答。".to_string()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerCommitted));
    assert!(events_rx.recv().await.is_none());
    assert!(search.requests.lock().unwrap().is_empty());

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let original = chat_request_body(&requests[0]);
    let retried = chat_request_body(&requests[1]);
    assert_chat_sampling_parameters(&original, &settings);
    assert_chat_sampling_parameters(&retried, &settings);
    assert_eq!(original["max_tokens"], settings.max_output_tokens);
    assert_eq!(original["max_tokens"], 131_072);
    assert_history_trim_preserves_current_turn(&original, &retried, 1);
    assert!(retried["max_tokens"].as_u64().unwrap() <= 1024);
    assert!(retried["max_tokens"].as_u64().unwrap() > 0);
    assert!(retried["max_tokens"].as_u64() < original["max_tokens"].as_u64());
    assert_eq!(original["tools"], retried["tools"]);
    assert_eq!(original["reasoning_effort"], retried["reasoning_effort"]);
}

#[tokio::test]
pub(crate) async fn http_context_retry_does_not_raise_a_small_configured_output_limit() {
    let (base_url, requests, server) = scripted_chat_http_server(vec![
        (
            "400 Bad Request",
            "application/json",
            nested_context_error(),
        ),
        ("200 OK", "text/event-stream", answer_sse("简短回答。")),
    ]);
    let settings = ChatGenerationSettings {
        max_output_tokens: 128,
        ..custom_chat_generation()
    };
    let runtime = context_test_runtime(base_url, &RecordingSearch::default())
        .with_chat_generation(settings.clone())
        .unwrap();
    runtime
        .answer(context_test_question(), None, AgentCancellation::default())
        .await
        .unwrap();
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(chat_request_body(&requests[0])["max_tokens"], 128);
    for request in requests.iter() {
        let request = chat_request_body(request);
        assert_chat_sampling_parameters(&request, &settings);
        let output_limit = request["max_tokens"].as_u64().unwrap();
        assert!((1..=128).contains(&output_limit));
    }
}

#[tokio::test]
pub(crate) async fn http_context_retry_keeps_tool_results_and_citations_without_reexecuting_tools()
{
    let tool_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"search_books\",\"arguments\":\"{\\\"query\\\":\\\"可信\\\",\\\"book_ids\\\":[\\\"book-1\\\"]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let context_error = serde_json::json!({
        "error": {
            "message": "This model's maximum context length is 4096 tokens.",
            "type": "invalid_request_error",
            "code": "context_length_exceeded",
            "n_prompt_tokens": 4155,
            "n_ctx": 4096
        }
    })
    .to_string();
    let (base_url, requests, server) = scripted_chat_http_server(vec![
        ("200 OK", "text/event-stream", tool_turn.to_string()),
        ("400 Bad Request", "application/json", context_error),
        (
            "200 OK",
            "text/event-stream",
            answer_sse("重试后仍有依据。 [[ngy-source:passage:passage-1]]"),
        ),
    ]);
    let search = RecordingSearch::default();
    let runtime = context_test_runtime(base_url, &search);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let answer = runtime
        .answer(
            context_test_question(),
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(answer.markdown, "重试后仍有依据。 ");
    assert_eq!(answer.citations.len(), 1);
    assert_eq!(answer.citations[0].citation_id, "passage:passage-1");
    assert_eq!(answer.citations[0].book_id, "book-1");
    assert_eq!(answer.citations[0].document_revision, Revision::new(1));
    assert_eq!(answer.citations[0].unit_revision, Revision::new(1));
    {
        let search_requests = search.requests.lock().unwrap();
        assert_eq!(search_requests.len(), 1);
        assert_eq!(search_requests[0].book_ids, ["book-1"]);
    }
    let mut events = Vec::new();
    while let Some(event) = events_rx.recv().await {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRunEvent::ToolStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRunEvent::ToolFinished { .. }))
            .count(),
        1
    );
    assert_eq!(events.last(), Some(&AgentRunEvent::AnswerCommitted));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let before_retry = chat_request_body(&requests[1]);
    let retried = chat_request_body(&requests[2]);
    assert_history_trim_preserves_current_turn(&before_retry, &retried, 3);
    let messages = retried["messages"].as_array().unwrap();
    let assistant = &messages[messages.len() - 2];
    let tool = messages.last().unwrap();
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["tool_calls"][0]["id"], "call-1");
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "call-1");
    assert!(
        tool["content"]
            .as_str()
            .unwrap()
            .contains("passage:passage-1")
    );
    assert!(retried["max_tokens"].as_u64().unwrap() <= 1024);
}

#[tokio::test]
pub(crate) async fn http_first_question_context_retry_trims_whole_tool_records_and_keeps_best_citation()
 {
    assert_first_question_tool_context_retry(
        "根据保留的首条证据回答。 [[ngy-source:passage:long-1]]",
        true,
    )
    .await;
}

#[tokio::test]
pub(crate) async fn http_first_question_context_retry_rejects_citation_from_removed_tool_record() {
    assert_first_question_tool_context_retry(
        "不能接受已移除的证据。 [[ngy-source:passage:long-4]]",
        false,
    )
    .await;
}

pub(crate) async fn assert_first_question_tool_context_retry(final_answer: &str, accepted: bool) {
    let tool_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"search_books\",\"arguments\":\"{\\\"query\\\":\\\"完整证据\\\",\\\"book_ids\\\":[\\\"book-1\\\"]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (base_url, requests, server) = scripted_chat_http_server(vec![
        ("200 OK", "text/event-stream", tool_turn.to_string()),
        (
            "400 Bad Request",
            "application/json",
            nested_context_error(),
        ),
        ("200 OK", "text/event-stream", answer_sse(final_answer)),
    ]);
    let search = LongRecordingSearch::default();
    let runtime = context_test_runtime(base_url, &search);
    let mut question = context_test_question();
    question.history.clear();
    let current_question = question.question.clone();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = runtime
        .answer(question, Some(events_tx), AgentCancellation::default())
        .await;
    assert_eq!(
        requests.lock().unwrap().len(),
        3,
        "the first question must reach the shortened retry: {result:?}"
    );
    server.join().unwrap();

    let mut events = Vec::new();
    while let Some(event) = events_rx.recv().await {
        events.push(event);
    }
    if accepted {
        let answer = result.unwrap();
        assert_eq!(answer.markdown, "根据保留的首条证据回答。 ");
        assert_eq!(answer.citations.len(), 1);
        assert_eq!(answer.citations[0].citation_id, "passage:long-1");
        assert_eq!(answer.citations[0].book_id, "book-1");
        assert_eq!(answer.citations[0].unit_id, "unit-1");
        assert_eq!(events.last(), Some(&AgentRunEvent::AnswerCommitted));
    } else {
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("unknown or unserved citation: passage:long-4"),
            "{error}"
        );
        assert!(!events.contains(&AgentRunEvent::AnswerCommitted));
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRunEvent::ToolStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRunEvent::ToolFinished { .. }))
            .count(),
        1
    );
    let search_requests = search.requests.lock().unwrap();
    assert_eq!(search_requests.len(), 1);
    assert_eq!(search_requests[0].book_ids, ["book-1"]);
    drop(search_requests);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let original = chat_request_body(&requests[1]);
    let retried = chat_request_body(&requests[2]);
    assert!(retried.to_string().len() + 2048 < original.to_string().len());
    let original_messages = original["messages"].as_array().unwrap();
    let retried_messages = retried["messages"].as_array().unwrap();
    assert_eq!(original_messages.len(), 4);
    assert_eq!(retried_messages.len(), 4);
    assert_eq!(&retried_messages[..3], &original_messages[..3]);
    assert_eq!(retried_messages[1]["content"], current_question);
    assert_eq!(retried_messages[2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(retried_messages[3]["role"], "tool");
    assert_eq!(retried_messages[3]["tool_call_id"], "call-1");
    assert_eq!(retried["tools"], original["tools"]);
    let original_tool: serde_json::Value =
        serde_json::from_str(original_messages[3]["content"].as_str().unwrap()).unwrap();
    let retried_tool: serde_json::Value =
        serde_json::from_str(retried_messages[3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(original_tool["truncated"], false);
    assert_eq!(retried_tool["truncated"], true);
    let original_records = original_tool["results"].as_array().unwrap();
    let retained_records = retried_tool["results"].as_array().unwrap();
    assert_eq!(original_records.len(), 4);
    assert!(!retained_records.is_empty());
    assert!(retained_records.len() < original_records.len());
    assert_eq!(retained_records[0]["citation_id"], "passage:long-1");
    assert_eq!(
        retained_records.as_slice(),
        &original_records[..retained_records.len()],
        "retained evidence must remain byte-exact, with only trailing records removed"
    );
    assert!(
        retained_records
            .iter()
            .all(|record| record["citation_id"] != "passage:long-4")
    );
}
