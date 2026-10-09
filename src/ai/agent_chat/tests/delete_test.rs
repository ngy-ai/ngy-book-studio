//! Tests for removing one stored turn through the conversation API.

use super::super::*;
use super::*;

async fn append_user(
    repository: &ChatRepository,
    thread_id: &str,
    parent_id: Option<&str>,
    content: &str,
) -> StoredChatMessage {
    let mut draft = NewChatMessage::text(ChatRole::User, content);
    draft.parent_id = parent_id.map(str::to_string);
    repository.append_message(thread_id, draft).await.unwrap()
}

async fn append_answer(
    repository: &ChatRepository,
    thread_id: &str,
    question: &StoredChatMessage,
    content: &str,
) -> StoredChatMessage {
    repository
        .append_message(
            thread_id,
            NewChatMessage {
                parent_id: Some(question.id.clone()),
                role: ChatRole::Assistant,
                content: content.to_string(),
                model: None,
                citations: Vec::new(),
            },
        )
        .await
        .unwrap()
}

#[test]
fn deleting_a_turn_reloads_the_conversation_without_the_removed_pair() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let book = library.create_book("Turns", "").expect("create book").id;
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some(book.clone()),
                title: "Turns".to_string(),
                scope: ChatScope::new([book.clone()]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let first = append_user(&repository, &thread.id, None, "first question").await;
        let first_answer = append_answer(&repository, &thread.id, &first, "first answer").await;
        let second = append_user(
            &repository,
            &thread.id,
            Some(&first_answer.id),
            "second question",
        )
        .await;
        append_answer(&repository, &thread.id, &second, "second answer").await;

        let conversation = AgentConversation::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book.clone()),
        )
        .unwrap();
        let scope = vec![book.clone()];
        conversation
            .select_session(&thread.id, &scope)
            .await
            .unwrap();

        let session = conversation
            .delete_turn(&first.id, &scope)
            .await
            .unwrap()
            .expect("the selected conversation reloads after the deletion");
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].content, "second question");
        assert_eq!(session.messages[1].content, "second answer");

        // The anchor is gone now; a repeated request must not write anything.
        assert!(
            conversation
                .delete_turn(&first.id, &scope)
                .await
                .unwrap()
                .is_none()
        );
        let stored = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(stored.messages.len(), 2);
    });
}

#[test]
fn deleting_a_turn_is_rejected_outside_the_host_scope_and_without_a_selection() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let first_book = library.create_book("First", "").expect("create book").id;
    let second_book = library.create_book("Second", "").expect("create book").id;
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    runtime.block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: None,
                title: "Scoped".to_string(),
                scope: ChatScope::new([first_book.clone()]).unwrap(),
                window_kind: ChatWindowKind::Library,
            })
            .await
            .unwrap();
        let question = append_user(&repository, &thread.id, None, "question").await;

        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        conversation
            .select_session(&thread.id, std::slice::from_ref(&first_book))
            .await
            .unwrap();

        let error = conversation
            .delete_turn(&question.id, std::slice::from_ref(&second_book))
            .await
            .expect_err("a narrowed host scope must refuse the deletion");
        assert!(error.to_string().contains("scope"));
        let stored = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(stored.messages.len(), 1, "nothing may be deleted");

        // A conversation without a selection cannot name a thread to edit.
        let fresh =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
        assert!(
            fresh
                .delete_turn(&question.id, std::slice::from_ref(&first_book))
                .await
                .unwrap()
                .is_none()
        );
        let stored = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(stored.messages.len(), 1);
    });
}
