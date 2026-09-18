//! Decoding images and collecting the ones a page needs.

use super::*;

pub(crate) fn append_pdf_image(
    document: &BookDocument,
    resolver: &dyn AssetResolver,
    asset_id: &str,
    emitted: &mut HashSet<String>,
    pages: &mut Vec<PreparedPdfPage>,
) -> Result<()> {
    if !emitted.insert(asset_id.to_string()) {
        return Ok(());
    }
    let metadata = document
        .find_asset(asset_id)
        .with_context(|| format!("PDF 引用了不存在的图片资源 {asset_id}"))?;
    let bytes = resolve_verified(metadata, resolver)?;
    match decode_pdf_image(&bytes) {
        Ok(image) => pages.push(PreparedPdfPage::Image(image)),
        Err(error) => tracing::warn!(
            asset_id,
            media_type = %metadata.media_type,
            %error,
            "PDF 导出跳过了无法解码的图片，替代文本仍保留在正文中"
        ),
    }
    Ok(())
}

pub(crate) fn decode_pdf_image(bytes: &[u8]) -> Result<PdfImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("无法识别图片格式")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PDF_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_PDF_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_PDF_IMAGE_ALLOCATION);
    reader.limits(limits);
    let rgba = reader.decode().context("无法解码图片")?.to_rgba8();
    let (width, height) = rgba.dimensions();
    ensure!(width > 0 && height > 0, "图片尺寸不能为空");
    let raw = rgba.into_raw();
    let mut rgb = Vec::with_capacity(raw.len() / 4 * 3);
    for pixel in raw.chunks_exact(4) {
        let alpha = u16::from(pixel[3]);
        for channel in &pixel[..3] {
            let composited = (u16::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255;
            rgb.push(composited as u8);
        }
    }
    Ok(PdfImage { width, height, rgb })
}

pub(crate) fn collect_block_image_ids<'a>(blocks: &'a [Block], output: &mut Vec<&'a str>) {
    for block in blocks {
        match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                collect_inline_image_ids(content, output)
            }
            Block::BlockQuote { blocks, .. } => collect_block_image_ids(blocks, output),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                for item in items {
                    collect_block_image_ids(&item.blocks, output);
                }
            }
            Block::Table { header, rows, .. } => {
                for row in header.iter().chain(rows.iter()) {
                    for cell in &row.cells {
                        collect_inline_image_ids(&cell.content, output);
                    }
                }
            }
            Block::Image { asset_id, .. } => output.push(asset_id),
            Block::Video {
                poster_asset_id: Some(asset_id),
                ..
            } => output.push(asset_id),
            Block::Video { .. }
            | Block::Audio { .. }
            | Block::CodeBlock { .. }
            | Block::ThematicBreak { .. }
            | Block::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn collect_inline_image_ids<'a>(inlines: &'a [Inline], output: &mut Vec<&'a str>) {
    for inline in inlines {
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => collect_inline_image_ids(content, output),
            Inline::Image { asset_id, .. } => output.push(asset_id),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn append_text_pages(pages: &mut Vec<PreparedPdfPage>, text: &str) {
    let mut lines = Vec::new();
    for source_line in text.lines() {
        if source_line.trim().is_empty() {
            lines.push(String::new());
        } else {
            lines.extend(wrap_visual_line(source_line, PDF_LINE_WIDTH));
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    for page in lines.chunks(PDF_LINES_PER_PAGE) {
        pages.push(PreparedPdfPage::Text(page.to_vec()));
    }
}
