//! Shared fixtures and constants for the library tests.

use super::*;
use crate::annotations::{AnnotationKind, TextAnchor};
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

mod asset_identity_test;

mod media_test;

mod progress_test;

mod share_reuse_test;

mod share_test;

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

/// A selection the host accepts: the tail of the first chapter's reading body
/// text, so the offsets are those of the reading projection rather than of an
/// edited copy. Returns the unit id, both revisions and the anchor.
pub(crate) fn first_chapter_anchor(
    library: &LibraryStore,
    book_id: &str,
) -> (String, u64, u64, TextAnchor) {
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

/// One stored machine译文 in the shape the reader parses.
pub(crate) fn stored_translation_payload(execution_identity: &str, translated: &str) -> String {
    serde_json::to_string(&crate::ai::translation::StoredTranslation {
        execution_identity: execution_identity.to_string(),
        segments: vec![crate::ai::translation::TranslationSegment {
            source: "开始写作……".into(),
            translated: translated.to_string(),
        }],
    })
    .unwrap()
}
