//! Share-package round trips through the real library.

use super::*;

use crate::{
    annotations::{AnnotationDraft, AnnotationKind},
    db::{
        chat_citations::ChatCitation, chat_messages::ChatMessage, chat_threads::ChatThread,
        translations::NewTranslation,
    },
    document::DocumentLocator,
    share::ShareTranslationStamp,
};

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
            translated_text: stored_translation_payload("translation-v2:sender", "Start writing…"),
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
    assert_eq!(outcome.book, ShareBookOutcome::Added);
    assert_eq!(outcome.sections_note, None);
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
    let restored: crate::ai::translation::StoredTranslation =
        serde_json::from_str(&rows[0].translated_text).unwrap();
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

    // Re-importing the same package changes nothing a second time.
    let again = receiver.import_share_package(&target, None, None).unwrap();
    assert_eq!(again.book, ShareBookOutcome::AlreadyImported);
    assert_eq!(again.chat_threads, 0, "the conversation is already here");
    assert_eq!(again.chat_messages, 0);
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

#[test]
fn a_package_whose_chapter_count_disagrees_applies_no_section() {
    let temp = tempfile::tempdir().unwrap();
    let epub = epub_source_file(&temp.path().join("source"), "章数之书");
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
                content_unit_id: unit_id,
                document_revision,
                unit_revision,
                anchor,
                kind: AnnotationKind::Highlight,
                comment: None,
            },
        )
        .unwrap();
    let target = temp.path().join("shared.ngyshare");
    let summary = sender
        .export_share_package(&sender_book.id, &target)
        .unwrap();
    assert_eq!(summary.notes, 1);

    // The receiver owns the very same original and then edits it. Sections
    // address chapters by position, and the positions moved.
    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    let receiver_book = match receiver.import(&epub).unwrap() {
        ImportOutcome::Added(record) | ImportOutcome::AlreadyExists(record) => record,
    };
    assert_eq!(
        receiver_book.id, sender_book.id,
        "the same original is the same book"
    );
    let document = receiver.document(&receiver_book.id).unwrap();
    let mut editor = DocumentEditor::new(document).unwrap();
    editor
        .add_unit(NewContentUnit::html_chapter("新增章节", 1))
        .unwrap();
    receiver.apply_document(editor.into_document()).unwrap();

    let outcome = receiver.import_share_package(&target, None, None).unwrap();
    assert_eq!(outcome.book, ShareBookOutcome::Reused);
    let note = outcome.sections_note.clone().unwrap_or_default();
    assert!(note.contains("章节对不上"), "unexpected note: {note}");
    assert_eq!(outcome.notes_imported, 0);
    assert_eq!(outcome.translations_imported, 0);
    assert!(
        receiver
            .list_annotations(&outcome.record.id, None)
            .unwrap()
            .is_empty()
    );
}

/// A sender library whose first chapter carries machine译文 in each language.
fn sender_with_translations(root: &Path, languages: &[&str]) -> PathBuf {
    let mut library = LibraryStore::load_from(root.join("sender")).unwrap();
    let book = library.create_book("译文之书", "作者").unwrap();
    let (unit_id, document_revision, unit_revision, _) = first_chapter_anchor(&library, &book.id);
    let conn = crate::db::open_conn(&library.database_path()).unwrap();
    for language in languages {
        crate::db::translations::upsert(
            &conn,
            &NewTranslation {
                book_id: book.id.clone(),
                content_unit_id: unit_id.clone(),
                block_id: format!("{unit_id}::h1"),
                ordinal: 0,
                document_revision,
                unit_revision,
                target_language: (*language).to_string(),
                source_language: Some("zh".into()),
                model: "sender-model".into(),
                source_text: "开始写作……".into(),
                translated_text: stored_translation_payload(
                    "translation-v2:sender",
                    "Start writing…",
                ),
                created_at: 10,
                updated_at: 10,
            },
        )
        .unwrap();
    }
    drop(conn);
    let target = root.join(format!("shared-{}.ngyshare", languages.join("-")));
    library.export_share_package(&book.id, &target).unwrap();
    target
}

#[test]
fn shared_translations_adopt_the_receiving_language_only_when_the_package_has_one() {
    let temp = tempfile::tempdir().unwrap();
    let stamp = ShareTranslationStamp {
        model: "receiver-model".into(),
        execution_identity: "translation-v2:receiver".into(),
        target_language: Some("en".into()),
    };

    // One shared language adopts the receiver's reading language, so the
    // receiver sees the friend's译文 without running the same engine.
    let single = sender_with_translations(&temp.path().join("one"), &["zh-Hans"]);
    let mut receiver = LibraryStore::load_from(temp.path().join("one-receiver")).unwrap();
    let outcome = receiver
        .import_share_package(&single, None, Some(&stamp))
        .unwrap();
    assert_eq!(outcome.translations_imported, 1);
    assert_eq!(outcome.translations_language.as_deref(), Some("en"));
    let document = receiver.document(&outcome.record.id).unwrap();
    let conn = crate::db::open_conn(&receiver.database_path()).unwrap();
    let rows = crate::db::translations::list_for_unit(&conn, &document.units[0].id, "en").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model, "receiver-model");
    let adopted: crate::ai::translation::StoredTranslation =
        serde_json::from_str(&rows[0].translated_text).unwrap();
    assert_eq!(adopted.execution_identity, "translation-v2:receiver");
    drop(conn);

    // Several shared languages must not collapse onto one translation scope
    // key: that would overwrite one friend's译文 with the other's.
    let both = sender_with_translations(&temp.path().join("two"), &["zh-Hans", "ja"]);
    let mut receiver = LibraryStore::load_from(temp.path().join("two-receiver")).unwrap();
    let outcome = receiver
        .import_share_package(&both, None, Some(&stamp))
        .unwrap();
    assert_eq!(outcome.translations_imported, 2);
    assert_eq!(
        outcome.translations_language, None,
        "a package with several languages has no single one to report"
    );
    let document = receiver.document(&outcome.record.id).unwrap();
    let conn = crate::db::open_conn(&receiver.database_path()).unwrap();
    let counted = ["zh-Hans", "ja", "en"]
        .into_iter()
        .map(|language| {
            (
                language,
                crate::db::translations::list_for_unit(&conn, &document.units[0].id, language)
                    .unwrap()
                    .len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        counted,
        vec![("zh-Hans", 1), ("ja", 1), ("en", 0)],
        "each shared language stays its own row"
    );
}

#[test]
fn a_shared_translation_never_lands_on_a_block_with_different_text() {
    let temp = tempfile::tempdir().unwrap();
    let target = sender_with_translations(&temp.path().join("sender"), &["zh-Hans"]);
    let bytes = std::fs::read(&target).unwrap();

    let mut receiver = LibraryStore::load_from(temp.path().join("receiver")).unwrap();
    let first = receiver.import_share_package(&target, None, None).unwrap();
    assert_eq!(first.translations_imported, 1);

    let document = receiver.document(&first.record.id).unwrap();
    let unit_id = document.units[0].id.clone();
    let conn = crate::db::open_conn(&receiver.database_path()).unwrap();
    let before = crate::db::translations::list_for_unit(&conn, &unit_id, "zh-Hans").unwrap();
    assert_eq!(before.len(), 1);

    // The receiving block at `h1` carries "开始写作……". A row aimed at another
    // position, a row carrying another text, and a row past the chapter's last
    // block all describe a paragraph this library does not have; none of them
    // may be imported as current, and none may displace the row already here.
    let variants = [
        (0usize, "开始写作……"),
        (1usize, "完全不同的一段文字"),
        (99usize, "开始写作……"),
    ];
    for (block_index, source_text) in variants {
        let mut package = crate::share::read_share_package(&bytes).unwrap();
        package.translations[0].block_index = block_index;
        package.translations[0].source_text = source_text.to_string();
        let outcome = receiver
            .import_share_package_contents(package, None, None)
            .unwrap();
        assert_eq!(
            (outcome.translations_imported, outcome.translations_skipped),
            (0, 1),
            "block_index {block_index} with source {source_text:?} must be skipped"
        );
    }

    let after = crate::db::translations::list_for_unit(&conn, &unit_id, "zh-Hans").unwrap();
    assert_eq!(
        after.len(),
        1,
        "a mismatched row must not displace the receiver's own译文"
    );
    assert_eq!(after[0].block_id, before[0].block_id);
    assert_eq!(after[0].source_text, before[0].source_text);
    assert_eq!(after[0].translated_text, before[0].translated_text);
}
