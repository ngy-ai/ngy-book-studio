//! Shared fixtures and constants for the library tests.

use super::*;
use crate::annotations::AnnotationKind;
use crate::editing::{DocumentEditor, NewContentUnit};

/// How long a background publication, or the hand-off to the publication
/// gate, may take in these tests.
///
/// `cargo test --all-targets` runs this binary next to ~900 other cases, so
/// the machine is oversubscribed while an assertion waits here: a budget
/// tight enough for an idle machine (5s) failed a full-suite run at
/// `concurrent_reuse_cannot_be_deleted_by_a_stale_gc_candidate` with the
/// publisher still inside `persist_document` (SQLite transaction + blob
/// write) after the gate had opened. A stall, not a deadlock: every lock
/// this side hands over is released before the wait. 30s matches the
/// fixture budgets in `tests/translation_flow.rs`.
const BACKGROUND_PUBLICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A book row can lose the language its own original declares: an earlier
/// EPUB importer ignored the OPF, and `create_book` never records one at all.
/// The startup repair reads the tag back, must never replace a language
/// somebody else already set, and must not run a second time.
mod annotations_test;

mod media_test;

mod progress_test;

mod store_test;

/// Writes an EPUB produced by this build to `directory` and returns its path.
/// Import tests need a real source file, and generating one keeps a binary
/// fixture out of the repository.
pub(crate) fn epub_source_file(directory: &Path, title: &str) -> PathBuf {
    let mut origin = LibraryStore::load_from(directory.join("epub-source")).unwrap();
    let template = origin.create_book(title, "作者").unwrap();
    let bytes = origin.epub_bytes(&template.id).unwrap();
    let path = directory.join(format!("{title}.epub"));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn create_two_unit_book(library: &mut LibraryStore) -> (BookRecord, String, String) {
    let book = library.create_book("双章节进度", "作者").unwrap();
    let document = library.document(&book.id).unwrap();
    let first_unit_id = document.units[0].id.clone();
    let mut editor = DocumentEditor::new(document).unwrap();
    let second_unit_id = editor
        .add_unit(NewContentUnit::html_chapter("第二章", 1))
        .unwrap();
    let updated = library.apply_document(editor.into_document()).unwrap();
    (updated, first_unit_id, second_unit_id)
}
