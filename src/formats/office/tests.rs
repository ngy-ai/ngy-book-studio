//! Tests for the Office importer, archive limits and worksheet rendering.

use std::io::Write as _;

use super::*;
use office_oxide::{
    docx::write::DocxWriter,
    xlsx::write::{CellData, XlsxWriter},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn office_archive_with_media(media: &[(&str, &[u8])], compression: CompressionMethod) -> Vec<u8> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file("word/document.xml", SimpleFileOptions::default())
        .expect("start document entry");
    archive
        .write_all(b"<w:document/>")
        .expect("write document entry");
    for (name, bytes) in media {
        archive
            .start_file(
                *name,
                SimpleFileOptions::default().compression_method(compression),
            )
            .expect("start media entry");
        archive.write_all(bytes).expect("write media entry");
    }
    archive
        .finish()
        .expect("finish Office archive")
        .into_inner()
}

fn office_asset_limits(
    max_assets: usize,
    max_asset_bytes: u64,
    max_total_asset_bytes: u64,
) -> ImportLimits {
    ImportLimits {
        max_assets,
        max_asset_bytes,
        max_total_asset_bytes,
        ..ImportLimits::default()
    }
}

#[test]
fn word_sections_split_at_headings() {
    let parsed = crate::markup::parse_source_for_unit(
        "<p>intro</p><h1>One</h1><p>body</p><h2>Two</h2><p>more</p>",
        "word-section",
    )
    .unwrap();
    let chunks = split_word_document(parsed.document, None);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[1].0, "One");
    assert_eq!(chunks[2].0, "Two");
}

#[test]
fn word_heading_split_preserves_inline_markup_and_literal_markdown() {
    let parsed = crate::markup::parse_source_for_unit(
        "<p># literal **text**</p><h1>One &amp; <strong>two</strong></h1>
             <h4>Detail</h4><pre><code>## code</code></pre><h2>Next</h2>",
        "word-section",
    )
    .unwrap();
    let original_blocks = parsed.document.blocks.clone();
    let chunks = split_word_document(parsed.document, None);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[1].0, "One & two");
    assert_eq!(chunks[1].1.blocks.len(), 3);
    assert_eq!(chunks[0].1.plain_text(), "# literal **text**");
    assert_eq!(chunks[2].0, "Next");
    let split_blocks: Vec<_> = chunks
        .into_iter()
        .flat_map(|(_, document)| document.blocks)
        .collect();
    assert_eq!(
        split_blocks, original_blocks,
        "splitting must preserve every typed block and its ID"
    );
}

#[test]
fn docx_native_html_keeps_literal_markdown_and_heading_boundaries() {
    let mut writer = DocxWriter::new();
    // Keep XML entity decoding outside this regression: office_oxide
    // 0.1.9 already drops entity events before creating its IR. Our HTML
    // escaping is covered directly at the parsed grid/AST boundary below.
    writer
        .add_paragraph("# literal **not emphasis** text")
        .add_heading("One", 1)
        .add_paragraph("[literal](not-a-link)")
        .add_heading("Detail", 4)
        .add_paragraph("detail body")
        .add_heading("Next", 2)
        .add_paragraph("last body");
    let mut output = Cursor::new(Vec::new());
    writer.write_to(&mut output).unwrap();
    let source = ImportSource {
        file_name: Some("native.docx".to_string()),
        bytes: Arc::new(output.into_inner()),
    };
    let imported = OfficeImporter
        .import(&source, &ImportLimits::default())
        .unwrap();
    imported.validate(&ImportLimits::default()).unwrap();
    let units = &imported.document.units;
    assert_eq!(units.len(), 3);
    assert_eq!(units[0].plain_text(), "# literal **not emphasis** text");
    assert_eq!(units[1].title, "One");
    assert!(units[1].source.contains("<h4>Detail</h4>"));
    assert!(units[1].plain_text().contains("[literal](not-a-link)"));
    assert!(!units[1].source.contains("<a "));
    assert_eq!(units[2].title, "Next");
    for unit in units {
        let parsed = crate::markup::parse_source_for_unit(&unit.source, &unit.id).unwrap();
        assert_eq!(
            parsed.document, unit.document,
            "unchanged HTML must retain block IDs"
        );
    }
}

#[test]
fn office_html_text_budget_is_enforced_before_section_parsing() {
    let limits = ImportLimits {
        max_total_text_bytes: 8,
        ..ImportLimits::default()
    };
    let mut units = Vec::new();
    let error = push_office_units(
        "book-office",
        BookFormat::Docx,
        1,
        None,
        "\0".repeat(9).as_str(),
        Vec::new(),
        None,
        &limits,
        &mut 0,
        &mut units,
    )
    .unwrap_err();
    assert!(error.to_string().contains("total text safety limit"));
    assert!(units.is_empty());
    let error = push_office_units(
        "book-office",
        BookFormat::Docx,
        2,
        None,
        "<p>x</p>",
        Vec::new(),
        None,
        &limits,
        &mut 1,
        &mut units,
    )
    .unwrap_err();
    assert!(error.to_string().contains("total text safety limit"));
    assert!(units.is_empty());
}

#[test]
fn worksheet_html_text_escapes_markup_and_preserves_line_breaks() {
    let mut html = String::new();
    push_escaped_html_text(&mut html, "A & <tag> | \\\r\nB\rC\nD");
    assert_eq!(html, "A &amp; &lt;tag&gt; | \\<br>B<br>C<br>D");
}

#[test]
fn office_probe_does_not_accept_random_bytes_as_a_document() {
    let source = ImportSource {
        file_name: Some("report.docx".to_string()),
        bytes: Arc::new(b"not an OOXML container".to_vec()),
    };
    assert_eq!(
        OfficeImporter.probe(&source).confidence,
        ProbeConfidence::Extension
    );
    assert!(
        OfficeImporter
            .import(&source, &ImportLimits::default())
            .is_err()
    );
}

#[test]
fn office_archive_asset_preflight_accepts_exact_boundaries() {
    let archive = office_archive_with_media(
        &[
            ("word/media/one.png", b"1234"),
            ("WORD/MEDIA/two.png", b"5678"),
        ],
        CompressionMethod::Stored,
    );
    validate_ooxml_archive(
        &archive,
        &office_asset_limits(3, archive.len() as u64, archive.len() as u64 + 8),
    )
    .expect("original plus two media assets fit exact limits");
}

#[test]
fn office_archive_asset_preflight_rejects_count_and_aggregate_overflow() {
    let archive = office_archive_with_media(
        &[
            ("word/media/one.png", b"1234"),
            ("word/media/two.png", b"5678"),
        ],
        CompressionMethod::Stored,
    );
    let count_error = validate_ooxml_archive(
        &archive,
        &office_asset_limits(2, archive.len() as u64, archive.len() as u64 + 8),
    )
    .expect_err("original plus two media entries exceed the asset count");
    assert!(count_error.to_string().contains("asset count safety limit"));

    let aggregate_error = validate_ooxml_archive(
        &archive,
        &office_asset_limits(3, archive.len() as u64, archive.len() as u64 + 7),
    )
    .expect_err("media entries exceed the aggregate extracted-asset limit");
    assert!(
        aggregate_error
            .to_string()
            .contains("aggregate asset safety limit")
    );
}

#[test]
fn office_archive_asset_preflight_rejects_compressed_oversized_media() {
    let media = vec![0_u8; 32 * 1024];
    let archive = office_archive_with_media(
        &[("word/media/large.png", media.as_slice())],
        CompressionMethod::Deflated,
    );
    assert!(
        archive.len() < media.len(),
        "fixture must exercise extraction"
    );
    let error = validate_ooxml_archive(
        &archive,
        &office_asset_limits(2, archive.len() as u64, u64::MAX),
    )
    .expect_err("decompressed media larger than the single-asset limit must fail");
    assert!(error.to_string().contains("single-asset safety limit"));
}

#[test]
fn office_html_ast_and_source_keep_extracted_images_in_sync() {
    let mut units = Vec::new();
    push_office_units(
        "book-office",
        BookFormat::Docx,
        1,
        Some("Section"),
        "<h1>Heading</h1><p>Paragraph with <strong>formatting</strong>.</p>",
        vec!["office-image-id".to_string()],
        None,
        &ImportLimits::default(),
        &mut 0,
        &mut units,
    )
    .unwrap();

    assert_eq!(units.len(), 1);
    assert!(units[0].source.contains("ngy-asset:office-image-id"));
    assert!(
        units[0]
            .document
            .referenced_asset_ids()
            .contains(&"office-image-id")
    );
    let reparsed = crate::markup::parse_source_for_unit(&units[0].source, &units[0].id).unwrap();
    assert!(
        reparsed
            .document
            .referenced_asset_ids()
            .contains(&"office-image-id")
    );
    assert!(reparsed.document.plain_text().contains("formatting"));
    assert_eq!(
        reparsed.document, units[0].document,
        "unchanged HTML must retain text and image block IDs"
    );
}

#[test]
fn worksheet_html_grid_preserves_escaped_values_from_parser_boundary() {
    let mut writer = XlsxWriter::new();
    writer
        .add_sheet("Escaping")
        .set_cell(1, 1, CellData::String("seed".into()));
    let mut output = Cursor::new(Vec::new());
    writer.write_to(&mut output).unwrap();
    let mut workbook =
        office_oxide::xlsx::XlsxDocument::from_reader(Cursor::new(output.into_inner())).unwrap();
    let value = "A & <script>literal</script> | \\ path\r\nnext\rthird";
    // Supply parsed text directly: pinned office_oxide 0.1.9 loses XML
    // entity events, a parser issue preceding the HTML conversion tested here.
    workbook.worksheets[0].rows[0].cells[0].value =
        office_oxide::xlsx::CellValue::String(value.into());
    let worksheet = &workbook.worksheets[0];
    let range = worksheet_used_range(worksheet, &workbook);
    assert_eq!(range.unwrap().to_a1(), "B2:B2");
    let html = worksheet_grid_html(worksheet, &workbook, range, &ImportLimits::default()).unwrap();
    assert!(html.contains("&amp; &lt;script&gt;literal&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
    let parsed = crate::markup::parse_source_for_unit(&html, "worksheet").unwrap();
    let Block::Table { header, rows, .. } = &parsed.document.blocks[0] else {
        panic!("HTML grid must retain typed cells");
    };
    assert!(rows.is_empty());
    assert_eq!(
        header.as_ref().unwrap().cells[0].plain_text(),
        "A & <script>literal</script> | \\ path\nnext\nthird"
    );
}

#[test]
fn xlsx_import_preserves_sparse_used_range_and_grid_coordinates() {
    let mut writer = XlsxWriter::new();
    {
        let mut sheet = writer.add_sheet("Sparse");
        sheet.set_cell(1, 1, CellData::String("top-left".to_string()));
        sheet.set_cell(2, 2, CellData::String("first\nsecond".to_string()));
        sheet.set_cell(3, 3, CellData::Formula("SUM(B2:C3)".to_string()));
        sheet.set_cell(4, 4, CellData::String("bottom-right".to_string()));
    }
    let mut output = Cursor::new(Vec::new());
    writer.write_to(&mut output).expect("write XLSX fixture");
    let source = ImportSource {
        file_name: Some("sparse.xlsx".to_string()),
        bytes: Arc::new(output.into_inner()),
    };

    let imported = OfficeImporter
        .import(&source, &ImportLimits::default())
        .expect("import sparse XLSX");
    imported
        .validate(&ImportLimits::default())
        .expect("valid imported XLSX");
    assert_eq!(imported.document.units.len(), 1);
    assert_eq!(
        imported.document.units[0].source_locator,
        Some(SourceLocator::worksheet(
            "Sparse",
            Some("B2:E5".to_string())
        ))
    );
    let Block::Table { header, rows, .. } = &imported.document.units[0].document.blocks[0] else {
        panic!("sparse worksheet should normalize to a table");
    };
    let header = header.as_ref().expect("table header");
    assert_eq!(header.cells.len(), 4);
    assert_eq!(header.cells[0].plain_text(), "top-left");
    assert_eq!(header.cells[1].plain_text(), "");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].cells[1].plain_text(), "first\nsecond");
    assert_eq!(rows[1].cells[2].plain_text(), "=SUM(B2:C3)");
    assert_eq!(rows[2].cells[3].plain_text(), "bottom-right");
}

#[test]
fn xlsx_import_uses_the_file_name_as_the_book_title() {
    let mut writer = XlsxWriter::new();
    writer
        .add_sheet("Quarterly Data")
        .set_cell(1, 1, CellData::String("value".to_string()));
    let mut output = Cursor::new(Vec::new());
    writer.write_to(&mut output).expect("write XLSX fixture");
    let source = ImportSource {
        file_name: Some("2026 预算.xlsx".to_string()),
        bytes: Arc::new(output.into_inner()),
    };

    let imported = OfficeImporter
        .import(&source, &ImportLimits::default())
        .expect("import XLSX");
    // office_oxide 把第一个工作表名放进 IR metadata title；XLSX 书名必须
    // 改用文件名，工作表名仍作为内容单元标题。
    assert_eq!(imported.document.title, "2026 预算");
    assert_eq!(imported.document.units.len(), 1);
    assert_eq!(imported.document.units[0].title, "Quarterly Data");
}
