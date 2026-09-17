//! Tests for reading progress and its ordering rules.

use super::*;

#[test]
fn stale_reader_clone_persists_unit_progress_against_the_current_revision() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    let mut writer = LibraryStore::load_from(data_dir).unwrap();
    let book = writer.create_book("并发阅读进度", "作者").unwrap();
    let unit_id = writer.document(&book.id).unwrap().units[0].id.clone();
    let mut stale_unit_reader = writer.clone();

    let updated = writer
        .update_content_unit_source(&book.id, &unit_id, "<h1>新版本</h1><p>正文</p>")
        .unwrap();
    assert!(updated.revision > book.revision);

    stale_unit_reader
        .update_progress_at(&book.id, 0, &unit_id)
        .unwrap();
    let conn = db::open_conn(writer.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(progress.source_revision, updated.revision);
    assert_eq!(progress.content_unit_id.as_deref(), Some(unit_id.as_str()));
}

#[test]
fn stale_reader_unit_progress_resolves_the_current_ordinal_after_reorder() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let (book, first_unit_id, _) = create_two_unit_book(&mut library);
    let incarnation = library.progress_incarnation(&book.id).unwrap();

    let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
    editor.move_unit(&first_unit_id, 1).unwrap();
    library.apply_document(editor.into_document()).unwrap();

    assert!(
        library
            .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 10)
            .unwrap()
    );
    let conn = db::open_conn(library.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(
        progress.content_unit_id.as_deref(),
        Some(first_unit_id.as_str())
    );
    assert_eq!(progress.spine_index, 1);
    let locator: DocumentLocator = serde_json::from_str(&progress.locator_json).unwrap();
    assert_eq!(locator.unit_id, first_unit_id);
    assert_eq!(library.book_record(&book.id).unwrap().last_spine, 1);
}

#[test]
fn document_reorder_preserves_stable_progress_before_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(data_dir.clone()).unwrap();
    let (book, first_unit_id, _) = create_two_unit_book(&mut library);
    library
        .update_progress_at(&book.id, 0, &first_unit_id)
        .unwrap();

    let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
    editor.move_unit(&first_unit_id, 1).unwrap();
    library.apply_document(editor.into_document()).unwrap();

    let conn = db::open_conn(library.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(
        progress.content_unit_id.as_deref(),
        Some(first_unit_id.as_str())
    );
    assert_eq!(progress.spine_index, 1);
    drop(conn);
    drop(library);

    let reopened = LibraryStore::load_from(data_dir).unwrap();
    assert_eq!(reopened.book_record(&book.id).unwrap().last_spine, 1);
}

#[test]
fn deleted_progress_unit_falls_back_to_a_valid_current_unit() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
    library
        .update_progress_at(&book.id, 1, &second_unit_id)
        .unwrap();

    let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
    editor.remove_unit(&second_unit_id).unwrap();
    library.apply_document(editor.into_document()).unwrap();

    let conn = db::open_conn(library.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(
        progress.content_unit_id.as_deref(),
        Some(first_unit_id.as_str())
    );
    assert_eq!(progress.spine_index, 0);
    assert_eq!(library.book_record(&book.id).unwrap().last_spine, 0);
}

#[test]
fn older_cross_window_progress_action_cannot_overwrite_a_newer_one() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
    let incarnation = library.progress_incarnation(&book.id).unwrap();

    assert!(
        library
            .update_progress_at_ordered(&book.id, 0, &second_unit_id, incarnation, 10)
            .unwrap()
    );
    // Repeating the same position is still a newer action and must advance
    // the guard; otherwise an in-flight action from another window can
    // arrive between the two sequence values and overwrite it.
    assert!(
        library
            .update_progress_at_ordered(&book.id, 0, &second_unit_id, incarnation, 30)
            .unwrap()
    );
    assert!(
        !library
            .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 20)
            .unwrap()
    );
    assert!(
        !library
            .update_progress_at_ordered(&book.id, 0, "missing-unit", incarnation, 50)
            .unwrap()
    );
    assert!(
        !library
            .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 40)
            .unwrap()
    );

    let conn = db::open_conn(library.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(
        progress.content_unit_id.as_deref(),
        Some(second_unit_id.as_str())
    );
    assert_eq!(progress.spine_index, 1);
    assert_eq!(library.book_record(&book.id).unwrap().last_spine, 1);
}

#[test]
fn failed_newer_progress_does_not_block_an_older_success_or_a_later_retry() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
    let incarnation = library.progress_incarnation(&book.id).unwrap();

    assert!(
        library
            .update_progress_at_ordered(&book.id, 1, &second_unit_id, incarnation, 10)
            .unwrap()
    );
    let conn = db::open_conn(library.database_path()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_progress_update
             BEFORE UPDATE ON progress
             BEGIN
                 SELECT RAISE(ABORT, 'test progress update failure');
             END;",
    )
    .unwrap();
    drop(conn);

    assert!(
        library
            .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 30)
            .is_err()
    );
    assert_eq!(
        library
            .progress_write_sequences
            .get(&(book.id.clone(), incarnation)),
        Some(&10)
    );

    let conn = db::open_conn(library.database_path()).unwrap();
    conn.execute_batch("DROP TRIGGER fail_progress_update")
        .unwrap();
    drop(conn);
    assert!(
        library
            .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 20)
            .unwrap()
    );
    assert!(
        library
            .update_progress_at_ordered(&book.id, 1, &second_unit_id, incarnation, 40)
            .unwrap()
    );

    let conn = db::open_conn(library.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(
        progress.content_unit_id.as_deref(),
        Some(second_unit_id.as_str())
    );
    assert_eq!(progress.spine_index, 1);
    assert_eq!(
        library
            .progress_write_sequences
            .get(&(book.id.clone(), incarnation)),
        Some(&40)
    );
}

#[test]
fn deleted_and_reimported_book_rejects_the_old_reader_incarnation() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
    let old_incarnation = library.progress_incarnation(&book.id).unwrap();
    let document = library.document(&book.id).unwrap();
    let source_bytes = library.source_bytes(&book.id).unwrap();

    library.remove_book(&book.id).unwrap();
    let record = library
        .persist_document(
            ImportedBook {
                document,
                assets: Vec::new(),
            },
            &source_bytes,
            "application/epub+zip",
            Some("reimported.epub".to_string()),
            "normalized",
            None,
        )
        .unwrap();
    assert!(matches!(
        library.finish_committed_import(record),
        ImportOutcome::Added(_)
    ));
    let new_incarnation = library.progress_incarnation(&book.id).unwrap();
    assert_ne!(new_incarnation, old_incarnation);

    assert!(
        !library
            .update_progress_at_ordered(&book.id, 1, &second_unit_id, old_incarnation, 100,)
            .unwrap()
    );
    let conn = db::open_conn(library.database_path()).unwrap();
    let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
    assert_eq!(
        progress.content_unit_id.as_deref(),
        Some(first_unit_id.as_str())
    );
    assert_eq!(progress.spine_index, 0);
    drop(conn);

    // Sequence watermarks are scoped by incarnation, so the new reader is
    // not blocked even by a numerically lower action in this direct test.
    assert!(
        library
            .update_progress_at_ordered(&book.id, 1, &second_unit_id, new_incarnation, 1,)
            .unwrap()
    );
    assert_eq!(library.book_record(&book.id).unwrap().last_spine, 1);
}

#[test]
fn created_books_keep_only_the_current_normalized_source() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("版本收敛", "作者").unwrap();
    let unit_id = library.document(&book.id).unwrap().units[0].id.clone();

    library
        .update_content_unit_source(&book.id, &unit_id, "<h1>第二版</h1><p>正文二</p>")
        .unwrap();
    let latest = library
        .update_content_unit_source(&book.id, &unit_id, "<h1>第三版</h1><p>正文三</p>")
        .unwrap();

    let conn = db::open_conn(library.database_path()).unwrap();
    let sources = db::book_sources::list_for_book(&conn, &book.id).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].revision, latest.revision);
    assert_eq!(sources[0].source_kind, "normalized");
}
