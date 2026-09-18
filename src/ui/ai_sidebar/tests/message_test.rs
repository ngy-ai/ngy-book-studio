//! Tests for message projection, badges and the session picker.

use super::*;

#[test]
fn selectable_message_html_keeps_untrusted_chat_content_inert() {
    let message =
        "<img src=\"https://example.test/tracker\"> & [link](https://example.test)\r\n'quoted'";
    let html = selectable_message_html(message);

    assert_eq!(
        html,
        "<p>&lt;img src=&quot;https://example.test/tracker&quot;&gt; &amp; [link](https://example.test)</p><p>&#39;quoted&#39;</p>"
    );
    assert!(!html.contains("<br>"));
    assert!(!html.contains("<img"));
    assert!(!html.contains("<a"));
}

#[test]
fn no_source_warning_appears_only_after_an_explicit_final_status() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let request = state.begin("question").unwrap();
    assert!(state.append_delta(request.request_id, "model answer"));

    let provisional = state
        .messages
        .iter()
        .find(|message| message.role == AiMessageRole::Assistant)
        .unwrap();
    assert_eq!(provisional.source_status, None);
    assert!(!should_show_no_knowledge_base_source_warning(provisional));

    assert!(state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));
    let finished = state
        .messages
        .iter()
        .find(|message| message.role == AiMessageRole::Assistant)
        .unwrap();
    assert!(should_show_no_knowledge_base_source_warning(finished));
    assert_eq!(finished.content, "model answer");
    assert!(
        !finished
            .content
            .contains(NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE)
    );
    assert!(
        !selectable_message_html(&finished.content)
            .contains(NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE)
    );

    let mut grounded_without_displayable_links = finished.clone();
    grounded_without_displayable_links.source_status =
        Some(AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources);
    grounded_without_displayable_links.sources.clear();
    assert!(!should_show_no_knowledge_base_source_warning(
        &grounded_without_displayable_links
    ));
}

#[test]
fn source_basis_badge_names_the_knowledge_the_answer_used() {
    fn assistant(status: AgentAnswerSourceStatus) -> AiMessage {
        AiMessage {
            id: 1,
            role: AiMessageRole::Assistant,
            content: "answer".to_string(),
            sources: Vec::new(),
            source_status: Some(status),
            request_id: None,
        }
    }
    assert_eq!(
        source_basis_badge(&assistant(
            AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
        ))
        .unwrap()
        .0,
        "已基于当前图书内容回答"
    );
    assert_eq!(
        source_basis_badge(&assistant(AgentAnswerSourceStatus::VerifiedWebSources))
            .unwrap()
            .0,
        "已基于联网搜索结果回答"
    );
    assert_eq!(
        source_basis_badge(&assistant(AgentAnswerSourceStatus::VerifiedMixedSources))
            .unwrap()
            .0,
        "已结合图书与联网搜索回答"
    );
    // Ungrounded answers keep the existing warning instead of a badge.
    assert!(source_basis_badge(&assistant(AgentAnswerSourceStatus::NoVerifiedSources)).is_none());
}

#[test]
fn web_source_links_survive_the_book_scope_filter() {
    let allowed = std::collections::HashSet::from(["book-a".to_string()]);
    let web = AiSourceLink {
        citation_id: "web:0".to_string(),
        book_id: String::new(),
        unit_id: String::new(),
        unit_index: None,
        document_revision: Revision::new(0),
        unit_revision: Revision::new(0),
        locator: None,
        label: "Docs".to_string(),
        quote: Some("snippet".to_string()),
        selection_snapshot: false,
        stale: false,
        url: Some("https://example.test/docs".to_string()),
    };
    assert!(allowed_source_link(&allowed, &web));

    // A book citation outside the current scope is still rejected.
    let mut foreign_book = web.clone();
    foreign_book.url = None;
    foreign_book.book_id = "secret".to_string();
    assert!(!allowed_source_link(&allowed, &foreign_book));
}

#[test]
fn session_picker_tracks_active_thread_and_recovers_from_errors() {
    let first = AiThreadOption::new("thread-a", "First", vec!["book-a".to_string()]);
    let second = AiThreadOption::new("thread-b", "Second", vec!["book-a".to_string()]);
    let mut sessions = SessionPickerState::default();

    assert_eq!(sessions.active_title(), "新对话");
    assert!(sessions.begin());
    assert!(!sessions.begin());
    sessions.apply(
        Some(second.id.clone()),
        vec![first.clone(), second.clone(), second],
    );
    assert_eq!(sessions.active_title(), "Second");
    assert_eq!(sessions.threads.len(), 2);

    assert!(sessions.begin());
    sessions.fail("会话正被其它窗口使用");
    assert!(!sessions.busy);
    assert_eq!(sessions.active_title(), "Second");
    assert_eq!(sessions.error.as_deref(), Some("会话正被其它窗口使用"));
}

#[test]
fn session_delete_confirmation_is_cleared_by_any_started_or_applied_operation() {
    let thread = AiThreadOption::new("thread-a", "First", vec!["book-a".to_string()]);
    let pending = PendingSessionDeletion {
        thread_id: thread.id.clone(),
        title: thread.title.clone(),
    };
    let mut sessions = SessionPickerState {
        threads: vec![thread.clone()],
        active_thread_id: Some(thread.id.clone()),
        open: true,
        pending_delete: Some(pending.clone()),
        ..SessionPickerState::default()
    };

    assert!(sessions.begin());
    assert_eq!(sessions.pending_delete, None);
    sessions.pending_delete = Some(pending);
    sessions.apply(None, vec![thread]);
    assert_eq!(sessions.pending_delete, None);
    assert_eq!(sessions.active_thread_id, None);
}

#[test]
fn session_options_are_hard_filtered_by_the_current_book_scope() {
    let allowed = ["book-a".to_string(), "book-b".to_string()]
        .into_iter()
        .collect::<HashSet<_>>();
    let options = authorized_thread_options(
        vec![
            AiThreadOption::new("a", "A", vec!["book-a".to_string()]),
            AiThreadOption::new(
                "ab",
                "A + B",
                vec!["book-a".to_string(), "book-b".to_string()],
            ),
            AiThreadOption::new("c", "C", vec!["book-c".to_string()]),
        ],
        &allowed,
    );

    assert_eq!(
        options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "ab"]
    );
}

#[test]
fn changing_persisted_threads_keeps_only_the_in_flight_request_projection() {
    let mut conversation =
        ConversationState::new(AiSidebarScope::library("Shelf", vec![book("book-a")]));
    conversation.messages.push(AiMessage {
        id: 99,
        role: AiMessageRole::Assistant,
        content: "old session".to_string(),
        sources: Vec::new(),
        source_status: Some(AgentAnswerSourceStatus::NoVerifiedSources),
        request_id: None,
    });
    let request = conversation.begin("new question").unwrap();
    assert!(conversation.append_delta(request.request_id, "new answer"));

    conversation.retain_current_request_messages();

    assert_eq!(conversation.messages.len(), 2);
    assert!(
        conversation
            .messages
            .iter()
            .all(|message| message.request_id == Some(request.request_id))
    );
    assert!(
        conversation
            .messages
            .iter()
            .all(|message| message.content != "old session")
    );
}
