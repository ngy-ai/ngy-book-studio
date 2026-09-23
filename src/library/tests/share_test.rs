//! Share-package round trips through the real library.

use super::*;

use crate::{
    ai::translation::{StoredTranslation, TranslationSegment},
    annotations::{AnnotationDraft, AnnotationKind, TextAnchor},
    db::{
        chat_citations::ChatCitation, chat_messages::ChatMessage, chat_threads::ChatThread,
        translations::NewTranslation,
    },
    document::DocumentLocator,
};

/// A selection the sender's own host accepts: the tail of the first chapter's
/// reader body text, so the offsets are those of the reading projection rather
/// than of an edited copy.
fn first_chapter_anchor(library: &LibraryStore, book_id: &str) -> (String, u64, u64, TextAnchor) {
    let record = library.book_record(book_id).unwrap();
    let document = library.document(book_id).unwrap();
    let unit = &document.units[0];
    let opened =
        crate::reader::OpenedBook::open_bytes(library.reader_epub_bytes(book_id).unwrap()).unwrap();
    let response = crate::reader::load_resource(&opened.epub, &opened.spine[0].href).unwrap();
    let html = std::str::from_utf8(&response.bytes).unwrap();
    let text = crate::annotations::reader_body_text(html);
    let start = text.chars().take(3).map(char::len_utf16).sum::<usize>() as u32;
    let end = text.encode_utf16().count() as u32;
    assert!(end > start, "the fixture chapter must have selectable text");
    let anchor = TextAnchor {
        quote: text.chars().skip(3).collect(),
        start,
        end,
    };
    (
        unit.id.clone(),
        record.revision,
        unit.revision.get(),
        anchor,
    )
}

#[test]
fn a_share_package_restores_notes_conversations_and_translations() {
    let temp = tempfile::tempdir().unwrap();
    let mut sender = LibraryStore::load_from(temp.path().join("sender")).unwrap();
    let book = sender.create_book("共享之书", "作者").unwrap();
    let (unit_id, document_revision, unit_revision, anchor) =
        first_chapter_anchor(&sender, &book.id);

    sender
        .create_annotation(
            &book.id,
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

    let stored = StoredTranslation {
        execution_identity: "translation-v2:sender".into(),
        segments: vec![TranslationSegment {
            source: "开始写作……".into(),
            translated: "Start writing…".into(),
        }],
    };
    let conn = crate::db::open_conn(&sender.database_path()).unwrap();
    crate::db::translations::upsert(
        &conn,
        &NewTranslation {
            book_id: book.id.clone(),
            content_unit_id: unit_id.clone(),
            block_id: format!("{unit_id}::h1"),
            ordinal: 1,
            document_revision,
            unit_revision,
            target_language: "zh-Hans".into(),
            source_language: Some("zh".into()),
            model: "sender-model".into(),
            source_text: "开始写作……".into(),
            translated_text: serde_json::to_string(&stored).unwrap(),
            created_at: 10,
            updated_at: 10,
        },
    )
    .unwrap();
    crate::db::chat_threads::insert(
        &conn,
        &ChatThread {
            id: "thread-1".into(),
            book_id: Some(book.id.clone()),
            title: "阅读讨论".into(),
            scope_json: serde_json::to_string(&serde_json::json!({ "book_ids": [book.id] }))
                .unwrap(),
            window_kind: "reader".into(),
            created_at: 20,
            updated_at: 20,
        },
    )
    .unwrap();
    crate::db::chat_messages::insert(
        &conn,
        &ChatMessage {
            id: "message-1".into(),
            thread_id: "thread-1".into(),
            parent_id: None,
            ordinal: 0,
            role: "user".into(),
            content: "这一章讲了什么？".into(),
            model: None,
            created_at: 20,
        },
    )
    .unwrap();
    crate::db::chat_citations::insert(
        &conn,
        &ChatCitation {
            id: "citation-1".into(),
            message_id: "message-1".into(),
            content_unit_id: Some(unit_id.clone()),
            search_chunk_id: None,
            ordinal: 0,
            quote: anchor.quote.clone(),
            document_revision,
            unit_revision,
            locator_json: Some(
                serde_json::to_string(&DocumentLocator::unit(&book.id, &unit_id)).unwrap(),
            ),
            source_kind: "book".into(),
            url: None,
            source_title: None,
            created_at: 20,
        },
    )
    .unwrap();
    drop(conn);

    let target = temp.path().join("shared.ngyshare");
    let summary = sender.export_share_package(&book.id, &target).unwrap();
    assert_eq!(summary.notes, 1);
    assert_eq!(summary.chat_threads, 1);
    assert_eq!(summary.chat_messages, 1);
    assert_eq!(summary.translations, 1);
    assert_eq!(summary.target_language.as_deref(), Some("zh-Hans"));

    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    let outcome = receiver.import_share_package(&target, None, None).unwrap();
    assert!(outcome.added);
    assert_eq!(outcome.notes_imported, 1);
    assert_eq!(outcome.notes_skipped, 0);
    assert_eq!(outcome.translations_imported, 1);
    assert_eq!(outcome.chat_threads, 1);
    assert_eq!(outcome.chat_messages, 1);

    let notes = receiver.list_annotations(&outcome.record.id, None).unwrap();
    assert_eq!(notes.len(), 1, "the shared note must survive");
    assert_eq!(notes[0].kind, AnnotationKind::Highlight);
    assert_eq!(notes[0].anchor, anchor);
    assert!(
        !notes[0].stale,
        "an imported note must carry the receiving revision"
    );
    assert_ne!(
        notes[0].content_unit_id, unit_id,
        "the receiver derives its own unit IDs; the import must remap them"
    );

    let receiver_document = receiver.document(&outcome.record.id).unwrap();
    let conn = crate::db::open_conn(&receiver.database_path()).unwrap();
    let receiver_unit = &receiver_document.units[0];
    let rows = crate::db::translations::list_for_unit(&conn, &receiver_unit.id, "zh-Hans").unwrap();
    assert_eq!(rows.len(), 1, "the shared译文 must survive");
    assert_eq!(rows[0].model, "sender-model");
    assert_eq!(rows[0].source_text, "开始写作……");
    assert_eq!(rows[0].unit_revision, receiver_unit.revision.get());
    let restored: StoredTranslation = serde_json::from_str(&rows[0].translated_text).unwrap();
    assert_eq!(restored.execution_identity, "translation-v2:sender");
    assert_eq!(restored.segments[0].translated, "Start writing…");

    let threads = crate::db::chat_threads::list_for_book(&conn, Some(&outcome.record.id)).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].title, "阅读讨论");
    let messages = crate::db::chat_messages::list_for_thread(&conn, &threads[0].id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "这一章讲了什么？");
    let citations = crate::db::chat_citations::list_for_message(&conn, &messages[0].id).unwrap();
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].quote, anchor.quote);
    assert_eq!(
        citations[0].content_unit_id.as_deref(),
        Some(receiver_unit.id.as_str())
    );
    assert!(
        citations[0].search_chunk_id.is_none(),
        "the receiving search index is rebuilt locally, never imported"
    );

    // Re-importing the same package must not duplicate what already landed.
    let again = receiver.import_share_package(&target, None, None).unwrap();
    assert!(!again.added, "the projection is already in the library");
    assert_eq!(
        receiver
            .list_annotations(&again.record.id, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        crate::db::chat_threads::list_for_book(&conn, Some(&again.record.id))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        crate::db::translations::list_for_unit(&conn, &receiver_unit.id, "zh-Hans")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_package_never_recolors_a_mark_the_receiver_already_made() {
    let temp = tempfile::tempdir().unwrap();
    let mut sender = LibraryStore::load_from(temp.path().join("sender")).unwrap();
    let book = sender.create_book("划线之书", "作者").unwrap();
    let (unit_id, document_revision, unit_revision, anchor) =
        first_chapter_anchor(&sender, &book.id);
    sender
        .create_annotation(
            &book.id,
            &AnnotationDraft {
                content_unit_id: unit_id,
                document_revision,
                unit_revision,
                anchor: anchor.clone(),
                kind: AnnotationKind::Highlight,
                comment: None,
            },
        )
        .unwrap();
    let target = temp.path().join("shared.ngyshare");
    sender.export_share_package(&book.id, &target).unwrap();

    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    let first = receiver.import_share_package(&target, None, None).unwrap();
    assert_eq!(first.notes_imported, 1);

    // The receiver restyles the very same selection, which is one note with a
    // different style here too.
    let receiver_document = receiver.document(&first.record.id).unwrap();
    receiver
        .create_annotation(
            &first.record.id,
            &AnnotationDraft {
                content_unit_id: receiver_document.units[0].id.clone(),
                document_revision: first.record.revision,
                unit_revision: receiver_document.units[0].revision.get(),
                anchor: anchor.clone(),
                kind: AnnotationKind::Underline,
                comment: None,
            },
        )
        .unwrap();

    let again = receiver.import_share_package(&target, None, None).unwrap();
    assert_eq!(again.notes_imported, 0);
    assert_eq!(again.notes_skipped, 1);
    let notes = receiver.list_annotations(&again.record.id, None).unwrap();
    assert_eq!(notes.len(), 1, "the receiver still has exactly one mark");
    assert_eq!(
        notes[0].kind,
        AnnotationKind::Underline,
        "a shared package must not recolor the receiver's own marking"
    );
}

#[test]
fn a_note_whose_offsets_do_not_match_the_receiving_text_is_skipped() {
    let temp = tempfile::tempdir().unwrap();
    let mut sender = LibraryStore::load_from(temp.path().join("sender")).unwrap();
    let book = sender.create_book("错位之书", "作者").unwrap();
    let (unit_id, document_revision, unit_revision, anchor) =
        first_chapter_anchor(&sender, &book.id);
    sender
        .create_annotation(
            &book.id,
            &AnnotationDraft {
                content_unit_id: unit_id,
                document_revision,
                unit_revision,
                anchor: anchor.clone(),
                kind: AnnotationKind::Underline,
                comment: None,
            },
        )
        .unwrap();

    let target = temp.path().join("shared.ngyshare");
    sender.export_share_package(&book.id, &target).unwrap();

    // Rewrite the packaged note to a range the receiving text does not have at
    // those offsets. A share import must never relocate a note by searching for
    // its quote.
    let bytes = std::fs::read(&target).unwrap();
    let mut package = crate::share::read_share_package(&bytes).unwrap();
    package.notes[0].start += 1;
    package.notes[0].end += 1;
    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    let outcome = receiver
        .import_share_package_contents(package, None, None)
        .unwrap();
    assert_eq!(outcome.notes_imported, 0);
    assert_eq!(outcome.notes_skipped, 1);
    assert!(
        receiver
            .list_annotations(&outcome.record.id, None)
            .unwrap()
            .is_empty()
    );
}
