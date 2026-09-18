//! Tests for session changes during an active request.

use super::super::*;
use super::*;

#[test]
fn selecting_and_deleting_reject_the_wrong_window_kind_or_primary_book() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let first_book = library.create_book("First", "").unwrap().id;
    let second_book = library.create_book("Second", "").unwrap().id;
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let thread = services
            .chat()
            .create_thread(NewChatThread {
                primary_book_id: Some(first_book.clone()),
                title: "Reader chat".to_string(),
                scope: ChatScope::new([first_book.clone()]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let first_scope = vec![first_book.clone()];
        let second_scope = vec![second_book.clone()];
        let editor = AgentConversation::new(
            Arc::clone(&services),
            ChatWindowKind::Editor,
            Some(first_book.clone()),
        )
        .unwrap();
        let other_reader = AgentConversation::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(second_book.clone()),
        )
        .unwrap();

        let error = editor
            .select_session(&thread.id, &first_scope)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("window kind"));
        let error = other_reader
            .select_session(&thread.id, &second_scope)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("primary book"));
        let error = editor
            .delete_session(&thread.id, &first_scope)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("window kind"));
        let error = other_reader
            .delete_session(&thread.id, &second_scope)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("primary book"));
        assert_eq!(editor.selected_thread_id().await, None);
        assert_eq!(other_reader.selected_thread_id().await, None);
        assert!(services.chat().session(&thread.id).await.unwrap().is_some());
    });
}

#[test]
fn reader_scope_narrowing_rejects_restore_list_and_selection() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let book_a = library.create_book("Book A", "").unwrap().id;
    let book_b = library.create_book("Book B", "").unwrap().id;
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let allowed_a = vec![book_a.clone()];
        let allowed_b = vec![book_b.clone()];
        let allowed_a_and_b = vec![book_a.clone(), book_b.clone()];
        let broad = services
            .chat()
            .create_thread(NewChatThread {
                primary_book_id: Some(book_a.clone()),
                title: "Reader A and B".to_string(),
                scope: ChatScope::new(allowed_a_and_b.clone()).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let conversation = AgentConversation::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book_a.clone()),
        )
        .unwrap();

        assert!(conversation.restore(&allowed_a).await.unwrap().is_none());
        assert!(
            conversation
                .list_sessions(&allowed_a)
                .await
                .unwrap()
                .is_empty()
        );
        let error = conversation
            .select_session(&broad.id, &allowed_a)
            .await
            .expect_err("narrowed Reader scope must reject A/B history");
        assert!(error.to_string().contains("host-authorized scope"));

        for error in [
            conversation.restore(&allowed_b).await.unwrap_err(),
            conversation.list_sessions(&allowed_b).await.unwrap_err(),
            conversation
                .select_session(&broad.id, &allowed_b)
                .await
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("required current book"));
        }

        let selected = conversation
            .select_session(&broad.id, &allowed_a_and_b)
            .await
            .unwrap();
        assert_eq!(selected.thread.id, broad.id);
    });
}

#[test]
fn active_request_blocks_session_changes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let conversation = AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
        .expect("create conversation");

    runtime.block_on(async {
        let thread = services
            .chat()
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Existing".to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        conversation.select_session(&thread.id, &[]).await.unwrap();
        let prepared = conversation.prepare_request(97).unwrap();

        assert!(conversation.start_new_session().await.is_err());
        assert!(conversation.select_session(&thread.id, &[]).await.is_err());
        assert!(conversation.delete_session(&thread.id, &[]).await.is_err());
        assert!(conversation.reconcile_scope(&[]).await.is_err());
        assert!(
            services.chat().session(&thread.id).await.unwrap().is_some(),
            "a blocked deletion must leave the persisted session intact"
        );
        assert_eq!(
            conversation.selected_thread_id().await.as_deref(),
            Some(thread.id.as_str())
        );

        drop(prepared);
        conversation.start_new_session().await.unwrap();
        assert_eq!(conversation.selected_thread_id().await, None);
    });
}

#[test]
fn prepared_request_can_be_cancelled_before_ask_without_persistence() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let conversation = AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
        .expect("create conversation");
    let prepared = conversation.prepare_request(41).expect("prepare request");
    let cancellation = prepared.cancellation();

    assert_eq!(prepared.request_id(), 41);
    assert!(conversation.cancel(41));
    assert!(cancellation.is_cancelled());

    let error = runtime
        .block_on(conversation.ask_prepared(
            ConversationQuestion {
                request_id: 41,
                question: "must never be persisted or sent".to_string(),
                allowed_book_ids: Vec::new(),
                book_titles: Vec::new(),
                snapshots: Vec::new(),
            },
            None,
            prepared,
        ))
        .expect_err("cancelled preparation must not run");
    assert!(error.to_string().contains("cancelled"));
    assert!(error.is::<AgentRequestCancelled>());
    assert_eq!(error_kind(&error), "cancelled");
    assert!(!conversation.cancel(41), "request lease must be removed");

    let threads = runtime
        .block_on(services.chat().list_threads(ChatWindowKind::Library, None))
        .expect("list conversations");
    assert!(
        threads.is_empty(),
        "a cancelled prepared request must not create persisted chat state"
    );
}

#[test]
fn duplicate_prepare_does_not_replace_the_original_cancellation_token() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let conversation = AgentConversation::new(services, ChatWindowKind::Library, None)
        .expect("create conversation");
    let prepared = conversation.prepare_request(73).expect("prepare request");
    let cancellation = prepared.cancellation();

    assert!(conversation.prepare_request(73).is_err());
    assert!(conversation.cancel(73));
    assert!(
        cancellation.is_cancelled(),
        "duplicate registration must leave the original request cancellable"
    );
    drop(prepared);
    assert!(!conversation.cancel(73));
}

#[test]
fn narrowing_scope_starts_a_clean_persisted_conversation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let conversation = AgentConversation::new(services, ChatWindowKind::Library, None)
        .expect("create conversation");

    runtime.block_on(async {
        let repository = conversation.services.chat();
        let broad_scope = ChatScope::new(["book-a", "book-b"]).unwrap();
        let (broad_thread, history) = conversation
            .thread_and_history(&repository, "Broad question", broad_scope.clone())
            .await
            .unwrap();
        assert!(history.is_empty());
        repository
            .append_message(
                &broad_thread,
                NewChatMessage::text(ChatRole::Assistant, "secret from book-b"),
            )
            .await
            .unwrap();

        let narrow_scope = ChatScope::new(["book-a"]).unwrap();
        let (narrow_thread, history) = conversation
            .thread_and_history(&repository, "Narrow question", narrow_scope.clone())
            .await
            .unwrap();
        assert_ne!(narrow_thread, broad_thread);
        assert!(
            history.is_empty(),
            "removed-book history must not be reused"
        );
        assert_eq!(
            repository
                .thread(&broad_thread)
                .await
                .unwrap()
                .unwrap()
                .scope,
            broad_scope
        );
        assert_eq!(
            repository
                .thread(&narrow_thread)
                .await
                .unwrap()
                .unwrap()
                .scope,
            narrow_scope
        );
    });
}
