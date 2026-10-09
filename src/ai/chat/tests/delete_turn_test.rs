//! Tests for removing one stored question together with its answer.

use super::super::*;
use super::*;

async fn create_thread(repository: &ChatRepository, title: &str) -> ChatThread {
    repository
        .create_thread(NewChatThread {
            primary_book_id: Some("book-1".to_string()),
            title: title.to_string(),
            scope: ChatScope::new(["book-1"]).unwrap(),
            window_kind: ChatWindowKind::Reader,
        })
        .await
        .unwrap()
}

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
                model: Some("chat-model".to_string()),
                citations: vec![NewChatCitation {
                    content_unit_id: Some("unit-1".to_string()),
                    search_chunk_id: Some("chunk-1".to_string()),
                    document_revision: Revision::new(1),
                    unit_revision: Revision::new(1),
                    quote: "quoted text".to_string(),
                    locator: DocumentLocator::unit("book-1", "unit-1"),
                    source_kind: AgentCitationSourceKind::Book,
                    url: None,
                    source_title: None,
                }],
            },
        )
        .await
        .unwrap()
}

#[test]
pub(crate) fn deleting_a_middle_turn_keeps_the_later_turns_contiguous() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = create_thread(&repository, "Turn deletion").await;
        let first = append_user(&repository, &thread.id, None, "first question").await;
        let first_answer = append_answer(&repository, &thread.id, &first, "first answer").await;
        let second = append_user(
            &repository,
            &thread.id,
            Some(&first_answer.id),
            "second question",
        )
        .await;
        let second_answer = append_answer(&repository, &thread.id, &second, "second answer").await;

        assert!(
            repository
                .delete_question_and_answer(&thread.id, &first.id)
                .await
                .unwrap()
        );

        let session = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(
            session
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            ["second question", "second answer"]
        );
        // Ordinal stays a contiguous sequence: the next append takes len().
        assert_eq!(session.messages[0].ordinal, 0);
        assert_eq!(session.messages[1].ordinal, 1);
        // The removed pair was the thread root, so the surviving question is
        // re-parented onto the root instead of pointing at a deleted answer.
        assert_eq!(session.messages[0].parent_id, None);
        assert_eq!(
            session.messages[1].parent_id.as_deref(),
            Some(second.id.as_str())
        );

        let conn = db::open_conn(repository.database_path()).unwrap();
        assert!(
            db::chat_citations::list_for_message(&conn, &first_answer.id)
                .unwrap()
                .is_empty(),
            "the removed answer must not leave dangling citations"
        );

        let third = append_user(
            &repository,
            &thread.id,
            Some(&second_answer.id),
            "third question",
        )
        .await;
        assert_eq!(third.ordinal, 2, "a later append reuses the freed ordinal");
    });
}

#[test]
pub(crate) fn deleting_the_last_turn_leaves_the_earlier_turns_untouched() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = create_thread(&repository, "Last turn").await;
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

        assert!(
            repository
                .delete_question_and_answer(&thread.id, &second.id)
                .await
                .unwrap()
        );

        let session = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].id, first.id);
        assert_eq!(session.messages[1].id, first_answer.id);
        assert_eq!(session.messages[0].ordinal, 0);
        assert_eq!(session.messages[1].ordinal, 1);
        assert_eq!(
            session.messages[1].parent_id.as_deref(),
            Some(first.id.as_str())
        );
    });
}

#[test]
pub(crate) fn a_question_without_a_stored_answer_is_removed_alone() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = create_thread(&repository, "Failed turn").await;
        let failed = append_user(&repository, &thread.id, None, "failed question").await;
        let next = append_user(&repository, &thread.id, Some(&failed.id), "next question").await;

        assert!(
            repository
                .delete_question_and_answer(&thread.id, &failed.id)
                .await
                .unwrap()
        );

        let session = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].id, next.id);
        assert_eq!(session.messages[0].ordinal, 0);
        assert_eq!(session.messages[0].parent_id, None);
    });
}

#[test]
pub(crate) fn answers_and_stale_anchors_are_rejected_without_writing() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = create_thread(&repository, "Anchors").await;
        let question = append_user(&repository, &thread.id, None, "question").await;
        let answer = append_answer(&repository, &thread.id, &question, "answer").await;
        let other = create_thread(&repository, "Other").await;

        assert!(
            !repository
                .delete_question_and_answer(&thread.id, &answer.id)
                .await
                .unwrap(),
            "an answer is not a deletable turn anchor"
        );
        assert!(
            !repository
                .delete_question_and_answer(&thread.id, "chat-message-missing")
                .await
                .unwrap()
        );
        assert!(
            !repository
                .delete_question_and_answer(&other.id, &question.id)
                .await
                .unwrap(),
            "an anchor from another thread must never delete rows"
        );

        let session = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].id, question.id);
        assert_eq!(session.messages[1].id, answer.id);
    });
}
