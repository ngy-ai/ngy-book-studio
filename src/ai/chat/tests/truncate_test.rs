//! Tests for rewinding a stored thread before a replacement question.

use super::super::*;
use super::*;

fn citation() -> NewChatCitation {
    NewChatCitation {
        content_unit_id: Some("unit-1".to_string()),
        search_chunk_id: Some("chunk-1".to_string()),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        quote: "quoted text".to_string(),
        locator: DocumentLocator::unit("book-1", "unit-1"),
        source_kind: AgentCitationSourceKind::Book,
        url: None,
        source_title: None,
    }
}

async fn seed_thread(repository: &ChatRepository) -> (String, String, String) {
    let thread = repository
        .create_thread(NewChatThread {
            primary_book_id: Some("book-1".to_string()),
            title: "Rewritten turn".to_string(),
            scope: ChatScope::new(["book-1"]).unwrap(),
            window_kind: ChatWindowKind::Reader,
        })
        .await
        .unwrap();
    let user = repository
        .append_message(
            &thread.id,
            NewChatMessage::text(ChatRole::User, "first question"),
        )
        .await
        .unwrap();
    let answer = repository
        .append_message(
            &thread.id,
            NewChatMessage {
                parent_id: Some(user.id.clone()),
                role: ChatRole::Assistant,
                content: "first answer".to_string(),
                model: Some("chat-model".to_string()),
                citations: vec![citation()],
            },
        )
        .await
        .unwrap();
    (thread.id, user.id, answer.id)
}

#[test]
pub(crate) fn truncating_from_a_question_removes_it_its_answer_and_citations() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let (thread_id, user_id, answer_id) = seed_thread(&repository).await;
        let follow_up = repository
            .append_message(
                &thread_id,
                NewChatMessage::text(ChatRole::User, "second question"),
            )
            .await
            .unwrap();
        let last_answer = repository
            .append_message(
                &thread_id,
                NewChatMessage {
                    parent_id: Some(follow_up.id.clone()),
                    role: ChatRole::Assistant,
                    content: "second answer".to_string(),
                    model: Some("chat-model".to_string()),
                    citations: vec![citation()],
                },
            )
            .await
            .unwrap();

        let cutoff = repository
            .truncate_from(&thread_id, &user_id)
            .await
            .unwrap();
        assert_eq!(cutoff, Some(0));

        let session = repository.session(&thread_id).await.unwrap().unwrap();
        assert!(
            session.messages.is_empty(),
            "the question anchor and every later message must be gone: {:?}",
            session
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>()
        );
        // The deleted messages' citations went with them, so a later append
        // reusing the same chunk cannot resurrect a dangling row.
        let conn = db::open_conn(repository.database_path()).unwrap();
        assert!(
            db::chat_citations::list_for_message(&conn, &answer_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            db::chat_citations::list_for_message(&conn, &last_answer.id)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
pub(crate) fn truncating_from_an_answer_keeps_everything_before_it() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let (thread_id, user_id, answer_id) = seed_thread(&repository).await;

        let cutoff = repository
            .truncate_from(&thread_id, &answer_id)
            .await
            .unwrap();
        assert_eq!(cutoff, Some(1));

        let session = repository.session(&thread_id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].id, user_id);
        assert_eq!(session.messages[0].content, "first question");

        // The replacement answer extends the kept question, exactly as the
        // backend builds it after a rewind.
        let replacement = repository
            .append_message(
                &thread_id,
                NewChatMessage {
                    parent_id: Some(user_id),
                    role: ChatRole::Assistant,
                    content: "replacement answer".to_string(),
                    model: Some("chat-model".to_string()),
                    citations: vec![citation()],
                },
            )
            .await
            .unwrap();
        assert_eq!(replacement.ordinal, 1);
        let session = repository.session(&thread_id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[1].content, "replacement answer");
    });
}

#[test]
pub(crate) fn a_stale_or_foreign_anchor_is_rejected_without_touching_the_thread() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let (thread_id, user_id, _) = seed_thread(&repository).await;
        let other = repository
            .create_thread(NewChatThread {
                primary_book_id: Some("book-1".to_string()),
                title: "Other".to_string(),
                scope: ChatScope::new(["book-1"]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();

        assert_eq!(
            repository
                .truncate_from(&thread_id, "chat-message-missing")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            repository.truncate_from(&other.id, &user_id).await.unwrap(),
            None,
            "an anchor from another thread must never be truncated"
        );

        let session = repository.session(&thread_id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].id, user_id);
    });
}

#[test]
pub(crate) fn appended_messages_after_a_truncate_keep_unique_ordinals() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let (thread_id, user_id, _) = seed_thread(&repository).await;
        let cutoff = repository
            .truncate_from(&thread_id, &user_id)
            .await
            .unwrap();
        assert_eq!(cutoff, Some(0));

        let replacement = repository
            .append_message(
                &thread_id,
                NewChatMessage::text(ChatRole::User, "edited question"),
            )
            .await
            .unwrap();
        assert_eq!(replacement.ordinal, 0);
        let answer = repository
            .append_message(
                &thread_id,
                NewChatMessage {
                    parent_id: Some(replacement.id.clone()),
                    role: ChatRole::Assistant,
                    content: "new answer".to_string(),
                    model: Some("chat-model".to_string()),
                    citations: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(answer.ordinal, 1);

        let session = repository.session(&thread_id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].content, "edited question");
    });
}
