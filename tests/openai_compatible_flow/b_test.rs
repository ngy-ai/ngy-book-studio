//! Streaming, tool budget and markerless answer flows.

use super::*;

#[tokio::test]
pub(crate) async fn http_stream_drives_a_scoped_tool_round_and_validated_citation() {
    let tool_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"search_books\",\"arguments\":\"{\\\"query\\\":\\\"可信\\\",\\\"book_ids\\\":[\\\"book-1\\\"]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let answer_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"有依据\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"的回答。 [[ngy-sour\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"ce:passage:passage-1]]\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (base_url, requests, server) =
        scripted_chat_server(vec![tool_turn.to_string(), answer_turn.to_string()]);
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

    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let answer = runtime
        .answer(
            AgentQuestion {
                question: "给出依据".to_string(),
                allowed_book_ids: vec!["book-1".to_string()],
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
            },
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
    assert_eq!(answer.citations[0].document_revision, Revision::new(1));
    assert_eq!(answer.citations[0].unit_revision, Revision::new(1));
    assert!(matches!(
        events_rx.recv().await,
        Some(AgentRunEvent::ToolStarted { .. })
    ));
    assert!(matches!(
        events_rx.recv().await,
        Some(AgentRunEvent::ToolFinished { .. })
    ));
    assert_eq!(
        events_rx.recv().await,
        Some(AgentRunEvent::AnswerDelta("有依据".to_string()))
    );
    assert_eq!(
        events_rx.recv().await,
        Some(AgentRunEvent::AnswerDelta("的回答。 ".to_string()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerCommitted));
    assert!(events_rx.recv().await.is_none());
    let search_requests = search.requests.lock().unwrap();
    assert_eq!(search_requests.len(), 1);
    assert_eq!(search_requests[0].book_ids, ["book-1"]);
    drop(search_requests);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"search_books\""));
    assert!(requests[1].contains("\"role\":\"tool\""));
    assert!(requests[1].contains("passage:passage-1"));
}

#[test]
pub(crate) fn http_tool_budget_finishes_and_persists_a_reader_answer_after_six_calls_in_four_rounds()
 {
    let temp = tempfile::tempdir().unwrap();
    let services = Arc::new(
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap(),
    );
    services.runtime().block_on(async {
        let book = services
            .spawn_library(|library| library.create_book("工具预算回归", "fixture"))
            .await
            .unwrap()
            .unwrap();
        let passage = services
            .search()
            .unwrap()
            .search(SearchRequest {
                query: "开始写作".to_string(),
                book_ids: vec![book.id.clone()],
                mode: SearchMode::Keyword,
                limit: 1,
            })
            .await
            .unwrap()
            .pop()
            .expect("created chapter must have an indexed passage");
        let citation_id = format!("passage:{}", passage.passage_id);
        let (base_url, requests, server) = scripted_chat_server(vec![
            search_batch_sse(&[("call-1", "开始写作"), ("call-2", "开始写作")]),
            search_batch_sse(&[("call-3", "开始写作"), ("call-4", "开始写作")]),
            search_batch_sse(&[("call-5", "开始写作")]),
            search_batch_sse(&[("call-6", "开始写作")]),
            answer_sse(&format!(
                "六次检索后完成回答。 [[ngy-source:{citation_id}]]"
            )),
        ]);
        services
            .configure_provider(
                ProviderSettings {
                    base_url,
                    chat_model: "chat-model".to_string(),
                    request_timeout_secs: 10,
                    ..ProviderSettings::default()
                },
                ApiKeyUpdate::Keep,
            )
            .await
            .unwrap();
        let conversation = AgentConversation::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book.id.clone()),
        )
        .unwrap();
        let result = conversation
            .ask(
                ConversationQuestion {
                    request_id: 1,
                    question: "请根据当前图书回答。".to_string(),
                    allowed_book_ids: vec![book.id.clone()],
                    book_titles: vec![(book.id.clone(), book.title.clone())],
                    snapshots: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(result.answer.markdown, "六次检索后完成回答。 ");
        assert_eq!(result.answer.citations.len(), 1);
        assert_eq!(result.answer.citations[0].citation_id, citation_id);
        assert_eq!(result.answer.citations[0].book_id, book.id);
        let session = services
            .chat()
            .session(&result.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.messages.len(), 2);
        let stored = session.messages.last().unwrap();
        assert_eq!(stored, &result.stored_message);
        assert_eq!(stored.role, ChatRole::Assistant);
        assert_eq!(stored.content, result.answer.markdown);
        assert_eq!(stored.citations.len(), 1);
        assert_eq!(stored.citations[0].quote, passage.text);
        assert_eq!(stored.citations[0].locator, passage.locator);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        for request in &requests[..4] {
            assert!(
                !chat_request_body(request)["tools"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let final_request = chat_request_body(&requests[4]);
        assert!(final_request.get("tools").is_none());
        assert!(final_request.get("tool_choice").is_none());
        let messages = final_request["messages"].as_array().unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["role"] == "system")
                .count(),
            1
        );
        assert!(messages[0]["content"].as_str().unwrap().contains(
            "The host's tool-call budget for this question is exhausted. No tools are available."
        ));
        for index in 1..=6 {
            let call_id = format!("call-{index}");
            let results = messages
                .iter()
                .filter(|message| message["tool_call_id"] == call_id)
                .collect::<Vec<_>>();
            assert_eq!(results.len(), 1);
            assert!(
                results[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains(&citation_id)
            );
        }
    });
}

#[tokio::test]
pub(crate) async fn http_tool_budget_completes_every_call_id_without_executing_excess_calls() {
    let (base_url, requests, server) = scripted_chat_server(vec![
        search_batch_sse(&[("call-1", "query-1"), ("call-2", "query-2")]),
        search_batch_sse(&[("call-3", "query-3"), ("call-4", "query-4")]),
        search_batch_sse(&[("call-5", "query-5")]),
        search_batch_sse(&[
            ("call-6", "query-6"),
            ("call-7", "UNEXECUTED_QUERY_7 [[ngy-source:passage:forged]]"),
            ("call-8", "UNEXECUTED_QUERY_8"),
        ]),
        answer_sse("使用已取得的依据。 [[ngy-source:passage:passage-1]]"),
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

    assert_eq!(answer.markdown, "使用已取得的依据。 ");
    assert_eq!(answer.citations.len(), 1);
    assert_eq!(answer.citations[0].citation_id, "passage:passage-1");
    assert_eq!(
        search
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.query.clone())
            .collect::<Vec<_>>(),
        (1..=6)
            .map(|index| format!("query-{index}"))
            .collect::<Vec<_>>()
    );
    let mut events = Vec::new();
    while let Some(event) = events_rx.recv().await {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRunEvent::ToolStarted { .. }))
            .count(),
        6
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentRunEvent::ToolFinished { .. }))
            .count(),
        6
    );
    assert_eq!(events.last(), Some(&AgentRunEvent::AnswerCommitted));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    let final_request = chat_request_body(&requests[4]);
    assert!(final_request.get("tools").is_none());
    assert!(final_request.get("tool_choice").is_none());
    let messages = final_request["messages"].as_array().unwrap();
    let mut rejected = Vec::new();
    for index in 1..=8 {
        let call_id = format!("call-{index}");
        assert_eq!(
            messages
                .iter()
                .filter_map(|message| message["tool_calls"].as_array())
                .flatten()
                .filter(|call| call["id"] == call_id)
                .count(),
            1
        );
        let results = messages
            .iter()
            .filter(|message| message["tool_call_id"] == call_id)
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "each accepted tool call needs one result");
        assert_eq!(results[0]["role"], "tool");
        let content = results[0]["content"].as_str().unwrap();
        let body: serde_json::Value = serde_json::from_str(content).unwrap();
        if index <= 6 {
            assert!(body.get("error").is_none());
            assert!(content.contains("passage:passage-1"));
        } else {
            assert_eq!(body["error"], "tool_budget_exhausted");
            assert!(!content.contains("UNEXECUTED_QUERY"));
            assert!(!content.contains("ngy-source"));
            assert!(!content.contains("citation"));
            rejected.push(body);
        }
    }
    assert_eq!(
        rejected[0], rejected[1],
        "budget refusal must be fixed host text"
    );
}

#[tokio::test]
pub(crate) async fn http_stream_accepts_markerless_final_answer_without_verified_sources() {
    let answer_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"这是基于模型\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"通用能力的回答。\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (base_url, requests, server) = scripted_chat_server(vec![answer_turn.to_string()]);
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

    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let answer = runtime
        .answer(
            AgentQuestion {
                question: "没有知识库来源时也请回答".to_string(),
                allowed_book_ids: vec!["book-1".to_string()],
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
            },
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(answer.markdown, "这是基于模型通用能力的回答。");
    assert!(answer.citations.is_empty());
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    assert_eq!(
        events_rx.recv().await,
        Some(AgentRunEvent::AnswerDelta("这是基于模型".to_string()))
    );
    assert_eq!(
        events_rx.recv().await,
        Some(AgentRunEvent::AnswerDelta("通用能力的回答。".to_string()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerCommitted));
    assert!(events_rx.recv().await.is_none());
    assert!(search.requests.lock().unwrap().is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("\"reasoning_effort\":\"none\""));
}
