//! Tests for the selection-explanation flow.

use super::*;

#[test]
fn explanation_question_names_the_displayed_translation_within_its_budget() {
    assert_eq!(
        selection_explanation_question(&explanation_reference(None)),
        SELECTION_EXPLANATION_QUESTION
    );
    assert_eq!(
        selection_explanation_question(&explanation_reference(Some(" \n\t "))),
        SELECTION_EXPLANATION_QUESTION
    );
    let translated = selection_explanation_question(&explanation_reference(Some("  译文段落  ")));
    assert!(translated.starts_with(SELECTION_TRANSLATION_QUESTION));
    assert!(translated.ends_with("译文段落"));
    // The sidebar's own question budget wins over the reader's larger
    // selection limit: an oversized译文 is truncated, never rejected.
    let oversized = "译".repeat(MAX_QUESTION_BYTES);
    let truncated = selection_explanation_question(&explanation_reference(Some(&oversized)));
    assert!(truncated.ends_with(SELECTION_TRANSLATION_QUESTION_TRUNCATED));
    assert!(truncated.len() <= MAX_QUESTION_BYTES);
    assert!(truncated.is_char_boundary(truncated.len()));
}

#[gpui::test]
fn explanation_events_publish_only_current_persisted_answers_and_report_recoverable_failures(
    cx: &mut TestAppContext,
) {
    fn begin(sidebar: &mut AiSidebar, cx: &mut Context<AiSidebar>) -> u64 {
        assert!(sidebar.begin_selection_explanation(cx));
        let mut reference = AiReferenceHint::chapter("book", "unit", 0, "Chapter");
        reference.frozen_text = Some("frozen selected words".to_string());
        sidebar.complete_selection_explanation(Vec::new(), reference, cx);
        let RequestState::Sending { request_id } = sidebar.conversation.request else {
            panic!("explanation should submit a new request")
        };
        request_id
    }

    let temp = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(temp.path()).unwrap());
    cx.update(gpui_component::init);
    let mut sidebar = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(book("book"), Vec::new()),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        sidebar = Some(view.clone());
        Root::new(view, window, cx)
    });
    let sidebar = sidebar.unwrap();
    let submission_order = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&submission_order);
    let _explanation_subscription = sidebar.update(visual, |_, cx| {
        cx.subscribe(&sidebar, move |_, _, event: &AiExplanationSubmitted, _| {
            recorded
                .borrow_mut()
                .push(("explanation", event.request_id));
        })
    });
    let recorded = Rc::clone(&submission_order);
    let _submission_subscription = sidebar.update(visual, |_, cx| {
        cx.subscribe(&sidebar, move |_, _, event: &AiSidebarEvent, _| {
            if let AiSidebarEvent::Submit(request) = event {
                recorded.borrow_mut().push(("submit", request.request_id));
            }
        })
    });
    let completed = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&completed);
    let _completion_subscription = sidebar.update(visual, |_, cx| {
        cx.subscribe(&sidebar, move |_, _, event: &AiAnswerCompleted, _| {
            recorded.borrow_mut().push(event.clone());
        })
    });
    let failed = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&failed);
    let _failure_subscription = sidebar.update(visual, |_, cx| {
        cx.subscribe(&sidebar, move |_, _, event: &AiExplanationFailed, _| {
            recorded.borrow_mut().push(event.clone());
        })
    });
    let failed_id = sidebar.update(visual, |sidebar, cx| {
        let request_id = begin(sidebar, cx);
        sidebar.append_answer_delta(request_id, "uncommitted provisional answer", cx);
        assert!(sidebar.fail_answer(request_id, "fixture persistence failure", cx));
        assert!(!sidebar.finish_persisted_answer(
            request_id,
            "late failed answer".into(),
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
            cx,
        ));
        request_id
    });
    let cancelled_id = sidebar.update(visual, |sidebar, cx| {
        let request_id = begin(sidebar, cx);
        sidebar.append_answer_delta(request_id, "cancelled provisional answer", cx);
        assert_eq!(sidebar.cancel_for_window_close(cx), Some(request_id));
        request_id
    });
    visual.run_until_parked();
    assert!(completed.borrow().is_empty());
    assert_eq!(failed.borrow()[0].request_id, Some(failed_id));
    assert_eq!(failed.borrow()[1].request_id, Some(cancelled_id));
    assert_eq!(failed.borrow().len(), 2);

    let successful_id = sidebar.update(visual, |sidebar, cx| {
        let request_id = begin(sidebar, cx);
        assert!(!sidebar.finish_persisted_answer(
            cancelled_id,
            "old generation".into(),
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
            cx,
        ));
        sidebar.append_answer_delta(request_id, "stream projection", cx);
        assert!(sidebar.finish_persisted_answer(
            request_id,
            "## Final persisted Markdown\n\n**final answer**".into(),
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
            cx,
        ));
        assert!(!sidebar.finish_persisted_answer(
            request_id,
            "duplicate result".into(),
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
            cx,
        ));
        request_id
    });
    visual.run_until_parked();
    assert_eq!(
        *completed.borrow(),
        vec![AiAnswerCompleted {
            request_id: successful_id,
            markdown: "## Final persisted Markdown\n\n**final answer**".into(),
        }]
    );
    assert_eq!(failed.borrow().len(), 2);

    sidebar.update(visual, |sidebar, cx| {
        sidebar.fail_session_operation("ordinary session failure", cx);
        let ordinary = sidebar.conversation.begin("ordinary question").unwrap();
        assert!(sidebar.fail_answer(ordinary.request_id, "ordinary answer failure", cx));
    });
    visual.run_until_parked();
    assert_eq!(
        failed.borrow().len(),
        2,
        "ordinary chat is not an explanation"
    );
    sidebar.update(visual, |sidebar, cx| {
        sidebar.fail_selection_explanation_session("fresh session failed", cx);
        let reference = AiReferenceHint::chapter("book", "unit", 0, "missing selection");
        sidebar.complete_selection_explanation(Vec::new(), reference, cx);
    });
    visual.run_until_parked();
    assert_eq!(failed.borrow().len(), 4);
    assert_eq!(failed.borrow()[2].request_id, None);
    assert_eq!(failed.borrow()[3].request_id, None);
    assert_eq!(completed.borrow().len(), 1);
    assert_eq!(
        *submission_order.borrow(),
        [failed_id, cancelled_id, successful_id]
            .into_iter()
            .flat_map(|id| [("explanation", id), ("submit", id)])
            .collect::<Vec<_>>(),
        "the exact explanation generation is bound before submission"
    );
}
