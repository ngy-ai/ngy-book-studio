//! Tests for scope filtering and session switching.

use super::super::*;
use super::*;

#[test]
fn library_group_scope_filters_restore_list_select_and_reconcile() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let group_a = vec!["book-a".to_string()];
        let group_a_and_b = vec!["book-a".to_string(), "book-b".to_string()];
        let broad = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "A and B".to_string(),
                scope: ChatScope::new(group_a_and_b.clone()).unwrap(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();

        assert!(
            conversation.restore(&group_a).await.unwrap().is_none(),
            "a recent conversation outside the current group must be skipped"
        );
        assert!(
            conversation
                .list_sessions(&group_a)
                .await
                .unwrap()
                .is_empty()
        );
        let error = conversation
            .select_session(&broad.id, &group_a)
            .await
            .expect_err("a broader conversation must not be selectable");
        assert!(error.to_string().contains("host-authorized scope"));

        conversation
            .select_session(&broad.id, &group_a_and_b)
            .await
            .unwrap();
        assert!(!conversation.reconcile_scope(&group_a_and_b).await.unwrap());
        assert!(conversation.reconcile_scope(&group_a).await.unwrap());
        assert_eq!(conversation.selected_thread_id().await, None);

        let other_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        other_window
            .select_session(&broad.id, &group_a_and_b)
            .await
            .expect("scope reconciliation must release the old claim");

        let narrow = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "A only".to_string(),
                scope: ChatScope::new(group_a.clone()).unwrap(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        let visible = conversation.list_sessions(&group_a).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, narrow.id);

        let restore_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        assert_eq!(
            restore_window
                .restore(&group_a)
                .await
                .unwrap()
                .unwrap()
                .thread
                .id,
            narrow.id
        );
    });
}

#[test]
fn switching_sessions_releases_the_previous_claim() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let create = |title: &str| NewChatThread {
            primary_book_id: None,
            title: title.to_string(),
            scope: ChatScope::default(),
            window_kind: ChatWindowKind::Library,
        };
        let first = repository.create_thread(create("A")).await.unwrap();
        let second = repository.create_thread(create("B")).await.unwrap();
        let first_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        let second_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();

        first_window.select_session(&first.id, &[]).await.unwrap();
        first_window.select_session(&second.id, &[]).await.unwrap();
        second_window
            .select_session(&first.id, &[])
            .await
            .expect("switching to B must release A");

        assert_eq!(
            first_window.selected_thread_id().await.as_deref(),
            Some(second.id.as_str())
        );
        assert_eq!(
            second_window.selected_thread_id().await.as_deref(),
            Some(first.id.as_str())
        );
    });
}

#[test]
fn switching_sessions_restores_only_the_selected_history() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let create = |title: &str| NewChatThread {
            primary_book_id: None,
            title: title.to_string(),
            scope: ChatScope::default(),
            window_kind: ChatWindowKind::Library,
        };
        let first = repository.create_thread(create("First")).await.unwrap();
        let second = repository.create_thread(create("Second")).await.unwrap();
        repository
            .append_message(
                &first.id,
                NewChatMessage::text(ChatRole::User, "first question"),
            )
            .await
            .unwrap();
        repository
            .append_message(
                &second.id,
                NewChatMessage::text(ChatRole::Assistant, "second answer"),
            )
            .await
            .unwrap();

        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        let first_session = conversation.select_session(&first.id, &[]).await.unwrap();
        assert_eq!(first_session.messages.len(), 1);
        assert_eq!(first_session.messages[0].content, "first question");

        let second_session = conversation.select_session(&second.id, &[]).await.unwrap();
        assert_eq!(second_session.messages.len(), 1);
        assert_eq!(second_session.messages[0].content, "second answer");
        assert_eq!(conversation.list_sessions(&[]).await.unwrap().len(), 2);
    });
}

#[test]
fn deleting_selected_session_starts_a_fresh_conversation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Delete current".to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        repository
            .append_message(
                &thread.id,
                NewChatMessage::text(ChatRole::User, "do not keep this"),
            )
            .await
            .unwrap();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        conversation.select_session(&thread.id, &[]).await.unwrap();

        assert!(
            conversation
                .delete_session(&thread.id, &[])
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(conversation.selected_thread_id().await, None);
        assert!(repository.session(&thread.id).await.unwrap().is_none());
        assert!(
            conversation.restore(&[]).await.unwrap().is_none(),
            "an explicit deletion must not silently restore another conversation"
        );
    });
}

#[test]
fn deleting_inactive_session_preserves_the_selected_transcript_and_claim() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let create = |title: &str| NewChatThread {
            primary_book_id: None,
            title: title.to_string(),
            scope: ChatScope::default(),
            window_kind: ChatWindowKind::Library,
        };
        let selected = repository.create_thread(create("Selected")).await.unwrap();
        let inactive = repository.create_thread(create("Inactive")).await.unwrap();
        repository
            .append_message(
                &selected.id,
                NewChatMessage::text(ChatRole::User, "keep this transcript"),
            )
            .await
            .unwrap();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        conversation
            .select_session(&selected.id, &[])
            .await
            .unwrap();

        let still_selected = conversation
            .delete_session(&inactive.id, &[])
            .await
            .unwrap()
            .expect("the selected session remains active");
        assert_eq!(still_selected.thread.id, selected.id);
        assert_eq!(still_selected.messages.len(), 1);
        assert_eq!(still_selected.messages[0].content, "keep this transcript");
        assert_eq!(
            conversation.selected_thread_id().await.as_deref(),
            Some(selected.id.as_str())
        );
        assert!(repository.session(&inactive.id).await.unwrap().is_none());

        let other_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        assert!(
            other_window
                .select_session(&selected.id, &[])
                .await
                .is_err(),
            "deleting an inactive session must not release the active claim"
        );
    });
}

#[test]
fn deleting_session_claimed_by_another_window_is_rejected() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Claimed".to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        let owner =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        let deleting_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        owner.select_session(&thread.id, &[]).await.unwrap();

        let error = deleting_window
            .delete_session(&thread.id, &[])
            .await
            .expect_err("another window owns this conversation");
        assert!(error.to_string().contains("another window"));
        assert!(repository.session(&thread.id).await.unwrap().is_some());
        assert_eq!(
            owner.selected_thread_id().await.as_deref(),
            Some(thread.id.as_str())
        );
        assert_eq!(deleting_window.selected_thread_id().await, None);
    });
}

#[test]
fn deleting_session_outside_the_host_scope_is_rejected() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Broad scope".to_string(),
                scope: ChatScope::new(["book-a", "book-b"]).unwrap(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();

        let error = conversation
            .delete_session(&thread.id, &["book-a".to_string()])
            .await
            .expect_err("a narrower host scope cannot delete broader history");
        assert!(error.to_string().contains("host-authorized scope"));
        assert!(repository.session(&thread.id).await.unwrap().is_some());
        assert_eq!(conversation.selected_thread_id().await, None);
    });
}

#[test]
fn deleting_missing_session_is_rejected_without_changing_selection() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let conversation = AgentConversation::new(services, ChatWindowKind::Library, None).unwrap();

    let error = runtime
        .block_on(conversation.delete_session("missing-thread", &[]))
        .expect_err("a missing conversation cannot be deleted");
    assert!(error.to_string().contains("does not exist"));
    assert_eq!(
        runtime.block_on(conversation.selected_thread_id()),
        None,
        "a failed deletion must not initialize or replace the selection"
    );
}

#[test]
fn failed_switch_to_claimed_session_keeps_the_previous_claim() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    runtime.block_on(async {
        let repository = services.chat();
        let create = |title: &str| NewChatThread {
            primary_book_id: None,
            title: title.to_string(),
            scope: ChatScope::default(),
            window_kind: ChatWindowKind::Library,
        };
        let first = repository.create_thread(create("A")).await.unwrap();
        let second = repository.create_thread(create("B")).await.unwrap();
        let first_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        let second_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        let third_window =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();

        first_window.select_session(&first.id, &[]).await.unwrap();
        second_window.select_session(&second.id, &[]).await.unwrap();
        let error = first_window
            .select_session(&second.id, &[])
            .await
            .expect_err("B is owned by the second window");
        assert!(error.to_string().contains("another window"));
        assert_eq!(
            first_window.selected_thread_id().await.as_deref(),
            Some(first.id.as_str())
        );
        assert!(
            third_window.select_session(&first.id, &[]).await.is_err(),
            "A must remain claimed after the failed switch"
        );
    });
}
