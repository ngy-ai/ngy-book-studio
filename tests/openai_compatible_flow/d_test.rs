//! Incomplete tool arguments and error classification flows.

use super::*;

#[tokio::test]
pub(crate) async fn http_unrelated_bad_request_is_not_retried_as_context_overflow() {
    let body = serde_json::json!({
        "error": {
            "message": "Unsupported parameter: reasoning_effort",
            "type": "invalid_request_error",
            "param": "reasoning_effort",
            "code": "unsupported_parameter"
        }
    })
    .to_string();
    let (base_url, requests, server) =
        scripted_chat_http_server(vec![("400 Bad Request", "application/json", body)]);
    let search = RecordingSearch::default();
    let runtime = context_test_runtime(base_url, &search);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let error = runtime
        .answer(
            context_test_question(),
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap_err();
    server.join().unwrap();

    let error = format!("{error:#}");
    assert!(error.contains("400 Bad Request"), "{error}");
    assert!(
        error.contains("Unsupported parameter: reasoning_effort"),
        "{error}"
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(search.requests.lock().unwrap().is_empty());
    assert!(events_rx.recv().await.is_none());
}

#[tokio::test]
pub(crate) async fn http_incomplete_read_arguments_retry_preserves_search_scope_and_citation() {
    let (base_url, requests, server) = scripted_chat_http_server(vec![
        (
            "200 OK",
            "text/event-stream",
            tool_sse("search-call", "search_books", r#"{"query":"可信"}"#),
        ),
        (
            "500 Internal Server Error",
            "application/json",
            incomplete_tool_arguments_error("read_passages"),
        ),
        (
            "200 OK",
            "text/event-stream",
            tool_sse(
                "read-call",
                "read_passages",
                r#"{"passage_ids":["passage-1"]}"#,
            ),
        ),
        (
            "200 OK",
            "text/event-stream",
            answer_sse("有依据的回答。 [[ngy-source:passage:passage-1]]"),
        ),
    ]);
    let search = RecordingSearch::default();
    let books = RecordingBooks::default();
    let settings = custom_chat_generation();
    let runtime = AgentRuntime::new(
        Arc::new(
            OpenAiHttpProvider::new(ProviderConfig {
                base_url,
                api_key: None,
                remote_content_confirmed: false,
                allow_insecure_remote_http: false,
                request_timeout_secs: 10,
            })
            .unwrap(),
        ),
        Arc::new(search.clone()),
        Arc::new(books.clone()),
        "chat-model",
        AgentLimits::default(),
    )
    .unwrap()
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

    assert_eq!(answer.markdown, "有依据的回答。 ");
    assert_eq!(answer.citations.len(), 1);
    assert_eq!(answer.citations[0].citation_id, "passage:passage-1");
    assert_eq!(answer.citations[0].book_id, "book-1");
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
    );
    {
        let search_requests = search.requests.lock().unwrap();
        assert_eq!(search_requests.len(), 1);
        assert_eq!(search_requests[0].book_ids, ["book-1"]);
        let read_requests = books.requests.lock().unwrap();
        assert_eq!(read_requests.len(), 1);
        assert_eq!(read_requests[0].book_ids, ["book-1"]);
        assert_eq!(read_requests[0].passage_ids, ["passage-1"]);
    }
    let mut started = Vec::new();
    let mut committed = 0;
    while let Some(event) = events_rx.recv().await {
        match event {
            AgentRunEvent::ToolStarted { name } => started.push(name),
            AgentRunEvent::AnswerCommitted => committed += 1,
            _ => {}
        }
    }
    assert_eq!(started, ["search_books", "read_passages"]);
    assert_eq!(committed, 1);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let rejected = chat_request_body(&requests[1]);
    let retried = chat_request_body(&requests[2]);
    let final_request = chat_request_body(&requests[3]);
    assert_chat_sampling_parameters(&chat_request_body(&requests[0]), &settings);
    assert_chat_sampling_parameters(&rejected, &settings);
    let repaired_settings = ChatGenerationSettings {
        temperature: Some(0.0),
        ..settings.clone()
    };
    assert_chat_sampling_parameters(&retried, &repaired_settings);
    assert_chat_sampling_parameters(&final_request, &settings);
    for request in requests.iter() {
        assert_eq!(
            chat_request_body(request)["max_tokens"],
            settings.max_output_tokens
        );
    }
    assert_eq!(retried["tools"], rejected["tools"]);
    assert_eq!(retried["max_tokens"], rejected["max_tokens"]);
    assert_eq!(retried["temperature"], 0.0);
    let rejected_messages = rejected["messages"].as_array().unwrap();
    let retried_messages = retried["messages"].as_array().unwrap();
    assert_eq!(retried_messages.len(), rejected_messages.len());
    assert_eq!(&retried_messages[1..], &rejected_messages[1..]);
    assert_eq!(retried_messages[0]["role"], "system");
    let original_policy = rejected_messages[0]["content"].as_str().unwrap();
    let repaired_policy = retried_messages[0]["content"].as_str().unwrap();
    assert!(repaired_policy.starts_with(original_policy));
    assert!(repaired_policy.len() > original_policy.len());
    assert!(!repaired_policy.contains("llama-server returned"));
    let search_message = retried_messages.last().unwrap();
    assert_eq!(search_message["role"], "tool");
    assert_eq!(search_message["tool_call_id"], "search-call");
    let search_result: serde_json::Value =
        serde_json::from_str(search_message["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        search_result["results"][0]["citation_id"],
        "passage:passage-1"
    );
    let final_messages = final_request["messages"].as_array().unwrap();
    assert_eq!(
        &final_messages[..retried_messages.len()],
        retried_messages.as_slice()
    );
    assert_eq!(final_messages.last().unwrap()["tool_call_id"], "read-call");
}

#[tokio::test]
pub(crate) async fn http_generic_server_error_is_not_retried_as_incomplete_tool_arguments() {
    let body = serde_json::json!({
        "error": {"message": "backend unavailable", "type": "api_error"}
    })
    .to_string();
    assert_tool_rejection_stops(context_test_question(), vec![body], "backend unavailable").await;
}

#[tokio::test]
pub(crate) async fn http_incomplete_tool_arguments_are_not_retried_without_an_offered_tool() {
    let mut no_books = context_test_question();
    no_books.allowed_book_ids.clear();
    no_books.book_titles.clear();
    assert_tool_rejection_stops(
        no_books,
        vec![incomplete_tool_arguments_error("read_passages")],
        "read_passages",
    )
    .await;
    assert_tool_rejection_stops(
        context_test_question(),
        vec![incomplete_tool_arguments_error("write_file")],
        "write_file",
    )
    .await;
}

#[tokio::test]
pub(crate) async fn http_repeated_incomplete_tool_arguments_stop_after_one_retry() {
    assert_tool_rejection_stops(
        context_test_question(),
        vec![
            incomplete_tool_arguments_error("read_passages"),
            incomplete_tool_arguments_error("read_passages"),
        ],
        "read_passages",
    )
    .await;
}

#[tokio::test]
pub(crate) async fn http_incomplete_tool_argument_retry_budget_is_shared_across_tool_rounds() {
    let (base_url, requests, server) = scripted_chat_http_server(vec![
        (
            "500 Internal Server Error",
            "application/json",
            incomplete_tool_arguments_error("search_books"),
        ),
        (
            "200 OK",
            "text/event-stream",
            tool_sse("search-call", "search_books", r#"{"query":"可信"}"#),
        ),
        (
            "500 Internal Server Error",
            "application/json",
            incomplete_tool_arguments_error("read_passages"),
        ),
    ]);
    let search = RecordingSearch::default();
    let runtime = context_test_runtime(base_url, &search);
    let error = runtime
        .answer(context_test_question(), None, AgentCancellation::default())
        .await
        .unwrap_err();
    server.join().unwrap();
    let error = format!("{error:#}");
    assert!(error.contains("read_passages"), "{error}");
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert_eq!(search.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
pub(crate) async fn http_streamed_incomplete_tool_arguments_reset_provisional_answer_without_retry()
{
    let provisional = serde_json::json!({
        "choices": [{"delta": {"content": "尚未核验的回答"}}]
    });
    let stream = format!(
        "data: {provisional}\n\n{}",
        tool_sse("read-call", "read_passages", r#"{"passage_ids":["#)
    );
    let (base_url, requests, server) = scripted_chat_server(vec![stream]);
    let search = RecordingSearch::default();
    let runtime = context_test_runtime(base_url, &search);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let error = runtime
        .answer(
            context_test_question(),
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap_err();
    server.join().unwrap();
    let error = format!("{error:#}");
    assert!(
        error.contains("tool arguments are not valid JSON"),
        "{error}"
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(search.requests.lock().unwrap().is_empty());
    let mut events = Vec::new();
    while let Some(event) = events_rx.recv().await {
        events.push(event);
    }
    assert!(events.contains(&AgentRunEvent::AnswerDelta("尚未核验的回答".to_string())));
    assert!(events.contains(&AgentRunEvent::AnswerReset));
    assert!(!events.contains(&AgentRunEvent::AnswerCommitted));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AgentRunEvent::ToolStarted { .. }))
    );
}
