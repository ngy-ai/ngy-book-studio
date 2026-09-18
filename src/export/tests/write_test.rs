//! Tests for atomic writes, wrapping and XHTML serialization.

use super::super::*;
use super::*;

#[test]
pub(crate) fn created_book_has_no_original_export() {
    let mut document = BookDocument::created("book-created", "新书");
    document.units.push(ContentUnit::empty(
        "unit-1",
        ContentUnitKind::Chapter,
        "第一章",
    ));
    let error = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Original, &MemoryAssets::default())
        .expect_err("created book has no original");
    assert!(error.to_string().contains("没有可导出的原文件"));
}

#[test]
pub(crate) fn atomic_write_never_requires_the_target_to_be_absent() {
    let directory = tempfile::tempdir().expect("temporary export directory");
    let target = directory.path().join("book.pdf");
    atomic_write(&target, b"first").expect("first atomic write");
    atomic_write(&target, b"second").expect("replace atomically");
    assert_eq!(fs::read(target).expect("read target"), b"second");
}

#[test]
pub(crate) fn pre_commit_sync_failure_leaves_existing_target_unchanged() {
    let directory = tempfile::tempdir().expect("temporary export directory");
    let target = directory.path().join("book.pdf");
    fs::write(&target, b"old complete file").expect("seed target");

    let error = atomic_write_with_syncs(
        &target,
        b"new complete file",
        |_| Err(std::io::Error::other("injected staged-file sync failure")),
        |_| Ok(()),
    )
    .expect_err("pre-commit sync failure must abort the replacement");

    assert!(error.to_string().contains("临时文件"));
    assert_eq!(
        fs::read(&target).expect("read unchanged target"),
        b"old complete file"
    );
}

#[test]
pub(crate) fn post_commit_directory_sync_failure_does_not_report_export_failure() {
    let directory = tempfile::tempdir().expect("temporary export directory");
    let target = directory.path().join("book.pdf");
    fs::write(&target, b"old complete file").expect("seed target");

    atomic_write_with_syncs(
        &target,
        b"new complete file",
        std::fs::File::sync_all,
        |_| Err(std::io::Error::other("injected directory sync failure")),
    )
    .expect("the replacement is already committed");

    assert_eq!(
        fs::read(&target).expect("read committed target"),
        b"new complete file"
    );
}

#[test]
pub(crate) fn visual_line_wrapping_is_utf8_safe() {
    let wrapped = wrap_visual_line(&"中".repeat(50), PDF_LINE_WIDTH);
    assert_eq!(wrapped.len(), 2);
    assert_eq!(wrapped.concat(), "中".repeat(50));
}

#[test]
pub(crate) fn normalized_epub_chapter_is_strict_xhtml_when_raw_html_has_named_entities() {
    let (mut document, assets, _) = sample();
    let raw = "<p>alpha&nbsp;<span>&mdash;</span> beta</p>";
    document.units[0].document.blocks = vec![Block::RawHtml {
        id: "raw-entity".to_string(),
        source: raw.to_string(),
        plain_text: crate::document::raw_html_plain_text(raw).expect("plain text"),
    }];

    let bytes = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Epub, &assets)
        .expect("export EPUB");
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("valid ZIP");
    let mut chapter = String::new();
    std::io::Read::read_to_string(
        &mut archive
            .by_name("EPUB/text/unit-0001.xhtml")
            .expect("chapter entry"),
        &mut chapter,
    )
    .expect("read chapter");

    // The chapter is served as `application/xhtml+xml`; a named reference
    // XML cannot resolve replaces the whole page with a parser error.
    assert!(!chapter.contains("&nbsp;"), "{chapter}");
    assert!(!chapter.contains("&mdash;"), "{chapter}");
    assert!(chapter.contains("&#160;"), "{chapter}");
    // The `<!DOCTYPE html>` declaration carries no entity definitions, so
    // DTD parsing has to be enabled for an XML parser to reach the body —
    // exactly the situation WebView2 is in.
    let xml = resvg::usvg::roxmltree::Document::parse_with_options(
        &chapter,
        resvg::usvg::roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .expect("strict XHTML");
    let text = xml
        .root_element()
        .descendants()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect::<String>();
    // The rewrite must keep the characters themselves: U+00A0 and U+2014.
    assert!(text.ends_with("alpha\u{a0}\u{2014} beta"), "{text:?}");
}

#[test]
pub(crate) fn raw_html_is_inert_and_serialized_as_xhtml() {
    let clean = sanitize_raw_html(
        r#"<script src="https://example.invalid/a.js">bad()</script><img src="//example.invalid/a.png"><br><a href="chapter.xhtml">next</a>"#,
    );
    assert!(!clean.contains("script"));
    assert!(!clean.contains("example.invalid"));
    assert!(clean.contains("<img />") || clean.contains("<img/>"));
    assert!(clean.contains("<br />") || clean.contains("<br/>"));
    assert!(clean.contains("href=\"chapter.xhtml\""));
}

#[test]
pub(crate) fn xml_escape_replaces_disallowed_control_characters() {
    assert_eq!(escape_xml("a\u{1}&b"), "a\u{fffd}&amp;b");
}
