//! Tests for persisting and restoring threads, messages and citations.

use super::super::*;
use super::*;

#[test]
pub(crate) fn persists_and_restores_an_independent_window_session() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some("book-1".to_string()),
                title: "Reading notes".to_string(),
                scope: ChatScope::new(["book-1"]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let user = repository
            .append_message(
                &thread.id,
                NewChatMessage::text(ChatRole::User, "What is this chapter about?"),
            )
            .await
            .unwrap();
        let answer = repository
            .append_message(
                &thread.id,
                NewChatMessage {
                    parent_id: Some(user.id.clone()),
                    role: ChatRole::Assistant,
                    content: "An answer".to_string(),
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
            .unwrap();

        assert_eq!(answer.ordinal, 1);
        let restored = repository.session(&thread.id).await.unwrap().unwrap();
        assert_eq!(restored.thread.id, thread.id);
        assert_eq!(restored.thread.scope, thread.scope);
        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.messages[1].citations.len(), 1);
        assert_eq!(restored.messages[1].citations[0].quote, "quoted text");
        assert_eq!(
            restored.messages[1].citations[0].document_revision,
            Revision::new(1)
        );
        assert_eq!(
            restored.messages[1].citations[0].unit_revision,
            Revision::new(1)
        );
        let listed = repository
            .list_threads(ChatWindowKind::Reader, Some("book-1"))
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, thread.id);
        assert_eq!(listed[0].scope, thread.scope);
        assert_eq!(listed[0].window_kind, thread.window_kind);
        assert!(
            listed[0].updated_at >= thread.updated_at,
            "appending messages must not move the thread timestamp backwards"
        );
    });
}

#[test]
pub(crate) fn web_citations_persist_without_a_book_locator() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some("book-1".to_string()),
                title: "Web grounded".to_string(),
                scope: ChatScope::new(["book-1"]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let user = repository
            .append_message(
                &thread.id,
                NewChatMessage::text(ChatRole::User, "最新进展是什么？"),
            )
            .await
            .unwrap();
        let answer = repository
            .append_message(
                &thread.id,
                NewChatMessage {
                    parent_id: Some(user.id.clone()),
                    role: ChatRole::Assistant,
                    content: "来自互联网的回答".to_string(),
                    model: Some("chat-model".to_string()),
                    citations: vec![NewChatCitation {
                        content_unit_id: None,
                        search_chunk_id: None,
                        document_revision: Revision::new(0),
                        unit_revision: Revision::new(0),
                        quote: "verified web snippet".to_string(),
                        locator: DocumentLocator::unit("", ""),
                        source_kind: AgentCitationSourceKind::Web,
                        url: Some("https://example.test/docs".to_string()),
                        source_title: Some("Docs".to_string()),
                    }],
                },
            )
            .await
            .unwrap();
        assert_eq!(answer.citations.len(), 1);
        assert_eq!(
            answer.citations[0].source_kind,
            AgentCitationSourceKind::Web
        );
        assert_eq!(
            answer.citations[0].url.as_deref(),
            Some("https://example.test/docs")
        );
        assert_eq!(answer.citations[0].source_title.as_deref(), Some("Docs"));
        assert!(answer.citations[0].content_unit_id.is_none());

        let restored = repository.session(&thread.id).await.unwrap().unwrap();
        let citation = &restored.messages.last().unwrap().citations[0];
        assert_eq!(citation.source_kind, AgentCitationSourceKind::Web);
        assert_eq!(citation.url.as_deref(), Some("https://example.test/docs"));
        assert_eq!(citation.source_title.as_deref(), Some("Docs"));
        assert!(citation.locator.book_id.is_empty());
        assert!(citation.locator.unit_id.is_empty());
    });
}

#[test]
pub(crate) fn delete_thread_cascades_messages_and_citations_and_is_idempotent() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some("book-1".to_string()),
                title: "Delete me".to_string(),
                scope: ChatScope::new(["book-1"]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let user = repository
            .append_message(
                &thread.id,
                NewChatMessage::text(ChatRole::User, "Delete this conversation"),
            )
            .await
            .unwrap();
        let answer = repository
            .append_message(
                &thread.id,
                NewChatMessage {
                    parent_id: Some(user.id.clone()),
                    role: ChatRole::Assistant,
                    content: "Deleted answer".to_string(),
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
            .unwrap();
        let citation_id = answer.citations[0].id.clone();

        assert!(repository.delete_thread(&thread.id).await.unwrap());
        assert!(repository.thread(&thread.id).await.unwrap().is_none());
        assert!(repository.session(&thread.id).await.unwrap().is_none());

        let conn = db::open_conn(repository.database_path()).unwrap();
        assert!(db::chat_messages::get(&conn, &user.id).unwrap().is_none());
        assert!(db::chat_messages::get(&conn, &answer.id).unwrap().is_none());
        assert!(
            db::chat_citations::get(&conn, &citation_id)
                .unwrap()
                .is_none()
        );
        drop(conn);

        assert!(!repository.delete_thread(&thread.id).await.unwrap());
    });
}

#[test]
pub(crate) fn message_and_citations_roll_back_together() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some("book-1".to_string()),
                title: "Atomic".to_string(),
                scope: ChatScope::new(["book-1"]).unwrap(),
                window_kind: ChatWindowKind::Editor,
            })
            .await
            .unwrap();
        let result = repository
            .append_message(
                &thread.id,
                NewChatMessage {
                    parent_id: None,
                    role: ChatRole::Assistant,
                    content: "Will roll back".to_string(),
                    model: None,
                    citations: vec![NewChatCitation {
                        content_unit_id: Some("unit-1".to_string()),
                        search_chunk_id: Some("missing-chunk".to_string()),
                        document_revision: Revision::new(1),
                        unit_revision: Revision::new(1),
                        quote: "quote".to_string(),
                        locator: DocumentLocator::unit("book-1", "unit-1"),
                        source_kind: AgentCitationSourceKind::Book,
                        url: None,
                        source_title: None,
                    }],
                },
            )
            .await;
        assert!(result.is_err());
        assert!(
            repository
                .session(&thread.id)
                .await
                .unwrap()
                .unwrap()
                .messages
                .is_empty()
        );
    });
}

#[test]
pub(crate) fn reader_scope_cannot_drop_current_book_or_store_foreign_citation() {
    let (_temp, runtime, repository) = repository();
    insert_book_graph(repository.database_path());
    runtime.block_on(async {
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some("book-1".to_string()),
                title: "Scoped".to_string(),
                scope: ChatScope::new(["book-1"]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        assert!(
            repository
                .update_thread(&thread.id, "Scoped", ChatScope::default())
                .await
                .is_err()
        );
        assert!(
            repository
                .append_message(
                    &thread.id,
                    NewChatMessage {
                        parent_id: None,
                        role: ChatRole::Assistant,
                        content: "No".to_string(),
                        model: None,
                        citations: vec![NewChatCitation {
                            content_unit_id: None,
                            search_chunk_id: None,
                            document_revision: Revision::new(1),
                            unit_revision: Revision::new(1),
                            quote: "foreign".to_string(),
                            locator: DocumentLocator::unit("book-2", "unit-x"),
                            source_kind: AgentCitationSourceKind::Book,
                            url: None,
                            source_title: None,
                        }],
                    },
                )
                .await
                .is_err()
        );
    });
}
