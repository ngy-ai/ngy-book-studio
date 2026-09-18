//! Splitting a Word document into units and labelling them.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_office_units(
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

pub(crate) fn split_word_document(
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

pub(crate) fn extract_section_images(
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

pub(crate) fn unit_kind(format: BookFormat) -> ContentUnitKind {
    match format {
        BookFormat::Pptx => ContentUnitKind::Slide,
        BookFormat::Xlsx => ContentUnitKind::Worksheet,
        _ => ContentUnitKind::Chapter,
    }
}

pub(crate) fn unit_label(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Pptx => "Slide",
        BookFormat::Xlsx => "Sheet",
        _ => "Section",
    }
}

pub(crate) fn source_locator(
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
