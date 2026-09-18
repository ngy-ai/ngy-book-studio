//! Tests for streaming, projection and cancellation.

pub(crate) use super::super::*;
pub(crate) use super::*;

#[tokio::test]
pub(crate) async fn tool_rounds_end_with_one_tool_free_final_request() {
    let provider = MockProvider {
        turns: Arc::new(Mutex::new(VecDeque::from([
            search_tool_turn("call-1", "first"),
            search_tool_turn("call-2", "second"),
            answer_turn("General answer."),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let requests = Arc::clone(&provider.requests);
    let limits = AgentLimits {
        max_tool_rounds: 2,
        ..AgentLimits::default()
    };
    let runtime = AgentRuntime::new(
        Arc::new(provider),
        Arc::new(MockSearch),
        Arc::new(MockBooks),
        "chat-model",
        limits,
    )
    .unwrap()
    .with_chat_generation(custom_generation())
    .unwrap();

    let answer = ask(&runtime).await.unwrap();

    assert_eq!(answer.markdown, "General answer.");
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let expected_tools = agent_tool_definitions();
    assert_eq!(requests[0].tools, expected_tools);
    assert_eq!(requests[1].tools, agent_tool_definitions());
    assert!(requests[2].tools.is_empty());
    assert!(matches!(
        requests[2].messages[0].content.as_ref(),
        Some(MessageContent::Text(policy)) if policy.ends_with(TOOL_BUDGET_FINAL_GUIDANCE)
    ));
    for request in requests.iter() {
        assert_custom_generation(request);
    }
}

#[tokio::test]
pub(crate) async fn exhausted_batch_keeps_only_executed_sources_and_validates_the_final_answer() {
    for (marker, succeeds) in [
        ("passage:passage-1", true),
        ("passage:never-executed", false),
    ] {
        let mut batch = search_tool_turn("call-1", "first");
        batch[0].tool_call_deltas.push(ToolCallDelta {
            index: 1,
            id: Some("call-2".into()),
            name: Some("search_books".into()),
            arguments_delta: Some("{\"query\":\"not executed\"}".into()),
        });
        let provider = MockProvider {
            turns: Arc::new(Mutex::new(VecDeque::from([
                batch,
                answer_turn(&format!("From evidence. [[ngy-source:{marker}]]")),
            ]))),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let requests = Arc::clone(&provider.requests);
        let runtime = AgentRuntime::new(
            Arc::new(provider),
            Arc::new(MockSearch),
            Arc::new(MockBooks),
            "chat-model",
            AgentLimits {
                max_tool_rounds: 1,
                ..AgentLimits::default()
            },
        )
        .unwrap();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let result = runtime
            .answer(
                AgentQuestion {
                    question: "What is verified?".into(),
                    allowed_book_ids: vec!["book-1".into()],
                    book_titles: Vec::new(),
                    history: Vec::new(),
                    snapshots: Vec::new(),
                },
                Some(events_tx),
                AgentCancellation::default(),
            )
            .await;
        if succeeds {
            let answer = result.unwrap();
            assert_eq!(answer.markdown, "From evidence. ");
            assert_eq!(answer.citations.len(), 1);
        } else {
            assert!(result.is_err(), "an unserved source must remain invalid");
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
        assert_eq!(events.contains(&AgentRunEvent::AnswerCommitted), succeeds);
        if !succeeds {
            assert_eq!(events.last(), Some(&AgentRunEvent::AnswerReset));
        }
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].tools.is_empty());
        let tool_messages = requests[1]
            .messages
            .iter()
            .filter(|message| message.role == ChatRole::Tool)
            .collect::<Vec<_>>();
        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(tool_messages[1].tool_call_id.as_deref(), Some("call-2"));
        assert_eq!(
            tool_messages[1].content,
            Some(TOOL_BUDGET_EXHAUSTED_RESULT.into())
        );
    }
}

#[tokio::test]
pub(crate) async fn empty_scope_is_tool_free_from_the_first_request() {
    let provider = answer_only("General answer.");
    let requests = Arc::clone(&provider.requests);
    let runtime = runtime(provider);

    let answer = runtime
        .answer(
            AgentQuestion {
                question: "What time is it?".into(),
                allowed_book_ids: Vec::new(),
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
            },
            None,
            AgentCancellation::default(),
        )
        .await
        .unwrap();

    assert_eq!(answer.markdown, "General answer.");
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].reasoning_effort, Some(ReasoningEffort::None));
    assert!(matches!(
        requests[0].messages.first().and_then(|message| message.content.as_ref()),
        Some(MessageContent::Text(policy))
            if policy.contains("The host authorized no books")
                && policy.contains("do not output any [[ngy-source:...]] marker")
    ));
}

#[tokio::test]
pub(crate) async fn tool_call_without_offered_tools_is_rejected_without_execution() {
    let provider = MockProvider {
        turns: Arc::new(Mutex::new(VecDeque::from([
            search_tool_turn("call-1", "first"),
            search_tool_turn("call-2", "second"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let requests = Arc::clone(&provider.requests);
    let search = CountingSearch::default();
    let search_calls = Arc::clone(&search.calls);
    let limits = AgentLimits {
        max_tool_rounds: 1,
        ..AgentLimits::default()
    };
    let runtime = AgentRuntime::new(
        Arc::new(provider),
        Arc::new(search),
        Arc::new(MockBooks),
        "chat-model",
        limits,
    )
    .unwrap();

    let error = ask(&runtime).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("tool calls when no tools were offered")
    );
    assert_eq!(search_calls.load(Ordering::SeqCst), 1);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, agent_tool_definitions());
    assert!(requests[1].tools.is_empty());
}

#[test]
pub(crate) fn every_protocol_marker_split_point_is_withheld() {
    for (raw, expected) in [
        ("before [[ngy-source:passage:p-1]] after", "before  after"),
        ("before [[ngy-no-source]] after", "before  after"),
    ] {
        for split in 0..=raw.len() {
            let mut projection = StreamingAnswerProjection::default();
            let mut visible = projection.push(&raw[..split]);
            visible.push_str(&projection.push(&raw[split..]));
            visible.push_str(&projection.finish(expected).unwrap());
            assert_eq!(visible, expected, "split at byte {split} for {raw:?}");
            assert!(!visible.contains("[[ngy-"));
        }
    }
}

#[tokio::test]
pub(crate) async fn explicit_no_source_answer_is_accepted_without_citations() {
    let answer = ask(&runtime(answer_only(
        "I could not find a supporting source. [[ngy-no-source]]",
    )))
    .await
    .unwrap();
    assert_eq!(answer.markdown, "I could not find a supporting source. ");
    assert!(answer.citations.is_empty());
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
}

#[tokio::test]
pub(crate) async fn marker_only_answer_is_still_rejected_as_empty() {
    let error = ask(&runtime(answer_only(NO_SOURCE_MARKER)))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("empty answer after removing source markers")
    );
}
