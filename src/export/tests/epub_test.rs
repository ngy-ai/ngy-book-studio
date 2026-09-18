//! Tests for the EPUB output and the PDF page assembly.

use super::super::*;
use super::*;

#[test]
pub(crate) fn normalized_epub_is_deterministic_valid_and_script_free() {
    let (document, assets, _) = sample();
    let first = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Epub, &assets)
        .expect("export EPUB");
    let second = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Epub, &assets)
        .expect("repeat export EPUB");
    assert_eq!(first, second);

    let mut archive = zip::ZipArchive::new(Cursor::new(&first)).expect("valid ZIP");
    let first_entry = archive.by_index(0).expect("first ZIP entry");
    assert_eq!(first_entry.name(), "mimetype");
    assert_eq!(first_entry.compression(), CompressionMethod::Stored);
    drop(first_entry);
    let mut chapter = String::new();
    std::io::Read::read_to_string(
        &mut archive
            .by_name("EPUB/text/unit-0001.xhtml")
            .expect("chapter entry"),
        &mut chapter,
    )
    .expect("read chapter");
    assert!(chapter.contains("中文测试 &amp; normalized export"));
    assert!(chapter.contains("../assets/asset-0001.png"));
    assert!(!chapter.to_ascii_lowercase().contains("<script"));
    assert!(rbook::Epub::read(Cursor::new(first)).is_ok());
}

#[test]
pub(crate) fn normalized_epub_does_not_duplicate_an_existing_unit_title() {
    let (mut document, assets, _) = sample();
    assert!(!unit_body_starts_with_title(&document.units[0]));
    let title = document.units[0].title.clone();
    document.units[0]
        .document
        .blocks
        .insert(0, Block::heading("chapter-title", 1, title));
    assert!(unit_body_starts_with_title(&document.units[0]));

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

    assert_eq!(chapter.matches("<h1").count(), 1, "{chapter}");
}

#[test]
pub(crate) fn only_a_heading_block_can_supply_the_unit_title() {
    let (mut document, _, _) = sample();
    let title = document.units[0].title.clone();
    let non_heading_blocks = vec![
        Block::paragraph("paragraph-title", &title),
        Block::BlockQuote {
            id: "quote-title".to_string(),
            blocks: vec![Block::heading("nested-heading", 1, &title)],
        },
        Block::BulletList {
            id: "bullet-title".to_string(),
            items: vec![ListItem::new(vec![Block::paragraph("bullet-text", &title)])],
        },
        Block::OrderedList {
            id: "ordered-title".to_string(),
            start: 1,
            items: vec![ListItem::new(vec![Block::paragraph(
                "ordered-text",
                &title,
            )])],
        },
        Block::CodeBlock {
            id: "code-title".to_string(),
            language: None,
            code: title.clone(),
        },
        Block::Table {
            id: "table-title".to_string(),
            header: Some(TableRow::new(vec![TableCell::text(&title)])),
            rows: Vec::new(),
        },
        Block::Image {
            id: "image-title".to_string(),
            asset_id: "image-asset".to_string(),
            alt: title.clone(),
            title: None,
            caption: Vec::new(),
        },
        Block::Audio {
            id: "audio-title".to_string(),
            asset_id: "audio-asset".to_string(),
            title: Some(title.clone()),
            caption: Vec::new(),
        },
        Block::Video {
            id: "video-title".to_string(),
            asset_id: "video-asset".to_string(),
            poster_asset_id: None,
            title: Some(title.clone()),
            caption: Vec::new(),
        },
        Block::RawHtml {
            id: "raw-title".to_string(),
            source: format!("<h1>{title}</h1>"),
            plain_text: title.clone(),
        },
    ];

    for block in non_heading_blocks {
        document.units[0].document.blocks = vec![block.clone()];
        assert!(
            !unit_body_starts_with_title(&document.units[0]),
            "non-heading block must not suppress the generated title: {block:?}"
        );
    }
}

#[test]
pub(crate) fn normalized_epub_keeps_title_when_code_only_has_the_same_text() {
    let (mut document, assets, _) = sample();
    let title = document.units[0].title.clone();
    document.units[0].document.blocks.insert(
        0,
        Block::CodeBlock {
            id: "code-title".to_string(),
            language: None,
            code: title.clone(),
        },
    );
    assert!(!unit_body_starts_with_title(&document.units[0]));

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

    assert_eq!(chapter.matches("<h1").count(), 1, "{chapter}");
    assert!(
        chapter.contains(&format!("<code>{title}</code>")),
        "{chapter}"
    );
}

#[test]
pub(crate) fn pdf_deduplicates_heading_title_but_not_code_with_the_same_text() {
    let (mut document, assets, _) = sample();
    let title = document.units[0].title.clone();
    let encoded_title = pdf_ucs2_hex(&title);

    document.units[0]
        .document
        .blocks
        .insert(0, Block::heading("chapter-title", 1, &title));
    let heading_pdf = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Pdf, &assets)
        .expect("export PDF with heading title");
    assert_eq!(
        String::from_utf8_lossy(&heading_pdf)
            .matches(&encoded_title)
            .count(),
        1,
        "semantic heading should suppress the generated duplicate"
    );

    document.units[0].document.blocks[0] = Block::CodeBlock {
        id: "code-title".to_string(),
        language: None,
        code: title,
    };
    let code_pdf = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Pdf, &assets)
        .expect("export PDF with code matching title");
    assert_eq!(
        String::from_utf8_lossy(&code_pdf)
            .matches(&encoded_title)
            .count(),
        2,
        "code text must not suppress the generated unit title"
    );
}

#[test]
pub(crate) fn pdf_is_deterministic_parseable_and_contains_text_and_image_pages() {
    let (document, assets, _) = sample();
    let first = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Pdf, &assets)
        .expect("export PDF");
    let second = BuiltinDocumentExporter
        .export_bytes(&document, ExportFormat::Pdf, &assets)
        .expect("repeat export PDF");
    assert_eq!(first, second);
    assert!(first.starts_with(b"%PDF-1.7"));
    let parsed = lopdf::Document::load_mem(&first).expect("parse generated PDF");
    assert!(parsed.get_pages().len() >= 3);
    let ascii = String::from_utf8_lossy(&first);
    assert!(ascii.contains("4E2D65876D4B8BD5"));
    assert!(ascii.contains("/Subtype /Image"));
}
