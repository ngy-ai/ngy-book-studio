//! Tests for claim reservations, history bounds and titles.

use super::super::*;
use super::*;

#[test]
fn history_is_bounded_and_keeps_chronological_order() {
    let messages = (0..120)
        .map(|index| StoredChatMessage {
            id: format!("m-{index}"),
            thread_id: "thread".into(),
            parent_id: None,
            ordinal: index,
            role: if index % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            },
            content: format!("message {index}"),
            model: None,
            created_at: 0,
            citations: Vec::new(),
        })
        .collect::<Vec<_>>();
    let history = history_messages(&messages);
    assert_eq!(history.len(), HISTORY_MESSAGES);
    assert_eq!(
        history.first().and_then(|message| message.content.as_ref()),
        Some(&crate::ai::MessageContent::Text("message 24".into()))
    );
}

#[test]
fn title_is_single_line_and_bounded() {
    let title = question_title(&format!("  first\n{}", "x".repeat(100)));
    assert!(!title.contains('\n'));
    assert_eq!(title.chars().count(), 80);
}

#[test]
fn active_windows_do_not_claim_the_same_persisted_thread() {
    let temp = tempfile::tempdir().expect("temp dir");
    let database = temp.path().join("library.db");
    let first = ConversationLease::new(&database);
    let second = ConversationLease::new(&database);

    assert!(first.switch_claim("thread-1").expect("first claim"));
    assert!(!second.switch_claim("thread-1").expect("second claim"));
    assert!(second.switch_claim("thread-2").expect("independent claim"));

    drop(first);
    let reopened = ConversationLease::new(&database);
    assert!(
        reopened
            .switch_claim("thread-1")
            .expect("released claim can be restored")
    );
}

#[test]
fn temporary_claim_reservation_blocks_and_then_releases_other_windows() {
    let temp = tempfile::tempdir().expect("temp dir");
    let database = temp.path().join("library.db");
    let deleting = ConversationLease::new(&database);
    let other = ConversationLease::new(&database);

    let reservation = deleting
        .reserve_claim("thread-1")
        .expect("reserve thread")
        .expect("unclaimed thread can be reserved");
    assert!(
        !other
            .switch_claim("thread-1")
            .expect("reservation blocks another window")
    );

    drop(reservation);
    assert!(
        other
            .switch_claim("thread-1")
            .expect("dropping reservation releases the thread")
    );
}

#[test]
fn selection_reservation_preserves_the_old_claim_until_promotion() {
    let temp = tempfile::tempdir().expect("temp dir");
    let database = temp.path().join("library.db");
    let selecting = ConversationLease::new(&database);
    let other = ConversationLease::new(&database);

    assert!(selecting.switch_claim("thread-old").expect("old claim"));
    let mut reservation = selecting
        .reserve_claim("thread-new")
        .expect("reserve new thread")
        .expect("new thread is unclaimed");
    assert!(
        other
            .reserve_claim("thread-new")
            .expect("competing reservation")
            .is_none(),
        "a delete cannot pass the selection reservation"
    );
    assert!(
        !other
            .switch_claim("thread-old")
            .expect("old thread remains claimed"),
        "validation must not release the previously selected conversation"
    );

    reservation
        .promote_to_selection(&selecting)
        .expect("promote selection");
    drop(reservation);
    assert!(
        other
            .switch_claim("thread-old")
            .expect("promotion releases old claim")
    );
    assert!(
        other
            .reserve_claim("thread-new")
            .expect("new thread remains claimed")
            .is_none(),
        "dropping a promoted reservation must not release the selected thread"
    );
}

#[test]
fn committed_delete_cleanup_error_cannot_change_success_into_failure() {
    let mut selection = ConversationSelection::Selected("deleted-thread".to_string());
    let session = finalize_committed_delete(&mut selection, true, None, || {
        Err(anyhow::anyhow!("forced claim cleanup failure"))
    });

    assert!(session.is_none());
    assert_eq!(selection, ConversationSelection::Fresh);
}

#[test]
fn fresh_selection_does_not_reuse_a_recent_session() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let conversation = AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
        .expect("create conversation");

    runtime.block_on(async {
        let repository = services.chat();
        let recent = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Recent".to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        assert_eq!(
            conversation.restore(&[]).await.unwrap().unwrap().thread.id,
            recent.id
        );

        conversation.start_new_session().await.unwrap();
        assert_eq!(conversation.selected_thread_id().await, None);
        assert!(conversation.restore(&[]).await.unwrap().is_none());

        let (fresh, history) = conversation
            .thread_and_history(&repository, "Start clean", ChatScope::default())
            .await
            .unwrap();
        assert_ne!(fresh, recent.id);
        assert!(history.is_empty());
        assert_eq!(conversation.selected_thread_id().await, Some(fresh));
    });
}
