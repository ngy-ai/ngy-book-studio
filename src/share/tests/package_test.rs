use super::super::*;

use super::manifest_test::manifest;

fn note() -> SharedNote {
    SharedNote {
        unit: SharedUnitRef {
            ordinal: 0,
            title: Some("第一章".into()),
        },
        kind: "highlight".into(),
        quote: "被划线的文字".into(),
        start: 3,
        end: 9,
        comment: None,
        created_at: 1,
        updated_at: 2,
    }
}

#[test]
fn a_package_round_trips_through_the_zip_container() {
    let mut manifest = manifest();
    manifest.notes.count = 1;
    manifest.chats.count = 1;
    manifest.translations = ShareTranslationSummary {
        count: 1,
        target_language: Some("zh-Hans".into()),
    };
    let notes = vec![note()];
    let chats = vec![SharedChatThread {
        title: "这一章讲了什么".into(),
        window_kind: "reader".into(),
        created_at: 20,
        updated_at: 21,
        messages: vec![SharedChatMessage {
            ordinal: 0,
            parent_ordinal: None,
            role: "user".into(),
            content: "讲讲这一章".into(),
            model: None,
            created_at: 20,
            citations: vec![SharedChatCitation {
                unit: Some(note().unit),
                source_kind: "book".into(),
                quote: "被划线的文字".into(),
                url: None,
                source_title: None,
                locator_json: Some(r#"{"book_id":"book-1","unit_id":"unit-1"}"#.into()),
            }],
        }],
    }];
    let translations = vec![SharedTranslation {
        unit: SharedUnitRef {
            ordinal: 0,
            title: Some("第一章".into()),
        },
        block_index: 2,
        ordinal: 3,
        target_language: "zh-Hans".into(),
        source_language: Some("en".into()),
        model: "sender-model".into(),
        source_text: "Original text".into(),
        translated_text: "译文".into(),
        manual_text: None,
    }];

    let bytes =
        write_share_package(&manifest, b"epub-bytes", &notes, &chats, &translations).unwrap();
    let package = read_share_package(&bytes).unwrap();

    assert_eq!(package.manifest, manifest);
    assert_eq!(package.book_epub, b"epub-bytes");
    assert_eq!(package.notes, notes);
    assert_eq!(package.chats, chats);
    assert_eq!(package.translations, translations);
}

#[test]
fn an_empty_section_is_optional() {
    let manifest = manifest();
    let bytes = write_share_package(&manifest, b"epub-bytes", &[], &[], &[]).unwrap();
    let package = read_share_package(&bytes).unwrap();
    assert!(package.notes.is_empty());
    assert!(package.chats.is_empty());
    assert!(package.translations.is_empty());
}

#[test]
fn counts_that_disagree_with_the_sections_are_refused_on_both_sides() {
    let mut manifest = manifest();
    manifest.notes.count = 2;
    assert!(
        write_share_package(&manifest, b"epub-bytes", &[note()], &[], &[]).is_err(),
        "an export may not write a manifest that overstates its sections"
    );

    // A package that was truncated after the manifest is refused on read as
    // well, so a damaged file cannot import half of its notes.
    let mut honest = manifest;
    honest.notes.count = 1;
    let bytes = write_share_package(&honest, b"epub-bytes", &[note()], &[], &[]).unwrap();
    let mut tampered = honest;
    tampered.notes.count = 5;
    let raw = raw_container(&tampered, &[note()]);
    assert!(read_share_package(&raw).is_err());
    assert!(read_share_package(&bytes).is_ok());
}

/// Writes a container directly, bypassing the export-side validation, so the
/// read-side count check can be exercised with a manifest the writer would have
/// refused.
fn raw_container(manifest: &ShareManifest, notes: &[SharedNote]) -> Vec<u8> {
    use std::io::{Cursor, Write as _};
    use zip::write::SimpleFileOptions;

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    let start = |writer: &mut zip::ZipWriter<Cursor<Vec<u8>>>, name: &str, bytes: &[u8]| {
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes).unwrap();
    };
    start(
        &mut writer,
        MANIFEST_ENTRY,
        &serde_json::to_vec(manifest).unwrap(),
    );
    start(&mut writer, BOOK_ENTRY, b"epub-bytes");
    start(
        &mut writer,
        NOTES_ENTRY,
        &serde_json::to_vec(notes).unwrap(),
    );
    writer.finish().unwrap().into_inner()
}

#[test]
fn a_package_that_is_not_a_zip_is_refused_with_a_reason() {
    let error = read_share_package(b"not a zip at all")
        .unwrap_err()
        .to_string();
    assert!(error.contains("压缩包"), "unexpected error: {error}");
}
