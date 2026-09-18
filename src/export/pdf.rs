//! Preparing and writing the PDF export.

use super::*;

pub(crate) enum PreparedPdfPage {
    Text(Vec<String>),
    Image(PdfImage),
}

pub(crate) struct PdfImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgb: Vec<u8>,
}

pub(crate) fn export_pdf(document: &BookDocument, resolver: &dyn AssetResolver) -> Result<Vec<u8>> {
    let mut pages = Vec::new();
    let mut title_text = document.title.clone();
    if !document.authors.is_empty() {
        title_text.push_str("\n\n");
        title_text.push_str(&document.authors.join(", "));
    }
    if let Some(description) = document.description.as_deref() {
        title_text.push_str("\n\n");
        title_text.push_str(description);
    }
    append_text_pages(&mut pages, &title_text);

    let mut emitted_images = HashSet::new();
    if let Some(cover_id) = document.cover_asset_id.as_deref() {
        append_pdf_image(
            document,
            resolver,
            cover_id,
            &mut emitted_images,
            &mut pages,
        )?;
    }
    for unit in &document.units {
        let mut text = if unit_body_starts_with_title(unit) {
            String::new()
        } else {
            unit.title.clone()
        };
        let body = unit.plain_text();
        if !body.trim().is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&body);
        }
        append_text_pages(&mut pages, &text);

        let mut unit_images = Vec::new();
        collect_block_image_ids(&unit.document.blocks, &mut unit_images);
        for asset_id in unit_images {
            append_pdf_image(
                document,
                resolver,
                asset_id,
                &mut emitted_images,
                &mut pages,
            )?;
        }
    }
    build_pdf(document, &pages)
}
