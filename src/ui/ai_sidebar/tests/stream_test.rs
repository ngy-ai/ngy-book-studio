//! Tests for the answer stream and cancellation.

use super::*;

#[test]
fn cancellation_rejects_late_stream_callbacks() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let request = state.begin("question").unwrap();
    assert!(state.append_delta(request.request_id, "partial"));
    assert_eq!(state.cancel(), Some(request.request_id));
    assert!(!state.messages.iter().any(|message| {
        message.request_id == Some(request.request_id) && message.role == AiMessageRole::Assistant
    }));
    assert!(!state.append_delta(request.request_id, "late"));
    assert!(!state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));
    assert!(!state.fail(request.request_id, "late failure"));
    assert!(!state.is_sending());
}

#[test]
fn provisional_answer_can_be_reset_before_a_later_final_stream() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let request = state.begin("question").unwrap();
    assert!(state.append_delta(request.request_id, "temporary tool planning"));
    assert!(state.reset_answer(request.request_id));
    assert!(!state.reset_answer(request.request_id));
    assert!(state.append_delta(request.request_id, "final "));
    assert!(state.append_delta(request.request_id, "answer"));
    assert!(state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));
    let answer = state
        .messages
        .iter()
        .find(|message| message.role == AiMessageRole::Assistant)
        .unwrap();
    assert_eq!(answer.content, "final answer");
}

#[test]
fn failed_stream_discards_provisional_answer_before_showing_error() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let request = state.begin("question").unwrap();
    assert!(state.append_delta(request.request_id, "unvalidated partial"));
    assert!(state.fail(request.request_id, "invalid citation"));
    assert!(!state.messages.iter().any(|message| {
        message.request_id == Some(request.request_id) && message.role == AiMessageRole::Assistant
    }));
    assert_eq!(state.messages.last().unwrap().role, AiMessageRole::Error);
    assert_eq!(state.messages.last().unwrap().content, "invalid citation");
}

#[test]
fn only_current_request_can_receive_sources() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let request = state.begin("question").unwrap();
    assert!(state.append_delta(request.request_id, "answer"));
    let source = AiSourceLink {
        citation_id: "source-1".to_string(),
        book_id: "a".to_string(),
        unit_id: "unit-1".to_string(),
        unit_index: Some(0),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        locator: None,
        label: "Chapter 1".to_string(),
        quote: Some("evidence".to_string()),
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    let foreign = AiSourceLink {
        citation_id: "source-foreign".to_string(),
        book_id: "secret".to_string(),
        ..source.clone()
    };
    assert!(state.finish(
        request.request_id,
        vec![source.clone(), foreign],
        AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources,
    ));
    assert_eq!(state.messages.last().unwrap().sources, vec![source]);
    assert!(!state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));
}

#[test]
fn source_versions_are_checked_before_navigation_and_stale_links_remain_identifiable() {
    let mut document = BookDocument::created("book", "Book");
    document.revision = Revision::new(3);
    let mut unit = ContentUnit::empty("unit-1", ContentUnitKind::Chapter, "Chapter");
    unit.revision = Revision::new(7);
    document.units.push(unit);
    let mut source = AiSourceLink {
        citation_id: "source-1".to_string(),
        book_id: "book".to_string(),
        unit_id: "unit-1".to_string(),
        unit_index: Some(0),
        document_revision: Revision::new(3),
        unit_revision: Revision::new(7),
        locator: Some(DocumentLocator::unit("book", "unit-1")),
        label: "Chapter".to_string(),
        quote: None,
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    assert_eq!(source.current_unit_index(&document), Some(0));

    document.units[0].revision = Revision::new(8);
    source.mark_current_status(Some(&document));
    assert!(source.stale);
    assert_eq!(source.current_unit_index(&document), None);
    assert!(source.validated_locator().is_some());
}
