use std::{collections::HashMap, io::Cursor, sync::Arc};

use anyhow::{Context as _, Result, bail};
use office_oxide::{Document, DocumentFormat, DocumentIR};

use crate::{
    document::{
        AssetRole, Block, BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind,
        SourceKind, SourceLocator, TocNode, TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedAsset, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, asset_from_bytes, original_asset,
        safe_title,
    },
};

#[derive(Clone, Copy, Debug, Default)]
pub struct OfficeImporter;

const MAX_OFFICE_ARCHIVE_ENTRIES: usize = 20_000;
const MAX_OFFICE_ENTRY_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OFFICE_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_COMPRESSION_RATIO_CHECK_BYTES: u64 = 1024 * 1024;
const MAX_OFFICE_COMPRESSION_RATIO: u64 = 1_000;
const MAX_XLSX_GRID_ROWS: u32 = 10_000;
const MAX_XLSX_GRID_CELLS: u64 = 1_000_000;
const MAX_XLSX_COLUMNS: u32 = 16_384;
const MAX_XLSX_ROWS: u32 = 1_048_576;

const OFFICE_FORMATS: &[BookFormat] = &[
    BookFormat::Doc,
    BookFormat::Docx,
    BookFormat::Pptx,
    BookFormat::Xlsx,
];

impl DocumentImporter for OfficeImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "office_oxide",
            formats: OFFICE_FORMATS,
            parser_version: office_oxide::VERSION,
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        if source.bytes.starts_with(b"PK\x03\x04") {
            if zip_contains(source.bytes.as_slice(), "word/document.xml") {
                return matched(BookFormat::Docx, "OOXML Word container");
            }
            if zip_contains(source.bytes.as_slice(), "ppt/presentation.xml") {
                return matched(BookFormat::Pptx, "OOXML PowerPoint container");
            }
            if zip_contains(source.bytes.as_slice(), "xl/workbook.xml") {
                return matched(BookFormat::Xlsx, "OOXML Excel container");
            }
        }
        if source.bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0])
            && source.extension().as_deref() == Some("doc")
        {
            return matched(BookFormat::Doc, "OLE Compound File Word document");
        }
        let format = match source.extension().as_deref() {
            Some("doc") => BookFormat::Doc,
            Some("docx") => BookFormat::Docx,
            Some("pptx") => BookFormat::Pptx,
            Some("xlsx") => BookFormat::Xlsx,
            _ => return ProbeResult::no_match(BookFormat::Docx),
        };
        ProbeResult {
            format,
            confidence: ProbeConfidence::Extension,
            detail: Some("Office extension without a matching container signature".to_string()),
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        let probe = self.probe(source);
        if probe.confidence < ProbeConfidence::Container {
            bail!("Office file signature does not match its supported format");
        }
        let mut asset_budget =
            AssetBudget::with_original(limits, source.bytes.len(), "Office original source")?;
        if source.bytes.starts_with(b"PK\x03\x04") {
            validate_ooxml_archive(source.bytes.as_slice(), limits)?;
        }
        let (format, office_format, media_type) = match probe.format {
            BookFormat::Doc => (BookFormat::Doc, DocumentFormat::Doc, "application/msword"),
            BookFormat::Docx => (
                BookFormat::Docx,
                DocumentFormat::Docx,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            BookFormat::Pptx => (
                BookFormat::Pptx,
                DocumentFormat::Pptx,
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            ),
            BookFormat::Xlsx => (
                BookFormat::Xlsx,
                DocumentFormat::Xlsx,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            _ => bail!("unsupported Office format"),
        };
        let parsed =
            Document::from_reader(Cursor::new(source.bytes.as_ref().clone()), office_format)
                .context("office_oxide could not parse the document")?;
        let ir = parsed.to_ir();
        let original = original_asset(source, format, media_type);
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        let title = safe_title(ir.metadata.title.as_deref(), source.stem().as_str());

        let mut units = Vec::new();
        let mut imported_assets = vec![original.clone()];
        if ir.sections.is_empty() {
            let markdown = parsed.to_markdown();
            push_office_units(
                &book_id,
                format,
                1,
                None,
                &markdown,
                Vec::new(),
                None,
                limits,
                &mut units,
            )?;
        } else {
            for (section_index, section) in ir.sections.iter().enumerate() {
                let one_section = DocumentIR {
                    metadata: ir.metadata.clone(),
                    sections: vec![section.clone()],
                };
                let (markdown, worksheet_range) = if format == BookFormat::Xlsx {
                    if let Some(xlsx) = parsed.as_xlsx() {
                        if let Some(worksheet) = xlsx.worksheets.get(section_index) {
                            let used_range = worksheet_used_range(worksheet, xlsx);
                            (
                                worksheet_grid_markdown(worksheet, xlsx, used_range, limits)?,
                                used_range.map(|range| range.to_a1()),
                            )
                        } else {
                            (one_section.to_markdown(), None)
                        }
                    } else {
                        (one_section.to_markdown(), None)
                    }
                } else {
                    (one_section.to_markdown(), None)
                };
                let image_assets = extract_section_images(
                    &book_id,
                    section_index,
                    section,
                    &mut asset_budget,
                    &mut imported_assets,
                )?;
                push_office_units(
                    &book_id,
                    format,
                    section_index + 1,
                    section.title.as_deref(),
                    &markdown,
                    image_assets,
                    worksheet_range.as_deref(),
                    limits,
                    &mut units,
                )?;
            }
        }
        if units.is_empty() {
            let unit_id = deterministic_id("unit", format!("{book_id}\0empty").as_bytes());
            units.push(
                ContentUnit::empty(
                    unit_id,
                    unit_kind(format),
                    title.clone(),
                    SourceKind::Markdown,
                )
                .with_source_locator(source_locator(format, 1, None, None)),
            );
        }

        let toc = units
            .iter()
            .enumerate()
            .map(|(index, unit)| {
                TocNode::new(
                    deterministic_id("toc", format!("{book_id}\0{index}").as_bytes()),
                    unit.title.clone(),
                    TocTarget::unit(unit.id.clone()),
                )
            })
            .collect();
        let mut document = BookDocument::new(
            book_id,
            title,
            BookSource::imported(
                format,
                original.metadata.id.clone(),
                source.file_name.clone(),
            ),
        );
        if let Some(author) = ir
            .metadata
            .author
            .as_deref()
            .map(str::trim)
            .filter(|author| !author.is_empty())
        {
            document.authors.push(author.to_string());
        }
        document.description = ir.metadata.description.clone();
        document.units = units;
        document.toc = toc;
        document.assets = imported_assets
            .iter()
            .map(|asset| asset.metadata.clone())
            .collect();
        Ok(ImportedBook {
            document,
            assets: imported_assets,
        })
    }
}

fn matched(format: BookFormat, detail: &str) -> ProbeResult {
    ProbeResult {
        format,
        confidence: ProbeConfidence::Container,
        detail: Some(detail.to_string()),
    }
}

fn zip_contains(bytes: &[u8], name: &str) -> bool {
    zip::ZipArchive::new(Cursor::new(bytes)).is_ok_and(|mut archive| archive.by_name(name).is_ok())
}

fn validate_ooxml_archive(bytes: &[u8], limits: &ImportLimits) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("Office OOXML archive is invalid")?;
    if archive.len() > MAX_OFFICE_ARCHIVE_ENTRIES {
        bail!("Office OOXML archive contains too many entries");
    }

    let mut total_uncompressed = 0_u64;
    let mut asset_budget =
        AssetBudget::with_original(limits, bytes.len(), "Office original source")?;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("cannot inspect Office OOXML archive entry {index}"))?;
        if entry.encrypted() {
            bail!("encrypted Office OOXML entries are not supported");
        }
        if entry.enclosed_name().is_none() {
            bail!("Office OOXML archive contains an unsafe entry path");
        }
        let uncompressed = entry.size();
        if uncompressed > MAX_OFFICE_ENTRY_UNCOMPRESSED_BYTES {
            bail!("Office OOXML archive entry exceeds the uncompressed size limit");
        }
        total_uncompressed = total_uncompressed
            .checked_add(uncompressed)
            .context("Office OOXML archive size overflowed")?;
        if total_uncompressed > MAX_OFFICE_ARCHIVE_UNCOMPRESSED_BYTES {
            bail!("Office OOXML archive exceeds the total uncompressed size limit");
        }
        if is_office_media_entry(entry.name()) {
            asset_budget.add(
                uncompressed,
                &format!("Office embedded media {}", entry.name()),
            )?;
        }
        if uncompressed >= MIN_COMPRESSION_RATIO_CHECK_BYTES
            && (entry.compressed_size() == 0
                || entry
                    .compressed_size()
                    .saturating_mul(MAX_OFFICE_COMPRESSION_RATIO)
                    < uncompressed)
        {
            bail!("Office OOXML archive entry has a suspicious compression ratio");
        }
    }
    Ok(())
}

fn is_office_media_entry(name: &str) -> bool {
    let name = name.trim_start_matches('/');
    ["word/media/", "ppt/media/", "xl/media/"]
        .iter()
        .any(|prefix| {
            name.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorksheetRange {
    start_col: u32,
    start_row: u32,
    end_col: u32,
    end_row: u32,
}

impl WorksheetRange {
    fn to_a1(self) -> String {
        format!(
            "{}{}:{}{}",
            office_oxide::xlsx::CellRef::col_name(self.start_col),
            self.start_row + 1,
            office_oxide::xlsx::CellRef::col_name(self.end_col),
            self.end_row + 1
        )
    }

    fn row_count(self) -> u32 {
        self.end_row - self.start_row + 1
    }

    fn column_count(self) -> u32 {
        self.end_col - self.start_col + 1
    }
}

fn worksheet_used_range(
    worksheet: &office_oxide::xlsx::Worksheet,
    workbook: &office_oxide::xlsx::XlsxDocument,
) -> Option<WorksheetRange> {
    let mut range: Option<WorksheetRange> = None;
    for row in &worksheet.rows {
        for cell in &row.cells {
            let visible = !workbook.format_cell_value(cell).trim().is_empty()
                || cell
                    .formula
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
            if !visible {
                continue;
            }
            let col = cell.reference.col;
            let row = cell.reference.row;
            range = Some(match range {
                Some(range) => WorksheetRange {
                    start_col: range.start_col.min(col),
                    start_row: range.start_row.min(row),
                    end_col: range.end_col.max(col),
                    end_row: range.end_row.max(row),
                },
                None => WorksheetRange {
                    start_col: col,
                    start_row: row,
                    end_col: col,
                    end_row: row,
                },
            });
        }
    }
    range
}

fn worksheet_grid_markdown(
    worksheet: &office_oxide::xlsx::Worksheet,
    workbook: &office_oxide::xlsx::XlsxDocument,
    range: Option<WorksheetRange>,
    limits: &ImportLimits,
) -> Result<String> {
    let Some(range) = range else {
        return Ok(String::new());
    };
    let rows = range.row_count();
    let columns = range.column_count();
    if range.end_col >= MAX_XLSX_COLUMNS || range.end_row >= MAX_XLSX_ROWS {
        bail!("XLSX used range exceeds the supported worksheet boundary");
    }
    if rows > MAX_XLSX_GRID_ROWS {
        bail!("XLSX used range exceeds the {MAX_XLSX_GRID_ROWS} row safety limit");
    }
    let cells = u64::from(rows)
        .checked_mul(u64::from(columns))
        .context("XLSX used range size overflowed")?;
    if cells > MAX_XLSX_GRID_CELLS {
        bail!("XLSX used range exceeds the {MAX_XLSX_GRID_CELLS} cell safety limit");
    }

    let mut values = HashMap::new();
    for row in &worksheet.rows {
        for cell in &row.cells {
            if cell.reference.row < range.start_row
                || cell.reference.row > range.end_row
                || cell.reference.col < range.start_col
                || cell.reference.col > range.end_col
            {
                continue;
            }
            let mut value = workbook.format_cell_value(cell);
            if value.trim().is_empty()
                && let Some(formula) = cell
                    .formula
                    .as_deref()
                    .map(str::trim)
                    .filter(|formula| !formula.is_empty())
            {
                value = format!("={formula}");
            }
            if !value.is_empty() {
                values.insert((cell.reference.row, cell.reference.col), value);
            }
        }
    }

    let mut markdown = String::new();
    for row in range.start_row..=range.end_row {
        push_worksheet_markdown_row(&mut markdown, row, range, &values);
        if row == range.start_row {
            markdown.push('|');
            for _ in range.start_col..=range.end_col {
                markdown.push_str(" --- |");
            }
            markdown.push('\n');
        }
        if markdown.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
    }
    if markdown.ends_with('\n') {
        markdown.pop();
    }
    for shape in &worksheet.text_shapes {
        let text = shape.text.trim();
        if text.is_empty() {
            continue;
        }
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(text);
        if markdown.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
    }
    Ok(markdown)
}

fn push_worksheet_markdown_row(
    output: &mut String,
    row: u32,
    range: WorksheetRange,
    values: &HashMap<(u32, u32), String>,
) {
    output.push('|');
    for col in range.start_col..=range.end_col {
        output.push(' ');
        if let Some(value) = values.get(&(row, col)) {
            push_escaped_table_cell(output, value);
        }
        output.push_str(" |");
    }
    output.push('\n');
}

fn push_escaped_table_cell(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '|' => output.push_str("\\|"),
            '\r' => {}
            '\n' => output.push_str("  "),
            _ => output.push(character),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_office_units(
    book_id: &str,
    format: BookFormat,
    section_index: usize,
    native_title: Option<&str>,
    markdown: &str,
    image_assets: Vec<String>,
    worksheet_range: Option<&str>,
    limits: &ImportLimits,
    units: &mut Vec<ContentUnit>,
) -> Result<()> {
    let chunks = if matches!(format, BookFormat::Doc | BookFormat::Docx) {
        split_word_markdown(markdown, native_title)
    } else {
        vec![(
            safe_title(
                native_title,
                &format!("{} {section_index}", unit_label(format)),
            ),
            markdown.to_string(),
        )]
    };
    for (chunk_index, (title, source)) in chunks.into_iter().enumerate() {
        if units.len() >= limits.max_units {
            bail!("Office document contains too many content units");
        }
        if source.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
        let unit_id = deterministic_id(
            "unit",
            format!("{book_id}\0{section_index}\0{chunk_index}\0{title}").as_bytes(),
        );
        let parsed = crate::markup::parse_source_for_unit(SourceKind::Markdown, &source, &unit_id)
            .context("failed to normalize Office Markdown")?;
        let mut document = parsed.document;
        if chunk_index == 0 {
            document.blocks.extend(image_assets.iter().enumerate().map(
                |(image_index, asset_id)| Block::Image {
                    id: deterministic_id(
                        "block",
                        format!("{unit_id}\0image\0{image_index}").as_bytes(),
                    ),
                    asset_id: asset_id.clone(),
                    alt: String::new(),
                    title: None,
                    caption: Vec::new(),
                },
            ));
        }
        let source = crate::markup::serialize_source(&document, SourceKind::Markdown)
            .context("failed to serialize normalized Office content")?;
        units.push(
            ContentUnit::new(
                unit_id,
                unit_kind(format),
                title,
                SourceKind::Markdown,
                source,
                document,
            )
            .with_source_locator(source_locator(
                format,
                section_index,
                native_title,
                worksheet_range,
            )),
        );
    }
    Ok(())
}

fn split_word_markdown(markdown: &str, native_title: Option<&str>) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut current_title = safe_title(native_title, "Introduction");
    let mut current = String::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        let is_heading = (1..=3).contains(&hashes)
            && trimmed
                .as_bytes()
                .get(hashes)
                .is_some_and(u8::is_ascii_whitespace);
        if is_heading {
            if !current.trim().is_empty() {
                result.push((current_title, current.trim().to_string()));
                current = String::new();
            }
            current_title = trimmed[hashes..].trim().to_string();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        result.push((current_title, current.trim().to_string()));
    }
    if result.is_empty() {
        result.push((
            safe_title(native_title, "Section 1"),
            markdown.trim().to_string(),
        ));
    }
    result
}

fn extract_section_images(
    book_id: &str,
    section_index: usize,
    section: &office_oxide::ir::Section,
    asset_budget: &mut AssetBudget,
    output: &mut Vec<ImportedAsset>,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for (element_index, element) in section.elements.iter().enumerate() {
        let office_oxide::ir::Element::Image(image) = element else {
            continue;
        };
        let (Some(bytes), Some(format)) = (image.data.as_ref(), image.format.as_ref()) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        asset_budget.add_usize(
            bytes.len(),
            &format!(
                "Office image in section {} element {}",
                section_index + 1,
                element_index + 1
            ),
        )?;
        let asset = asset_from_bytes(
            &format!("{book_id}\0office-image\0{section_index}\0{element_index}"),
            vec![AssetRole::ContentImage],
            format.content_type(),
            Some(format!(
                "office-image-{}-{}.{}",
                section_index + 1,
                element_index + 1,
                format.extension()
            )),
            Arc::new(bytes.clone()),
        );
        ids.push(asset.metadata.id.clone());
        output.push(asset);
    }
    Ok(ids)
}

fn unit_kind(format: BookFormat) -> ContentUnitKind {
    match format {
        BookFormat::Pptx => ContentUnitKind::Slide,
        BookFormat::Xlsx => ContentUnitKind::Worksheet,
        _ => ContentUnitKind::Chapter,
    }
}

fn unit_label(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Pptx => "Slide",
        BookFormat::Xlsx => "Sheet",
        _ => "Section",
    }
}

fn source_locator(
    format: BookFormat,
    index: usize,
    title: Option<&str>,
    worksheet_range: Option<&str>,
) -> SourceLocator {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    match format {
        BookFormat::Pptx => SourceLocator::slide(index),
        BookFormat::Xlsx => SourceLocator::worksheet(
            safe_title(title, &format!("Sheet {index}")),
            worksheet_range.map(str::to_string),
        ),
        _ => SourceLocator::office_section(index),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use office_oxide::xlsx::write::{CellData, XlsxWriter};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn office_archive_with_media(
        media: &[(&str, &[u8])],
        compression: CompressionMethod,
    ) -> Vec<u8> {
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
        let chunks = split_word_markdown("intro\n\n# One\nbody\n\n## Two\nmore", None);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].0, "One");
        assert_eq!(chunks[2].0, "Two");
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
    fn office_markdown_ast_and_source_keep_extracted_images_in_sync() {
        let mut units = Vec::new();
        push_office_units(
            "book-office",
            BookFormat::Docx,
            1,
            Some("Section"),
            "# Heading\n\nParagraph with **formatting**.",
            vec!["office-image-id".to_string()],
            None,
            &ImportLimits::default(),
            &mut units,
        )
        .unwrap();

        assert_eq!(units.len(), 1);
        assert!(units[0].source.contains("moye-asset:office-image-id"));
        assert!(
            units[0]
                .document
                .referenced_asset_ids()
                .contains(&"office-image-id")
        );
        let reparsed = crate::markup::parse_source_for_unit(
            SourceKind::Markdown,
            &units[0].source,
            &units[0].id,
        )
        .unwrap();
        assert!(
            reparsed
                .document
                .referenced_asset_ids()
                .contains(&"office-image-id")
        );
        assert!(reparsed.document.plain_text().contains("formatting"));
    }

    #[test]
    fn xlsx_import_preserves_sparse_used_range_and_grid_coordinates() {
        let mut writer = XlsxWriter::new();
        {
            let mut sheet = writer.add_sheet("Sparse");
            sheet.set_cell(1, 1, CellData::String("top-left".to_string()));
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
        let Block::Table { header, rows, .. } = &imported.document.units[0].document.blocks[0]
        else {
            panic!("sparse worksheet should normalize to a table");
        };
        assert_eq!(header.as_ref().expect("table header").cells.len(), 4);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].cells[3].plain_text(), "bottom-right");
    }
}
