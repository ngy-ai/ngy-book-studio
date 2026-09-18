//! Tests for the IPC identity, selection limits and pending actions.

use super::*;

#[test]
fn ipc_snapshot_is_keyed_to_its_page() {
    let state = EditorWebState::new(
        "book-a".to_string(),
        "/EPUB/ch.xhtml".to_string(),
        "<html><head></head><body><p>old</p></body></html>".to_string(),
    );
    let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
    let update = state
        .apply_message(EditorIpcMessage {
            session_id: session_id.clone(),
            chapter_id: chapter_id.clone(),
            href: "EPUB/ch.xhtml".to_string(),
            revision: 0,
            request_id: Some(11),
            body: Some("<body><p>new</p></body>".to_string()),
            selected_text: "  exact\n selection  ".to_string(),
            too_large: false,
            ready: false,
        })
        .expect("matching snapshot");
    assert!(update.html.contains("<p>new</p>"));
    assert_eq!(update.href, "/EPUB/ch.xhtml");
    assert_eq!(update.request_id, Some(11));
    assert_eq!(update.selected_text.as_deref(), Some("exact selection"));
    assert!(
        state
            .apply_message(EditorIpcMessage {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: "EPUB/other.xhtml".to_string(),
                revision: 0,
                request_id: None,
                body: Some("<body><p>wrong chapter</p></body>".to_string()),
                selected_text: String::new(),
                too_large: false,
                ready: false,
            })
            .is_none()
    );
    let unchanged = state
        .apply_message(EditorIpcMessage {
            session_id,
            chapter_id,
            href: "EPUB/ch.xhtml".to_string(),
            revision: 0,
            request_id: Some(12),
            body: None,
            selected_text: String::new(),
            too_large: false,
            ready: false,
        })
        .expect("unchanged snapshot acknowledgement");
    assert_eq!(unchanged.request_id, Some(12));
    assert!(unchanged.html.contains("<p>new</p>"));
}

#[test]
fn ipc_parser_requires_the_complete_snapshot_identity() {
    let complete = serde_json::json!({
        "session_id": "session-a",
        "chapter_id": "unit-a",
        "href": "EPUB/ch.xhtml",
        "revision": 3,
        "request_id": 11,
        "body": null,
        "selected_text": "",
        "too_large": false,
        "ready": false
    });
    let parsed = parse_editor_ipc_message(&complete.to_string()).expect("complete identity");
    assert_eq!(parsed.session_id, "session-a");
    assert_eq!(parsed.chapter_id, "unit-a");
    assert_eq!(parsed.request_id, Some(11));

    for missing in ["session_id", "chapter_id", "revision", "request_id"] {
        let mut legacy = complete.clone();
        legacy.as_object_mut().unwrap().remove(missing);
        assert!(
            parse_editor_ipc_message(&legacy.to_string()).is_none(),
            "legacy message without {missing} must be rejected"
        );
    }
    let mut unknown = complete;
    unknown["legacy_href"] = serde_json::json!("EPUB/ch.xhtml");
    assert!(parse_editor_ipc_message(&unknown.to_string()).is_none());
    let mut zero_request = unknown;
    zero_request.as_object_mut().unwrap().remove("legacy_href");
    zero_request["request_id"] = serde_json::json!(0);
    assert!(parse_editor_ipc_message(&zero_request.to_string()).is_none());
}

#[test]
fn ipc_is_accepted_only_from_the_trusted_shell_origin() {
    let message = EditorIpcMessage {
        session_id: "session-a".to_string(),
        chapter_id: "unit-a".to_string(),
        href: "EPUB/ch.xhtml".to_string(),
        revision: 3,
        request_id: Some(11),
        body: None,
        selected_text: String::new(),
        too_large: false,
        ready: false,
    };
    let shell = editor_document_request(
        &"epubeditor://shell/EPUB/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=3"
            .parse()
            .unwrap(),
    )
    .unwrap();
    let content = editor_document_request(
            &"epubeditor://content/EPUB/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=3"
                .parse()
                .unwrap(),
        )
        .unwrap();

    assert!(editor_ipc_matches_document_request(&shell, &message));
    assert!(!editor_ipc_matches_document_request(&content, &message));
    let mut stale = message;
    stale.revision += 1;
    assert!(!editor_ipc_matches_document_request(&shell, &stale));
}

#[test]
fn citation_result_requires_the_exact_preview_page_identity() {
    let result = serde_json::json!({
        "type": "citation_navigation_result",
        "session_id": "session-a",
        "chapter_id": "unit-a",
        "href": "chapter-1.xhtml",
        "revision": 4,
        "request_id": 4,
        "found": false,
        "reason": "ambiguous"
    });
    let parsed = parse_editor_citation_navigation_result(&result.to_string()).unwrap();
    assert_eq!(parsed.chapter_id, "unit-a");
    assert_eq!(parsed.reason, "ambiguous");

    let preview = editor_document_request(
            &"epubeditor://content/chapter-1.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=4"
                .parse()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(preview.kind, EditorDocumentKind::Preview);
    assert_eq!(parsed.session_id, preview.session_id);
    assert_eq!(parsed.chapter_id, preview.chapter_id);
    assert_eq!(parsed.revision, preview.revision);
    assert_eq!(normalized_editor_path(&parsed.href).unwrap(), preview.href);

    let mut stale = result;
    stale["revision"] = serde_json::json!(3);
    let stale = parse_editor_citation_navigation_result(&stale.to_string()).unwrap();
    assert_ne!(stale.revision, preview.revision);
}

#[test]
fn ipc_state_rejects_another_session_or_stable_chapter() {
    let state = EditorWebState::new(
        "book-a".to_string(),
        "EPUB/ch.xhtml".to_string(),
        "<html><body><p>old</p></body></html>".to_string(),
    );
    let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
    let message = |session_id: String, chapter_id: String| EditorIpcMessage {
        session_id,
        chapter_id,
        href: "EPUB/ch.xhtml".to_string(),
        revision: 0,
        request_id: Some(1),
        body: None,
        selected_text: String::new(),
        too_large: false,
        ready: false,
    };
    assert!(
        state
            .apply_message(message("another-session".to_string(), chapter_id.clone()))
            .is_none()
    );
    assert!(
        state
            .apply_message(message(session_id, "another-unit".to_string()))
            .is_none()
    );
}

#[test]
fn ipc_ready_handshake_is_keyed_to_its_page() {
    let state = EditorWebState::new(
        "book-a".to_string(),
        "/EPUB/ch.xhtml".to_string(),
        "<html><body><p>old</p></body></html>".to_string(),
    );
    let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
    let ready = state
        .apply_message(EditorIpcMessage {
            session_id: session_id.clone(),
            chapter_id: chapter_id.clone(),
            href: "EPUB/ch.xhtml".to_string(),
            revision: 0,
            request_id: None,
            body: None,
            selected_text: String::new(),
            too_large: false,
            ready: true,
        })
        .expect("matching ready message");
    assert!(ready.ready);
    assert_eq!(ready.href, "/EPUB/ch.xhtml");
    assert!(
        state
            .apply_message(EditorIpcMessage {
                session_id,
                chapter_id,
                href: "EPUB/other.xhtml".to_string(),
                revision: 0,
                request_id: None,
                body: None,
                selected_text: String::new(),
                too_large: false,
                ready: true,
            })
            .is_none()
    );
}

#[test]
fn ipc_rejects_selection_text_over_the_host_limit() {
    let state = EditorWebState::new(
        "book-a".to_string(),
        "/EPUB/ch.xhtml".to_string(),
        "<html><body><p>old</p></body></html>".to_string(),
    );
    let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
    assert!(
        state
            .apply_message(EditorIpcMessage {
                session_id,
                chapter_id,
                href: "EPUB/ch.xhtml".to_string(),
                revision: 0,
                request_id: Some(9),
                body: None,
                selected_text: "x".repeat(MAX_EDITOR_SELECTION_BYTES + 1),
                too_large: false,
                ready: false,
            })
            .is_none()
    );
}

#[test]
fn pending_editor_actions_are_latest_wins_but_close_is_sticky() {
    let mut export_path = None;
    assert_eq!(
        merge_pending_editor_action(
            PendingEditorAction::SwitchTab(EditorTab::Preview),
            PendingEditorAction::SwitchTab(EditorTab::Source),
            &mut export_path,
        ),
        PendingEditorAction::SwitchTab(EditorTab::Source)
    );
    let mut export_path = None;
    assert_eq!(
        merge_pending_editor_action(
            PendingEditorAction::Save,
            PendingEditorAction::Close,
            &mut export_path,
        ),
        PendingEditorAction::Close
    );
    let mut export_path = None;
    assert_eq!(
        merge_pending_editor_action(
            PendingEditorAction::Close,
            PendingEditorAction::SwitchTab(EditorTab::Source),
            &mut export_path,
        ),
        PendingEditorAction::Close
    );
}

#[test]
fn editor_write_completion_never_closes_after_failure_or_export_error() {
    let operation = ActiveEditorWrite {
        id: 1,
        intent: EditorWriteIntent::Close,
        draft_generation: 7,
        title: "测试".to_string(),
        target_display: None,
    };
    assert_eq!(
        editor_write_follow_up(&operation, true, 7, false, false),
        EditorWriteFollowUp::StayOpen
    );

    let export = ActiveEditorWrite {
        intent: EditorWriteIntent::Export,
        ..operation.clone()
    };
    assert_eq!(
        editor_write_follow_up(&export, true, 7, true, false),
        EditorWriteFollowUp::StayOpen
    );
    assert_eq!(
        editor_write_follow_up(&operation, true, 8, true, true),
        EditorWriteFollowUp::SaveThenClose
    );
    assert_eq!(
        editor_write_follow_up(&operation, true, 7, true, true),
        EditorWriteFollowUp::Close
    );
}
