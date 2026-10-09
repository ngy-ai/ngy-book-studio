//! Tests for request scoping, references and scope refreshes.

use super::*;

#[test]
fn requests_capture_exact_scope_and_independently_selected_references() {
    let mut state = ConversationState::new(AiSidebarScope::book(
        book("current"),
        vec![book("current"), book("other")],
    ));
    assert!(state.scope.add_book("other"));
    let first = AiReferenceHint::chapter("current", "unit-1", 0, "Chapter 1");
    let second = AiReferenceHint::chapter("other", "unit-2", 1, "Chapter 2");
    state.set_reference_hints(vec![first.clone(), second.clone()]);
    assert!(state.toggle_reference(&first));
    assert!(state.toggle_reference(&second));
    assert!(state.toggle_reference(&first));
    assert!(state.toggle_reference(&first));
    let request = state.begin("  What happened?  ").unwrap();
    assert_eq!(request.question, "What happened?");
    assert_eq!(request.book_ids, vec!["current", "other"]);
    assert_eq!(
        request
            .reference_hints
            .iter()
            .map(|reference| reference.unit_id.as_str())
            .collect::<Vec<_>>(),
        vec!["unit-1", "unit-2"]
    );
    assert_eq!(request.reference.unwrap().unit_id, "unit-1");
    assert!(state.begin("second request").is_none());
}

#[test]
fn active_frozen_reference_enters_the_request_without_a_manual_toggle() {
    let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
    let mut highlighted = AiReferenceHint::chapter("current", "unit-1", 0, "Current highlight");
    highlighted.frozen_text = Some("exact selected words".to_string());
    highlighted.revision = Some(7);

    state.set_reference_hints(vec![highlighted.clone()]);

    assert_eq!(state.selected_reference_count(), 1);
    assert!(state.reference_is_included(&highlighted));
    let request = state.begin("Explain this").expect("valid request");
    assert_eq!(request.reference_hints, vec![highlighted.clone()]);
    assert_eq!(request.reference, Some(highlighted));
}

#[test]
fn selection_explanation_replaces_previous_history_and_reference_choices() {
    let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
    let mut selected = AiReferenceHint::chapter("current", "unit-1", 0, "Selected text");
    selected.frozen_text = Some("exact selected words\n第二行".to_string());
    assert_eq!(
        selected.revision, None,
        "Reader freezes text without an editor revision"
    );
    let manual = AiReferenceHint::chapter("current", "unit-2", 1, "Manual chapter");
    state.set_reference_hints(vec![selected.clone(), manual.clone()]);
    assert!(state.toggle_reference(&selected));
    assert!(state.toggle_reference(&manual));
    let previous = state.begin("Previous question").unwrap();
    assert!(state.append_delta(previous.request_id, "Previous answer"));
    assert!(state.finish(
        previous.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));

    let request = state.begin_selection_explanation(selected.clone()).unwrap();

    assert_eq!(request.question, "用汉语详细解释一下");
    assert_eq!(request.reference_hints, vec![selected.clone()]);
    assert_eq!(request.reference, Some(selected.clone()));
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].content, "用汉语详细解释一下");
    assert!(request.request_id > previous.request_id);
    assert!(state.included_references.is_empty());
    assert!(state.reference_is_included(&selected));

    // Selection changes while the answer is running only affect a later
    // question; they must not rewrite the already emitted request.
    let mut changed = selected.clone();
    changed.frozen_text = Some("new live selection".to_string());
    state.set_reference_hints(vec![changed, manual]);
    assert_eq!(request.reference_hints, vec![selected]);
}

#[test]
fn selection_explanation_rejects_busy_or_removed_scope_without_mutating_conversation() {
    let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
    let mut selected = AiReferenceHint::chapter("current", "unit-1", 0, "Selected text");
    selected.frozen_text = Some("selected words".to_string());
    state.set_reference_hints(vec![selected.clone()]);
    let request = state.begin("Keep this request").unwrap();
    assert!(state.append_delta(request.request_id, "Still answering"));
    let messages = state.messages.clone();
    assert!(
        state
            .begin_selection_explanation(selected.clone())
            .is_none()
    );
    assert!(state.is_current(request.request_id));
    assert_eq!(state.messages, messages);
    assert_eq!(state.reference_hints, vec![selected.clone()]);

    assert!(state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));
    let completed = state.messages.clone();
    selected.book_id = "removed-book".to_string();
    assert!(state.begin_selection_explanation(selected).is_none());
    assert_eq!(state.messages, completed);
}

#[test]
fn automatic_highlight_can_be_excluded_and_cleared_without_losing_manual_references() {
    let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
    let mut highlighted = AiReferenceHint::chapter("current", "unit-1", 0, "Current highlight");
    highlighted.frozen_text = Some("exact selected words".to_string());
    highlighted.revision = Some(7);
    let manual = AiReferenceHint::chapter("current", "unit-2", 1, "Other chapter");
    state.set_reference_hints(vec![highlighted.clone(), manual.clone()]);
    assert!(state.toggle_reference(&manual));

    assert!(state.toggle_reference(&highlighted));
    assert!(!state.reference_is_included(&highlighted));
    assert_eq!(state.selected_reference_count(), 1);

    state.set_reference_hints(vec![highlighted.clone(), manual.clone()]);
    assert!(
        !state.reference_is_included(&highlighted),
        "a redundant host refresh must preserve the user's exclusion"
    );
    assert!(state.toggle_reference(&highlighted));
    assert_eq!(state.selected_reference_count(), 2);

    let mut cleared = highlighted.clone();
    cleared.frozen_text = None;
    state.set_reference_hints(vec![cleared.clone(), manual.clone()]);
    assert!(state.automatic_reference.is_none());
    assert!(!state.reference_is_included(&cleared));
    assert!(state.reference_is_included(&manual));
    assert_eq!(state.selected_reference_count(), 1);
}

#[test]
fn effective_reference_count_deduplicates_manual_and_automatic_keys_and_enforces_the_limit() {
    let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
    let plain = AiReferenceHint::chapter("current", "unit-0", 0, "Current chapter");
    state.set_reference_hints(vec![plain.clone()]);
    assert!(state.toggle_reference(&plain));

    let mut highlighted = plain.clone();
    highlighted.label = "Current highlight".to_string();
    highlighted.frozen_text = Some("selected".to_string());
    let mut references = vec![highlighted.clone()];
    references.extend((1..=MAX_SELECTED_REFERENCES).map(|index| {
        AiReferenceHint::chapter(
            "current",
            format!("unit-{index}"),
            index,
            format!("Chapter {index}"),
        )
    }));
    state.set_reference_hints(references.clone());

    assert_eq!(state.selected_reference_count(), 1);
    for reference in references.iter().skip(1).take(MAX_SELECTED_REFERENCES - 1) {
        assert!(state.toggle_reference(reference));
    }
    assert_eq!(state.selected_reference_count(), MAX_SELECTED_REFERENCES);
    assert!(!state.toggle_reference(references.last().unwrap()));

    let request = state.begin("Compare").expect("valid request");
    assert_eq!(request.reference_hints.len(), MAX_SELECTED_REFERENCES);
    assert_eq!(
        request
            .reference_hints
            .iter()
            .filter(|reference| reference.unit_id == "unit-0")
            .count(),
        1
    );
}

#[test]
fn page_locators_distinguish_references_within_one_content_unit() {
    let mut state = ConversationState::new(AiSidebarScope::book(book("book"), Vec::new()));
    let mut first = AiReferenceHint::chapter("book", "unit-1", 0, "Page 1");
    first.locator = Some(
        DocumentLocator::unit("book", "unit-1").with_source(SourceLocator::office_rendered_page(1)),
    );
    let mut second = first.clone();
    second.label = "Page 2".to_string();
    second.locator = Some(
        DocumentLocator::unit("book", "unit-1").with_source(SourceLocator::office_rendered_page(2)),
    );

    state.set_reference_hints(vec![first.clone(), second.clone()]);
    assert_eq!(state.reference_hints.len(), 2);
    assert!(state.toggle_reference(&first));
    assert!(state.toggle_reference(&second));
    let request = state.begin("compare pages").expect("valid request");
    assert_eq!(request.reference_hints.len(), 2);
    assert_eq!(request.reference_hints[0].locator, first.locator);
    assert_eq!(request.reference_hints[1].locator, second.locator);
}

#[test]
fn scope_refresh_preserves_only_still_authorized_additions() {
    let mut state = ConversationState::new(AiSidebarScope::book(
        book("current"),
        vec![book("other"), book("removed")],
    ));
    assert!(state.scope.add_book("other"));
    assert!(state.scope.add_book("removed"));

    assert!(state.set_scope(AiSidebarScope::book(
        AiBookOption::new("current", "Renamed current"),
        vec![book("other"), book("new")],
    )));
    assert_eq!(state.scope.book_ids(), vec!["current", "other"]);
    assert_eq!(state.scope.label(), "Renamed current 等 2 本");
    assert!(!state.scope.add_book("removed"));
    assert!(state.scope.add_book("new"));
    assert!(!state.set_scope(AiSidebarScope::book(
        AiBookOption::new("current", "Renamed current"),
        vec![book("other"), book("new")],
    )));
}

#[test]
fn scope_change_drops_only_unauthorized_reference_selections() {
    let mut state =
        ConversationState::new(AiSidebarScope::library("All", vec![book("a"), book("b")]));
    let a = AiReferenceHint::chapter("a", "unit-a", 0, "A");
    let b = AiReferenceHint::chapter("b", "unit-b", 0, "B");
    state.set_reference_hints(vec![a.clone(), b.clone()]);
    assert!(state.toggle_reference(&a));
    assert!(state.toggle_reference(&b));
    state.messages.push(AiMessage {
        id: 99,
        role: AiMessageRole::Assistant,
        content: "answer".to_string(),
        sources: vec![
            AiSourceLink {
                citation_id: "citation-a".to_string(),
                book_id: "a".to_string(),
                unit_id: "unit-a".to_string(),
                unit_index: None,
                document_revision: Revision::new(1),
                unit_revision: Revision::new(1),
                locator: Some(DocumentLocator::unit("a", "unit-a")),
                label: "A".to_string(),
                quote: None,
                selection_snapshot: false,
                stale: false,
                url: None,
            },
            AiSourceLink {
                citation_id: "citation-b".to_string(),
                book_id: "b".to_string(),
                unit_id: "unit-b".to_string(),
                unit_index: None,
                document_revision: Revision::new(1),
                unit_revision: Revision::new(1),
                locator: Some(DocumentLocator::unit("b", "unit-b")),
                label: "B".to_string(),
                quote: None,
                selection_snapshot: false,
                stale: false,
                url: None,
            },
        ],
        source_status: Some(AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources),
        request_id: None,
        db_id: None,
    });

    state.set_scope(AiSidebarScope::library("Only B", vec![book("b")]));

    assert_eq!(state.reference_hints, vec![b.clone()]);
    assert_eq!(state.included_references.len(), 1);
    assert!(state.included_references.contains(&reference_key(&b)));
    assert_eq!(state.messages[0].sources.len(), 1);
    assert_eq!(state.messages[0].sources[0].book_id, "b");
}
