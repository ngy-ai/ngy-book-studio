//! Opt-in parser gate for real, externally sourced documents.
//!
//! Set `NGY_FORMAT_CORPUS` to a directory containing `sample.epub`,
//! `sample.pdf`, `sample.doc`, `sample.docx`, `sample.pptx`, `sample.xlsx`,
//! `sample.mobi`, `sample.azw`, `sample.azw3`, `sample.kfx` (DRM-free), and
//! `sample.djvu`, then run:
//!
//! `cargo test --test format_corpus_gate --locked -- --ignored --nocapture`
//!
//! Corpus files deliberately stay outside the repository. This keeps the
//! parser gate reproducible without redistributing third-party documents.

use std::{env, fs, io::Cursor, path::Path, sync::Arc};

use ngy_book_studio::{
    document::{BookFormat, BookSource},
    formats::{FormatRegistry, ImportLimits, ImportSource, ProbeConfidence},
    library::{ImportOutcome, LibraryStore},
};

#[test]
#[ignore = "requires the external NGY_FORMAT_CORPUS fixture directory"]
fn real_documents_cross_the_canonical_import_gate() {
    let root = env::var_os("NGY_FORMAT_CORPUS")
        .map(std::path::PathBuf::from)
        .expect("set NGY_FORMAT_CORPUS to the external fixture directory");
    let registry = FormatRegistry::with_builtin_importers();

    for (file_name, expected_format) in [
        ("sample.epub", BookFormat::Epub),
        ("sample.pdf", BookFormat::Pdf),
        ("sample.doc", BookFormat::Doc),
        ("sample.docx", BookFormat::Docx),
        ("sample.pptx", BookFormat::Pptx),
        ("sample.xlsx", BookFormat::Xlsx),
        ("sample.mobi", BookFormat::Mobi),
        ("sample.azw", BookFormat::Azw),
        ("sample.azw3", BookFormat::Azw3),
        ("sample.kfx", BookFormat::Kfx),
        ("sample.djvu", BookFormat::Djvu),
    ] {
        validate_sample(&registry, &root, file_name, expected_format);
    }
}

fn validate_sample(
    registry: &FormatRegistry,
    root: &Path,
    file_name: &str,
    expected_format: BookFormat,
) {
    let path = root.join(file_name);
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("cannot read corpus fixture {}: {error}", path.display()));
    let source = ImportSource {
        file_name: Some(file_name.to_string()),
        bytes: Arc::new(bytes.clone()),
    };
    let probes = registry.probe(&source);
    let best = probes
        .first()
        .unwrap_or_else(|| panic!("no importer recognized {file_name}"));
    assert_eq!(best.format, expected_format, "wrong probe for {file_name}");
    assert!(
        best.confidence >= ProbeConfidence::Container,
        "{file_name} was recognized only from its extension"
    );

    let imported = registry
        .import(&source, &ImportLimits::default())
        .unwrap_or_else(|error| panic!("failed to import {file_name}: {error:#}"));
    assert!(
        matches!(
            imported.document.source,
            BookSource::Imported { format, .. } if format == expected_format
        ),
        "canonical source format changed for {file_name}"
    );
    assert!(
        !imported.document.units.is_empty(),
        "{file_name} produced no content units"
    );
    assert!(
        imported
            .document
            .units
            .iter()
            .all(|unit| unit.source_locator.is_some()),
        "{file_name} produced a unit without a source locator"
    );
    assert_eq!(
        imported
            .original_asset()
            .unwrap_or_else(|| panic!("{file_name} has no immutable original asset"))
            .bytes
            .as_slice(),
        bytes,
        "{file_name} did not retain byte-exact source data"
    );

    let case = tempfile::tempdir().expect("create isolated corpus case");
    let mut library = LibraryStore::load_from(case.path().join("library"))
        .unwrap_or_else(|error| panic!("cannot create library for {file_name}: {error:#}"));
    let record = match library
        .import(&path)
        .unwrap_or_else(|error| panic!("library failed to import {file_name}: {error:#}"))
    {
        ImportOutcome::Added(record) => record,
        ImportOutcome::AlreadyExists(_) => panic!("fresh corpus case deduplicated {file_name}"),
    };
    let canonical = library
        .document(&record.id)
        .unwrap_or_else(|error| panic!("cannot reload {file_name} canonical model: {error:#}"));
    let first_unit = canonical
        .units
        .first()
        .unwrap_or_else(|| panic!("{file_name} has no editable content unit"));
    let edited_token = format!("NgyCorpusEdited{expected_format:?}");
    let updated = library
        .update_content_unit_source(
            &record.id,
            &first_unit.id,
            &format!("<h1>Corpus edit</h1><p>{edited_token}</p>"),
        )
        .unwrap_or_else(|error| panic!("cannot edit {file_name}: {error:#}"));
    assert_eq!(updated.revision, record.revision + 1);
    assert!(
        library
            .search_book(&record.id, &edited_token, 10)
            .unwrap_or_else(|error| panic!("cannot search edited {file_name}: {error:#}"))
            .iter()
            .any(|hit| hit.book_id == record.id),
        "{file_name} edited text was not indexed"
    );

    let normalized_epub = case.path().join("normalized.epub");
    let normalized_pdf = case.path().join("normalized.pdf");
    let original_export = case.path().join(file_name);
    library
        .export_epub(&record.id, &normalized_epub)
        .unwrap_or_else(|error| panic!("cannot export {file_name} as EPUB: {error:#}"));
    library
        .export_pdf(&record.id, &normalized_pdf)
        .unwrap_or_else(|error| panic!("cannot export {file_name} as PDF: {error:#}"));
    library
        .export_original(&record.id, &original_export)
        .unwrap_or_else(|error| panic!("cannot export original {file_name}: {error:#}"));
    rbook::Epub::read(Cursor::new(
        fs::read(&normalized_epub).expect("read normalized EPUB"),
    ))
    .unwrap_or_else(|error| panic!("normalized EPUB for {file_name} is invalid: {error:#}"));
    lopdf::Document::load(&normalized_pdf)
        .unwrap_or_else(|error| panic!("normalized PDF for {file_name} is invalid: {error:#}"));
    assert_eq!(
        fs::read(&original_export).expect("read exported original"),
        bytes,
        "{file_name} original changed after normalized editing"
    );

    drop(library);
    let reopened = LibraryStore::load_from(case.path().join("library"))
        .unwrap_or_else(|error| panic!("cannot reopen library for {file_name}: {error:#}"));
    let reopened_document = reopened
        .document(&record.id)
        .unwrap_or_else(|error| panic!("cannot restore edited {file_name}: {error:#}"));
    assert_eq!(reopened_document.revision.get(), record.revision + 1);
    assert!(
        reopened_document.units[0]
            .plain_text()
            .contains(&edited_token)
    );
    let reopened_original = case.path().join(format!("reopened-{file_name}"));
    reopened
        .export_original(&record.id, &reopened_original)
        .unwrap_or_else(|error| panic!("cannot re-export original {file_name}: {error:#}"));
    assert_eq!(
        fs::read(&reopened_original).expect("read reopened original export"),
        bytes,
        "{file_name} original changed after reopening"
    );

    eprintln!(
        "validated {file_name}: {} units, {} assets, edit/search/EPUB/PDF/original/reopen passed",
        imported.document.units.len(),
        imported.document.assets.len()
    );
}
