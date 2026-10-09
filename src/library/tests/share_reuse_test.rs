//! Sharing with a friend who already owns the same book.

use super::*;

use crate::{
    annotations::{AnnotationDraft, AnnotationKind},
    db::{chat_citations::ChatCitation, chat_messages::ChatMessage, chat_threads::ChatThread},
    document::{DocumentLocator, TextRange},
};

fn insert_thread(
    conn: &rusqlite::Connection,
    book_id: &str,
    id: &str,
    title: &str,
    created_at: u64,
    updated_at: u64,
) {
    crate::db::chat_threads::insert(
        conn,
        &ChatThread {
            id: id.to_string(),
            book_id: Some(book_id.to_string()),
            title: title.to_string(),
            scope_json: serde_json::to_string(&serde_json::json!({ "book_ids": [book_id] }))
                .unwrap(),
            window_kind: "reader".into(),
            created_at,
            updated_at,
        },
    )
    .unwrap();
}

fn insert_message(
    conn: &rusqlite::Connection,
    thread_id: &str,
    id: &str,
    ordinal: usize,
    content: &str,
    created_at: u64,
) {
    crate::db::chat_messages::insert(
        conn,
        &ChatMessage {
            id: id.to_string(),
            thread_id: thread_id.to_string(),
            parent_id: None,
            ordinal,
            role: "user".into(),
            content: content.to_string(),
            model: None,
            created_at,
        },
    )
    .unwrap();
}

/// Both sides import the same file, then the sender shares. The receiver's copy
/// *is* the sender's own book, so it must be reused rather than duplicated, and
/// a citation that pointed at one block must still point at that block.
#[test]
fn a_receiver_that_owns_the_same_original_reuses_it_and_keeps_block_citations() {
    let temp = tempfile::tempdir().unwrap();
    let epub = epub_source_file(&temp.path().join("source"), "同源之书");
    let mut sender = LibraryStore::load_from(temp.path().join("sender")).unwrap();
    let sender_book = match sender.import(&epub).unwrap() {
        ImportOutcome::Added(record) | ImportOutcome::AlreadyExists(record) => record,
    };
    let (unit_id, document_revision, unit_revision, anchor) =
        first_chapter_anchor(&sender, &sender_book.id);
    sender
        .create_annotation(
            &sender_book.id,
            &AnnotationDraft {
                content_unit_id: unit_id.clone(),
                document_revision,
                unit_revision,
                anchor: anchor.clone(),
                kind: AnnotationKind::Highlight,
                comment: None,
            },
        )
        .unwrap();

    let document = sender.document(&sender_book.id).unwrap();
    let block = document.units[0].document.blocks.first().cloned().unwrap();
    let block_text = block.plain_text();
    let locator = DocumentLocator::text(
        &sender_book.id,
        &unit_id,
        block.id(),
        0,
        block_text.len() as u64,
    );
    let conn = crate::db::open_conn(&sender.database_path()).unwrap();
    insert_thread(&conn, &sender_book.id, "thread-1", "同源讨论", 20, 20);
    insert_message(&conn, "thread-1", "message-1", 0, "这段说的是什么？", 20);
    crate::db::chat_citations::insert(
        &conn,
        &ChatCitation {
            id: "citation-1".into(),
            message_id: "message-1".into(),
            content_unit_id: Some(unit_id.clone()),
            search_chunk_id: None,
            ordinal: 0,
            quote: block_text.clone(),
            document_revision,
            unit_revision,
            locator_json: Some(serde_json::to_string(&locator).unwrap()),
            source_kind: "book".into(),
            url: None,
            source_title: None,
            created_at: 20,
        },
    )
    .unwrap();
    drop(conn);

    let target = temp.path().join("shared.ngyshare");
    sender
        .export_share_package(&sender_book.id, &target)
        .unwrap();

    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    receiver.import(&epub).unwrap();
    let outcome = receiver.import_share_package(&target, None, None).unwrap();

    assert_eq!(
        outcome.book,
        ShareBookOutcome::Reused,
        "the same original must not be imported twice"
    );
    assert_eq!(
        receiver.books().len(),
        1,
        "a reused package must not add a second copy of the same book"
    );
    assert_eq!(outcome.notes_imported, 1);

    let conn = crate::db::open_conn(&receiver.database_path()).unwrap();
    let threads = crate::db::chat_threads::list_for_book(&conn, Some(&outcome.record.id)).unwrap();
    assert_eq!(threads.len(), 1);
    let messages = crate::db::chat_messages::list_for_thread(&conn, &threads[0].id).unwrap();
    let citations = crate::db::chat_citations::list_for_message(&conn, &messages[0].id).unwrap();
    assert_eq!(citations.len(), 1);
    let stored: DocumentLocator =
        serde_json::from_str(citations[0].locator_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        stored.block_id.as_deref(),
        Some(block.id()),
        "a reused book keeps the sender's block-level citation"
    );
    assert_eq!(
        stored.text_range,
        Some(TextRange::new(0, block_text.len() as u64))
    );
}

/// Replying in an older conversation reorders the sender's thread list. The
/// next export must still be recognised as the same conversation, and only the
/// new turn may be appended.
#[test]
fn re_sharing_a_conversation_after_a_new_reply_appends_only_the_new_turns() {
    let temp = tempfile::tempdir().unwrap();
    let mut sender = LibraryStore::load_from(temp.path().join("sender")).unwrap();
    let book = sender.create_book("多线程之书", "作者").unwrap();
    let conn = crate::db::open_conn(&sender.database_path()).unwrap();
    insert_thread(&conn, &book.id, "thread-a", "先问的", 100, 100);
    insert_message(&conn, "thread-a", "message-a0", 0, "第一个问题", 100);
    insert_thread(&conn, &book.id, "thread-b", "后问的", 200, 200);
    insert_message(&conn, "thread-b", "message-b0", 0, "第二个问题", 200);
    drop(conn);

    let first = temp.path().join("first.ngyshare");
    sender.export_share_package(&book.id, &first).unwrap();

    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    let imported = receiver.import_share_package(&first, None, None).unwrap();
    assert_eq!(imported.chat_threads, 2);
    assert_eq!(imported.chat_messages, 2);

    // The sender replies in the older conversation, which moves it to the top
    // of the sender's activity-ordered list.
    let conn = crate::db::open_conn(&sender.database_path()).unwrap();
    insert_message(&conn, "thread-a", "message-a1", 1, "追加的回答", 300);
    crate::db::chat_threads::touch(&conn, "thread-a", 300).unwrap();
    drop(conn);
    let second = temp.path().join("second.ngyshare");
    sender.export_share_package(&book.id, &second).unwrap();

    let again = receiver.import_share_package(&second, None, None).unwrap();
    assert_eq!(again.chat_threads, 0, "the conversation is not a new one");
    assert_eq!(again.chat_messages, 1, "only the reply is new");

    let conn = crate::db::open_conn(&receiver.database_path()).unwrap();
    let threads = crate::db::chat_threads::list_for_book(&conn, Some(&again.record.id)).unwrap();
    assert_eq!(threads.len(), 2, "no duplicate conversations");
    let total = threads
        .iter()
        .map(|thread| {
            crate::db::chat_messages::list_for_thread(&conn, &thread.id)
                .unwrap()
                .len()
        })
        .sum::<usize>();
    assert_eq!(total, 3);
}
