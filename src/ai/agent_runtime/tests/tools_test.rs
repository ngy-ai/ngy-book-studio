//! Tests for tool rounds, budgets and empty scopes.

pub(crate) use super::super::*;
pub(crate) use super::*;

#[tokio::test]
pub(crate) async fn returns_only_sources_explicitly_cited_by_the_answer() {
    let runtime = runtime(search_then_answer(
        "Grounded answer. [[ngy-source:passage:passage-1]]",
    ));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let answer = runtime
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
        .await
        .unwrap();
    assert_eq!(answer.markdown, "Grounded answer. ");
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
    );
    assert_eq!(answer.citations.len(), 1);
    assert_eq!(answer.citations[0].citation_id, "passage:passage-1");
    assert_eq!(answer.citations[0].book_id, "book-1");
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
        Some(AgentRunEvent::AnswerDelta("Grounded answer. ".into()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerCommitted));
}

#[tokio::test]
pub(crate) async fn streams_multiple_chunks_and_hides_split_protocol_markers() {
    let snapshot = authorized_snapshot();
    let provider = SelectionAliasProvider::new(true);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let answer = runtime(provider)
        .answer(
            AgentQuestion {
                question: "Use my selection".into(),
                allowed_book_ids: vec!["book-1".into()],
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: vec![snapshot.clone(), snapshot],
            },
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(answer.markdown, "first second  done");
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
    );

    let mut streamed = String::new();
    let mut committed = false;
    while let Some(event) = events_rx.recv().await {
        match event {
            AgentRunEvent::AnswerDelta(delta) => {
                assert!(!delta.contains("[[ngy-"));
                streamed.push_str(&delta);
            }
            AgentRunEvent::AnswerCommitted => committed = true,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(streamed, answer.markdown);
    assert!(committed);
}

#[tokio::test]
pub(crate) async fn split_no_source_marker_never_reaches_answer_deltas() {
    let provider = answer_chunks(&["No supporting ", "source. [[ngy-no-", "source]]"]);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let answer = runtime(provider)
        .answer(
            AgentQuestion {
                question: "Unknown?".into(),
                allowed_book_ids: vec!["book-1".into()],
                book_titles: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
            },
            Some(events_tx),
            AgentCancellation::default(),
        )
        .await
        .unwrap();
    let mut streamed = String::new();
    let mut committed = false;
    while let Some(event) = events_rx.recv().await {
        match event {
            AgentRunEvent::AnswerDelta(delta) => streamed.push_str(&delta),
            AgentRunEvent::AnswerCommitted => committed = true,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(streamed, "No supporting source. ");
    assert_eq!(streamed, answer.markdown);
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    assert!(!streamed.contains("ngy-no-source"));
    assert!(committed);
}

#[tokio::test]
pub(crate) async fn text_is_reset_when_the_same_turn_later_requests_a_tool() {
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let answer = runtime(tool_after_content_then_answer())
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
        .await
        .unwrap();
    assert_eq!(answer.markdown, "final answer. ");
    assert_eq!(
        events_rx.recv().await,
        Some(AgentRunEvent::AnswerDelta("temporary planning".into()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerReset));
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
        Some(AgentRunEvent::AnswerDelta("final answer. ".into()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerCommitted));
}

#[tokio::test]
pub(crate) async fn cancellation_stops_an_incremental_answer_without_committing_it() {
    let runtime = AgentRuntime::new(
        Arc::new(DelayedProvider),
        Arc::new(MockSearch),
        Arc::new(MockBooks),
        "chat-model",
        AgentLimits::default(),
    )
    .unwrap();
    let cancellation = AgentCancellation::default();
    let cancel_request = cancellation.clone();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        runtime
            .answer(
                AgentQuestion {
                    question: "Wait".into(),
                    allowed_book_ids: vec!["book-1".into()],
                    book_titles: Vec::new(),
                    history: Vec::new(),
                    snapshots: Vec::new(),
                },
                Some(events_tx),
                cancellation,
            )
            .await
    });
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
            .await
            .unwrap(),
        Some(AgentRunEvent::AnswerDelta("partial answer".into()))
    );
    cancel_request.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    let mut reset = false;
    while let Some(event) = events_rx.recv().await {
        assert_ne!(event, AgentRunEvent::AnswerCommitted);
        reset |= event == AgentRunEvent::AnswerReset;
    }
    assert!(reset, "cancelled provisional output must be reset");
}

#[test]
pub(crate) fn incremental_projection_releases_invalid_prefixes_without_splitting_utf8() {
    let mut projection = StreamingAnswerProjection::default();
    assert_eq!(projection.push("中文 ["), "中文 ");
    assert_eq!(
        projection.push("[ngy-unknown]] tail"),
        "[[ngy-unknown]] tail"
    );
    assert_eq!(projection.finish("中文 [[ngy-unknown]] tail").unwrap(), "");
}

#[tokio::test]
pub(crate) async fn substantive_answer_without_markers_is_an_unverified_streamed_answer() {
    let runtime = runtime(search_then_answer("Uncited answer."));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let answer = runtime
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
        .await
        .unwrap();
    assert_eq!(answer.markdown, "Uncited answer.");
    assert!(answer.citations.is_empty());
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
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
        Some(AgentRunEvent::AnswerDelta("Uncited answer.".into()))
    );
    assert_eq!(events_rx.recv().await, Some(AgentRunEvent::AnswerCommitted));
}
