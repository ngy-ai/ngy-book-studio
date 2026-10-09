//! Tests for the selection-explanation flow and session handling.

use super::*;

#[test]
pub(crate) fn explains_local_ollama_connection_failure() {
    assert!(friendly_agent_error("connection refused").contains("Ollama"));
}

#[test]
pub(crate) fn selection_explanation_rejects_missing_selection_and_untrusted_scope() {
    let allowed = vec!["book-a".to_string()];
    let mut selected = reference("book-a", "unit-1", 0);
    assert!(validate_explanation_reference(&selected, &allowed).is_err());
    selected.frozen_text = Some("  \n".to_string());
    assert!(validate_explanation_reference(&selected, &allowed).is_err());
    selected.frozen_text = Some("selected words\n精确选区".to_string());
    assert_eq!(
        selected.revision, None,
        "Reader uses host-authorized metadata"
    );
    assert!(validate_explanation_reference(&selected, &allowed).is_ok());
    assert!(validate_explanation_reference(&selected, &["other".to_string()]).is_err());
    selected.frozen_text = Some("x".repeat(MAX_REFERENCE_TOTAL_BYTES + 1));
    assert!(validate_explanation_reference(&selected, &allowed).is_err());
}

#[gpui::test]
pub(crate) fn reader_selection_explanation_emits_once_after_fresh_session_callback(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
    let book = library.create_book("Reader fixture", "Author").unwrap();
    let document = library.document(&book.id).unwrap();
    let mut selected = reference(&book.id, &document.units[0].id, 0);
    selected.locator = Some(DocumentLocator::unit(&book.id, &document.units[0].id));
    selected.frozen_text = Some("original menu selection".to_string());
    assert_eq!(selected.revision, None);
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).unwrap());
    let runtime = services.runtime();
    let controller = AiSidebarController::new(
        Arc::clone(&services),
        ChatWindowKind::Reader,
        Some(book.id.clone()),
    )
    .unwrap();
    let backend = controller.conversation.clone();
    let thread = runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some(book.id.clone()),
                title: "Previous question".to_string(),
                scope: ChatScope::new([book.id.clone()]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        backend
            .select_session(&thread.id, std::slice::from_ref(&book.id))
            .await
            .unwrap();
        thread
    });
    cx.update(gpui_component::init);
    let mut sidebar = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(AiBookOption::new(&book.id, "Reader fixture"), Vec::new()),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        sidebar = Some(view.clone());
        Root::new(view, window, cx)
    });
    let sidebar = sidebar.unwrap();
    sidebar.update(visual, |sidebar, cx| {
        sidebar.apply_session(
            Some(thread.id.clone()),
            vec![AiThreadOption::new(
                &thread.id,
                &thread.title,
                vec![book.id.clone()],
            )],
            vec![AiRestoredMessage {
                role: AiRestoredRole::User,
                content: "Previous question".to_string(),
                sources: Vec::new(),
                source_status: None,
                db_id: "stored-question".to_string(),
            }],
            cx,
        );
    });
    let controller = visual.new(|_| controller);
    let requests = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&requests);
    let _subscription = sidebar.update(visual, |_, cx| {
        cx.subscribe(&sidebar, move |_, _, event, _| {
            if let AiSidebarEvent::Submit(request) = event {
                recorded.borrow_mut().push(request.clone());
            }
        })
    });
    controller.update(visual, |controller, cx| {
        assert!(controller.explain_selection(selected.clone(), sidebar.clone(), cx));
        assert!(!controller.explain_selection(selected.clone(), sidebar.clone(), cx));
        assert!(
            requests.borrow().is_empty(),
            "submission waits for backend reset"
        );
        let mut changed = selected.clone();
        changed.frozen_text = Some("later live selection".to_string());
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(vec![changed], cx)
        });
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while requests.borrow().is_empty() && Instant::now() < deadline {
        visual.run_until_parked();
        // The product uses an independent Tokio runtime, outside GPUI's
        // deterministic executor. Give that real worker a bounded turn.
        std::thread::sleep(Duration::from_millis(1));
    }
    let emitted = requests.borrow();
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].question, "用汉语详细解释一下");
    assert_eq!(emitted[0].reference_hints, vec![selected]);
    assert_eq!(runtime.block_on(backend.selected_thread_id()), None);
    assert!(!sidebar.read_with(visual, |sidebar, _| sidebar.is_collapsed()));
    controller.update(visual, |controller, _| controller.close());
}

#[test]
pub(crate) fn fresh_ui_session_waits_for_backend_reset_and_keeps_old_persisted_history() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let conversation = AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
        .expect("create conversation");
    runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Previous question".to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        repository
            .append_message(
                &thread.id,
                NewChatMessage::text(ChatRole::User, "Previous question"),
            )
            .await
            .unwrap();
        conversation.select_session(&thread.id, &[]).await.unwrap();
        let prepared = conversation.prepare_request(41).unwrap();
        assert!(
            start_new_ui_session(Arc::clone(&services), conversation.clone(), Vec::new())
                .await
                .is_err()
        );
        assert_eq!(
            conversation.selected_thread_id().await,
            Some(thread.id.clone())
        );
        assert!(!prepared.cancellation().is_cancelled());
        drop(prepared);

        let state = start_new_ui_session(Arc::clone(&services), conversation.clone(), Vec::new())
            .await
            .unwrap();
        assert_eq!(state.active_thread_id, None);
        assert!(state.messages.is_empty());
        assert_eq!(state.threads.len(), 1);
        assert_eq!(state.threads[0].id, thread.id);
        assert_eq!(conversation.selected_thread_id().await, None);
        assert!(conversation.restore(&[]).await.unwrap().is_none());
        let saved = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(saved.messages.len(), 1);
        assert_eq!(saved.messages[0].content, "Previous question");
    });
}

#[test]
pub(crate) fn editor_frozen_text_replaces_only_the_matching_selected_reference() {
    let first = reference("book-a", "unit-1", 0);
    let second = reference("book-a", "unit-2", 1);
    let mut frozen = first.clone();
    frozen.revision = Some(27);
    frozen.frozen_text = Some("未保存的正文\nexact bytes".to_string());

    let merged = merge_editor_reference(vec![first, second.clone()], Some(frozen.clone()));

    assert_eq!(merged, vec![frozen.clone(), second]);
    let snapshot = capture_frozen_reference(&merged[0]).unwrap().unwrap();
    assert_eq!(snapshot.text, "未保存的正文\nexact bytes");
    assert_eq!(snapshot.revision, 27);
    assert!(snapshot.validate().is_ok());

    let outside = reference("book-secret", "unit-x", 0);
    assert_eq!(
        merge_editor_reference(merged.clone(), Some(outside)),
        merged
    );
}

#[test]
pub(crate) fn controller_rejects_out_of_scope_duplicate_and_too_many_references() {
    let allowed = vec!["book-a".to_string()];
    assert!(validate_reference_request(&[reference("book-a", "unit-1", 0)], &allowed).is_ok());
    assert!(
        validate_reference_request(&[reference("book-secret", "unit-1", 0)], &allowed)
            .unwrap_err()
            .to_string()
            .contains("授权范围")
    );
    let duplicate = reference("book-a", "unit-1", 0);
    assert!(
        validate_reference_request(&[duplicate.clone(), duplicate], &allowed)
            .unwrap_err()
            .to_string()
            .contains("重复")
    );

    let too_many = (0..=MAX_SELECTED_REFERENCES)
        .map(|index| reference("book-a", &format!("unit-{index}"), index))
        .collect::<Vec<_>>();
    assert!(
        validate_reference_request(&too_many, &allowed)
            .unwrap_err()
            .to_string()
            .contains("最多引用")
    );
}
