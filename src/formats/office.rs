use std::{collections::HashMap, io::Cursor, sync::Arc};

use anyhow::{Context as _, Result, bail};
use office_oxide::{Document, DocumentFormat, DocumentIR};

use crate::{
    document::{
        AssetRole, Block, BlockDocument, BookDocument, BookFormat, BookSource, ContentUnit,
        ContentUnitKind, SourceLocator, TocNode, TocTarget, deterministic_id,
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
        // Excel 工作簿的 core properties 常常为空或沿用模板标题，导入时一律以
        // 文件名作为书名，避免把模板里的标题当成用户图书名。
        let title = if format == BookFormat::Xlsx {
            safe_title(None, source.stem().as_str())
        } else {
            safe_title(ir.metadata.title.as_deref(), source.stem().as_str())
        };

        let mut units = Vec::new();
        let mut total_text_bytes = 0;
        let mut imported_assets = vec![original.clone()];
        if ir.sections.is_empty() {
            let html = parsed.to_html();
            push_office_units(
                &book_id,
                format,
                1,
                None,
                &html,
                Vec::new(),
                None,
                limits,
                &mut total_text_bytes,
                &mut units,
            )?;
        } else {
            for (section_index, section) in ir.sections.iter().enumerate() {
                let mut one_section = DocumentIR {
                    metadata: ir.metadata.clone(),
                    sections: vec![section.clone()],
                };
                if matches!(format, BookFormat::Doc | BookFormat::Docx) {
                    // Word section titles repeat the first body heading in the
                    // parser IR. Keep them as fallback metadata; rendering an
                    // extra h2 would duplicate content and create a false split.
                    one_section.sections[0].title = None;
                }
                let (html, worksheet_range) = if format == BookFormat::Xlsx {
                    if let Some(xlsx) = parsed.as_xlsx() {
                        if let Some(worksheet) = xlsx.worksheets.get(section_index) {
                            let used_range = worksheet_used_range(worksheet, xlsx);
                            (
                                worksheet_grid_html(worksheet, xlsx, used_range, limits)?,
                                used_range.map(|range| range.to_a1()),
                            )
                        } else {
                            (one_section.to_html(), None)
                        }
                    } else {
                        (one_section.to_html(), None)
                    }
                } else {
                    (one_section.to_html(), None)
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
                    &html,
                    image_assets,
                    worksheet_range.as_deref(),
                    limits,
                    &mut total_text_bytes,
                    &mut units,
                )?;
            }
        }
        if units.is_empty() {
            let unit_id = deterministic_id("unit", format!("{book_id}\0empty").as_bytes());
            units.push(
                ContentUnit::empty(unit_id, unit_kind(format), title.clone())
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

fn worksheet_grid_html(
    worksheet: &office_oxide::xlsx::Worksheet,
    workbook: &office_oxide::xlsx::XlsxDocument,
    range: Option<WorksheetRange>,
    limits: &ImportLimits,
) -> Result<String> {
    let mut html = String::new();
    if let Some(range) = range {
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

        html.push_str("<table><thead>");
        push_worksheet_html_row(&mut html, range.start_row, range, &values, "th");
        html.push_str("</thead><tbody>");
        for row in range.start_row + 1..=range.end_row {
            push_worksheet_html_row(&mut html, row, range, &values, "td");
            if html.len() > limits.max_unit_text_bytes {
                bail!("Office content unit exceeds the text safety limit");
            }
        }
        html.push_str("</tbody></table>");
    }
    for shape in &worksheet.text_shapes {
        let text = shape.text.trim();
        if text.is_empty() {
            continue;
        }
        html.push_str("<p>");
        push_escaped_html_text(&mut html, text);
        html.push_str("</p>");
        if html.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
    }
    if html.len() > limits.max_unit_text_bytes {
        bail!("Office content unit exceeds the text safety limit");
    }
    Ok(html)
}

fn push_worksheet_html_row(
    output: &mut String,
    row: u32,
    range: WorksheetRange,
    values: &HashMap<(u32, u32), String>,
    cell_tag: &str,
) {
    output.push_str("<tr>");
    for col in range.start_col..=range.end_col {
        output.push('<');
        output.push_str(cell_tag);
        output.push('>');
        if let Some(value) = values.get(&(row, col)) {
            push_escaped_html_text(output, value);
        }
        output.push_str("</");
        output.push_str(cell_tag);
        output.push('>');
    }
    output.push_str("</tr>");
}

fn push_escaped_html_text(output: &mut String, value: &str) {
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                output.push_str("<br>");
            }
            '\n' => output.push_str("<br>"),
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
    html: &str,
    image_assets: Vec<String>,
    worksheet_range: Option<&str>,
    limits: &ImportLimits,
    total_text_bytes: &mut usize,
    units: &mut Vec<ContentUnit>,
) -> Result<()> {
    if html.len()
        > limits
            .max_total_text_bytes
            .saturating_sub(*total_text_bytes)
    {
        bail!("Office document exceeds the total text safety limit");
    }
    if !matches!(format, BookFormat::Doc | BookFormat::Docx)
        && html.len() > limits.max_unit_text_bytes
    {
        bail!("Office content unit exceeds the text safety limit");
    }
    // Section-scoped IDs remain unique when one Word section is split into
    // several chapters. Splitting the AST also keeps literal '#' characters,
    // heading markup and nested table/list content intact.
    let section_id = deterministic_id(
        "office-section",
        format!("{book_id}\0{section_index}").as_bytes(),
    );
    let parsed = crate::markup::parse_source_for_unit(html, &section_id)
        .context("failed to normalize Office HTML")?;
    let chunks = if matches!(format, BookFormat::Doc | BookFormat::Docx) {
        split_word_document(parsed.document, native_title)
    } else {
        vec![(
            safe_title(
                native_title,
                &format!("{} {section_index}", unit_label(format)),
            ),
            parsed.document,
        )]
    };
    for (chunk_index, (title, mut document)) in chunks.into_iter().enumerate() {
        if units.len() >= limits.max_units {
            bail!("Office document contains too many content units");
        }
        let unit_id = deterministic_id(
            "unit",
            format!("{book_id}\0{section_index}\0{chunk_index}\0{title}").as_bytes(),
        );
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
        let source = crate::markup::serialize_source(&document)
            .context("failed to serialize normalized Office content")?;
        if source.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
        if source.len()
            > limits
                .max_total_text_bytes
                .saturating_sub(*total_text_bytes)
        {
            bail!("Office document exceeds the total text safety limit");
        }
        // Chapter boundaries establish the final unit identity. Normalize every
        // block, including appended images, with that same seed used by future
        // source edits so an unchanged save keeps block IDs stable.
        let parsed = crate::markup::parse_source_for_unit(&source, &unit_id)
            .context("failed to normalize final Office content unit")?;
        let source = parsed.canonical_source;
        let document = parsed.document;
        if source.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
        *total_text_bytes = total_text_bytes
            .checked_add(source.len())
            .context("total Office text size overflowed")?;
        if *total_text_bytes > limits.max_total_text_bytes {
            bail!("Office document exceeds the total text safety limit");
        }
        units.push(
            ContentUnit::new(unit_id, unit_kind(format), title, source, document)
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

fn split_word_document(
    document: BlockDocument,
    native_title: Option<&str>,
) -> Vec<(String, BlockDocument)> {
    let mut result = Vec::new();
    let mut current_title = safe_title(native_title, "Introduction");
    let mut current = Vec::new();
    for block in document.blocks {
        if matches!(&block, Block::Heading { level: 1..=3, .. }) {
            if !current.is_empty() {
                result.push((
                    current_title,
                    BlockDocument::new(std::mem::take(&mut current)),
                ));
            }
            current_title = safe_title(Some(&block.plain_text()), "Section");
        }
        current.push(block);
    }
    if !current.is_empty() {
        result.push((current_title, BlockDocument::new(current)));
    }
    if result.is_empty() {
        result.push((
            safe_title(native_title, "Section 1"),
            BlockDocument::default(),
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
    use office_oxide::{
        docx::write::DocxWriter,
        xlsx::write::{CellData, XlsxWriter},
    };
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
        assert!(units[0].source.contains("moye-asset:office-image-id"));
        assert!(
            units[0]
                .document
                .referenced_asset_ids()
                .contains(&"office-image-id")
        );
        let reparsed =
            crate::markup::parse_source_for_unit(&units[0].source, &units[0].id).unwrap();
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
            office_oxide::xlsx::XlsxDocument::from_reader(Cursor::new(output.into_inner()))
                .unwrap();
        let value = "A & <script>literal</script> | \\ path\r\nnext\rthird";
        // Supply parsed text directly: pinned office_oxide 0.1.9 loses XML
        // entity events, a parser issue preceding the HTML conversion tested here.
        workbook.worksheets[0].rows[0].cells[0].value =
            office_oxide::xlsx::CellValue::String(value.into());
        let worksheet = &workbook.worksheets[0];
        let range = worksheet_used_range(worksheet, &workbook);
        assert_eq!(range.unwrap().to_a1(), "B2:B2");
        let html =
            worksheet_grid_html(worksheet, &workbook, range, &ImportLimits::default()).unwrap();
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
        let Block::Table { header, rows, .. } = &imported.document.units[0].document.blocks[0]
        else {
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
}
