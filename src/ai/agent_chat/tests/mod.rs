//! Tests for snapshot authorization and the session claim bookkeeping.

use super::*;

#[test]
fn frozen_selection_is_authorized_from_current_library_metadata() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let record = library
        .create_book("Selection book", "Author")
        .expect("create book");
    let document = library.document(&record.id).expect("load document");
    let unit = document.units.first().expect("default chapter");
    let snapshot = SelectionSnapshot::capture(
        document.id.clone(),
        unit.id.clone(),
        23,
        "unsaved selected words",
    )
    .expect("capture selection");
    let scope = ChatScope::new([document.id.clone()]).expect("scope");

    let authorized = authorize_selection_snapshots(&library, &scope, vec![snapshot])
        .expect("authorize selection")
        .pop()
        .unwrap();
    let citation = authorized.citation().expect("selection citation");
    assert!(citation.citation_id.starts_with("selection:"));
    assert_eq!(citation.book_id, document.id);
    assert_eq!(citation.book_title, document.title);
    assert_eq!(citation.unit_id, unit.id);
    assert_eq!(citation.unit_title, unit.title);
    assert_eq!(citation.document_revision, document.revision);
    assert_eq!(citation.unit_revision, unit.revision);
    assert_eq!(citation.quote, "unsaved selected words");
    assert_eq!(citation.locator.source, unit.source_locator);
}

#[test]
fn frozen_selection_authorization_rejects_out_of_scope_or_stale_units() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let record = library
        .create_book("Selection book", "Author")
        .expect("create book");
    let document = library.document(&record.id).expect("load document");
    let unit_id = document.units[0].id.clone();
    let snapshot = SelectionSnapshot::capture(&document.id, &unit_id, 1, "selected").unwrap();

    let out_of_scope = ChatScope::new(["some-other-book"]).unwrap();
    assert!(
        authorize_selection_snapshots(&library, &out_of_scope, vec![snapshot.clone()])
            .unwrap_err()
            .to_string()
            .contains("outside the host-authorized scope")
    );

    let scope = ChatScope::new([document.id.clone()]).unwrap();
    let stale = SelectionSnapshot::capture(&document.id, "missing-unit", 1, "selected").unwrap();
    assert!(
        authorize_selection_snapshots(&library, &scope, vec![stale])
            .unwrap_err()
            .to_string()
            .contains("content unit no longer exists")
    );
}

#[test]
fn frozen_selection_authorization_rejects_forged_and_preview_only_sources() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let record = library
        .create_book("Selection book", "Author")
        .expect("create book");
    let document = library.document(&record.id).expect("load document");
    let unit = document.units.first().expect("default chapter");
    let scope = ChatScope::new([document.id.clone()]).expect("scope");

    let forged = SelectionSnapshot::capture(&document.id, &unit.id, 1, "selected")
        .unwrap()
        .with_locator(
            DocumentLocator::unit(&document.id, &unit.id).with_source(SourceLocator::pdf_page(1)),
        )
        .unwrap();
    let error = authorize_selection_snapshots(&library, &scope, vec![forged])
        .expect_err("cross-format source must be rejected");
    assert!(error.to_string().contains("does not match"));

    let preview_only = SelectionSnapshot::capture(&document.id, &unit.id, 1, "selected")
        .unwrap()
        .with_locator(
            DocumentLocator::unit(&document.id, &unit.id)
                .with_source(SourceLocator::office_rendered_page(1)),
        )
        .unwrap();
    let error = authorize_selection_snapshots(&library, &scope, vec![preview_only])
        .expect_err("preview-only source must be rejected");
    assert!(error.to_string().contains("preview-only"));
}

mod delete_test;

mod request_test;

mod scope_test;

mod selection_test;
