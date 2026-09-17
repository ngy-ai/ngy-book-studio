//! Preparing and bounding the images a rendered page may publish.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_structural_image_pages(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
    descriptor: &RendererDescriptor,
    profile_id: &str,
    emitter: &RenderPageEmitter,
    total_bytes: &mut u64,
) -> Result<()> {
    for (image_index, image_reference) in visual_image_references(&unit.document.blocks)
        .into_iter()
        .enumerate()
    {
        control.checkpoint().map_err(anyhow::Error::new)?;
        let (page_index, should_render) = emitter.claim_page();
        if !should_render {
            continue;
        }
        let image = load_visual_image(request, unit, control, image_reference).await?;
        add_visual_page_bytes(total_bytes, image.prepared.bytes.len())?;
        let metadata = render_metadata(request, unit, descriptor, profile_id);
        let id_seed = format!(
            "{}\0{}\0{}\0image\0{}\0{}\0{}\0{}",
            request.document.id,
            request.document.revision.get(),
            unit.id,
            image.reference.block_id,
            image_index,
            image.reference.asset_id,
            blake3::hash(&image.prepared.bytes).to_hex()
        );
        let mut locator =
            DocumentLocator::block(&request.document.id, &unit.id, image.reference.block_id);
        if let Some(source) = unit.source_locator.clone() {
            locator = locator.with_source(source);
        }
        emitter
            .emit(RenderedVisualPage {
                id: deterministic_id("visual-page", id_seed),
                page_index,
                content_unit_id: Some(unit.id.clone()),
                width: image.prepared.width,
                height: image.prepared.height,
                media_type: image.prepared.media_type,
                bytes: image.prepared.bytes,
                locator,
                metadata,
            })
            .await?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

pub(crate) async fn load_visual_images(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
) -> Result<Vec<LoadedVisualImage>> {
    let image_references = visual_image_references(&unit.document.blocks);
    if image_references.is_empty() {
        return Ok(Vec::new());
    }
    let mut loaded = Vec::with_capacity(image_references.len());
    for image_reference in image_references {
        loaded.push(load_visual_image(request, unit, control, image_reference).await?);
    }
    Ok(loaded)
}

pub(crate) async fn load_visual_image(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
    image_reference: VisualImageReference,
) -> Result<LoadedVisualImage> {
    control.checkpoint().map_err(anyhow::Error::new)?;
    let asset_source = request
        .asset_source
        .as_ref()
        .context("视觉渲染缺少受书籍范围约束的图片来源")?;
    let payload = asset_source
        .load_asset(
            request.document.id.clone(),
            image_reference.asset_id.clone(),
        )
        .await
        .with_context(|| {
            format!(
                "无法加载内容单元 {} 的图片 {}",
                unit.id, image_reference.asset_id
            )
        })?;
    ensure!(!payload.bytes.is_empty(), "视觉图片内容不能为空");
    ensure!(
        payload.bytes.len() <= MAX_VISUAL_IMAGE_BYTES,
        "视觉图片超过 {} 字节上限",
        MAX_VISUAL_IMAGE_BYTES
    );
    let media_type = canonical_visual_image_media_type(&payload.media_type)
        .with_context(|| format!("视觉图片 MIME 不受支持：{}", payload.media_type))?;
    let profile = request.profile.clone();
    let image_control = control.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_visual_image(payload, media_type, &profile, &image_control)
    })
    .await
    .context("视觉图片检查线程异常退出")??;
    Ok(LoadedVisualImage {
        reference: image_reference,
        prepared,
    })
}

pub(crate) fn render_metadata(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    descriptor: &RendererDescriptor,
    profile_id: &str,
) -> RenderMetadata {
    RenderMetadata {
        renderer: descriptor.renderer.clone(),
        renderer_version: descriptor.version.clone(),
        fidelity: descriptor.fidelity,
        document_revision: request.document.revision,
        unit_revision: unit.revision,
        profile_id: profile_id.to_string(),
    }
}

pub(crate) fn located_unit_lines(
    unit: &ContentUnit,
    viewport_width: u32,
    control: &RenderControl,
) -> Result<Vec<LocatedLine>> {
    let mut lines = wrap_located_text(&unit.title, viewport_width, None, control)?;
    if !unit.document.blocks.is_empty() {
        lines.push(LocatedLine {
            text: String::new(),
            block_id: None,
            start_byte: 0,
            end_byte: 0,
        });
    }
    for (index, block) in unit.document.blocks.iter().enumerate() {
        if index > 0 {
            lines.push(LocatedLine {
                text: String::new(),
                block_id: None,
                start_byte: 0,
                end_byte: 0,
            });
        }
        let text = block.plain_text();
        if !text.trim().is_empty() {
            lines.extend(wrap_located_text(
                &text,
                viewport_width,
                Some(block.id()),
                control,
            )?);
        }
    }
    if lines.is_empty() {
        lines.push(LocatedLine {
            text: String::new(),
            block_id: None,
            start_byte: 0,
            end_byte: 0,
        });
    }
    Ok(lines)
}

pub(crate) fn locator_for_lines(
    book_id: &str,
    unit: &ContentUnit,
    lines: &[LocatedLine],
) -> DocumentLocator {
    let first = lines.iter().enumerate().find(|(_, line)| {
        line.block_id.is_some() && line.end_byte > line.start_byte && !line.text.trim().is_empty()
    });
    let mut locator = if let Some((first_index, first)) = first {
        let block_id = first.block_id.as_deref().expect("located block line");
        let end_byte = lines[first_index..]
            .iter()
            .take_while(|line| line.block_id.as_deref() == Some(block_id))
            .map(|line| line.end_byte)
            .max()
            .unwrap_or(first.end_byte);
        DocumentLocator::text(book_id, &unit.id, block_id, first.start_byte, end_byte)
    } else if let Some(block_id) = lines.iter().find_map(|line| line.block_id.as_deref()) {
        DocumentLocator::block(book_id, &unit.id, block_id)
    } else {
        DocumentLocator::unit(book_id, &unit.id)
    };
    if let Some(source) = unit.source_locator.clone() {
        locator = locator.with_source(source);
    }
    locator
}

pub(crate) fn wrap_located_text(
    text: &str,
    viewport_width: u32,
    block_id: Option<&str>,
    control: &RenderControl,
) -> Result<Vec<LocatedLine>> {
    let max_columns = ((viewport_width.saturating_sub(120)) / 16).max(12) as usize;
    let mut lines = Vec::new();
    let mut source_offset = 0_usize;
    for (line_index, raw_line) in text.split_inclusive('\n').enumerate() {
        if line_index % 64 == 0 {
            control.checkpoint().map_err(anyhow::Error::new)?;
        }
        let without_newline = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let source_line = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        if source_line.is_empty() {
            lines.push(LocatedLine {
                text: String::new(),
                block_id: block_id.map(str::to_string),
                start_byte: source_offset as u64,
                end_byte: source_offset as u64,
            });
            source_offset += raw_line.len();
            continue;
        }

        let mut segment_start = 0_usize;
        let mut columns = 0_usize;
        for (character_index, (byte_index, character)) in source_line.char_indices().enumerate() {
            if character_index % 4_096 == 0 {
                control.checkpoint().map_err(anyhow::Error::new)?;
            }
            let width = if character.is_ascii() { 1 } else { 2 };
            if columns > 0 && columns + width > max_columns {
                lines.push(LocatedLine {
                    text: source_line[segment_start..byte_index].to_string(),
                    block_id: block_id.map(str::to_string),
                    start_byte: (source_offset + segment_start) as u64,
                    end_byte: (source_offset + byte_index) as u64,
                });
                segment_start = byte_index;
                columns = 0;
            }
            columns += width;
        }
        lines.push(LocatedLine {
            text: source_line[segment_start..].to_string(),
            block_id: block_id.map(str::to_string),
            start_byte: (source_offset + segment_start) as u64,
            end_byte: (source_offset + source_line.len()) as u64,
        });
        source_offset += raw_line.len();
    }
    if lines.is_empty() {
        lines.push(LocatedLine {
            text: String::new(),
            block_id: block_id.map(str::to_string),
            start_byte: 0,
            end_byte: 0,
        });
    }
    control.checkpoint().map_err(anyhow::Error::new)?;
    Ok(lines)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisualImageReference {
    pub(crate) asset_id: String,
    pub(crate) block_id: String,
}

pub(crate) fn visual_image_references(blocks: &[Block]) -> Vec<VisualImageReference> {
    let mut output = Vec::new();
    collect_block_image_references(blocks, &mut output);
    output
}

pub(crate) fn collect_block_image_references(
    blocks: &[Block],
    output: &mut Vec<VisualImageReference>,
) {
    for block in blocks {
        match block {
            Block::Paragraph { id, content } | Block::Heading { id, content, .. } => {
                collect_inline_image_references(content, id, output);
            }
            Block::BlockQuote { blocks, .. } => collect_block_image_references(blocks, output),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                for item in items {
                    collect_block_image_references(&item.blocks, output);
                }
            }
            Block::Table { id, header, rows } => {
                for row in header.iter().chain(rows.iter()) {
                    for cell in &row.cells {
                        collect_inline_image_references(&cell.content, id, output);
                    }
                }
            }
            Block::Image { id, asset_id, .. } => output.push(VisualImageReference {
                asset_id: asset_id.clone(),
                block_id: id.clone(),
            }),
            Block::Video {
                id,
                poster_asset_id: Some(asset_id),
                ..
            } => output.push(VisualImageReference {
                asset_id: asset_id.clone(),
                block_id: id.clone(),
            }),
            Block::Video { .. }
            | Block::Audio { .. }
            | Block::CodeBlock { .. }
            | Block::ThematicBreak { .. }
            | Block::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn collect_inline_image_references(
    inlines: &[Inline],
    block_id: &str,
    output: &mut Vec<VisualImageReference>,
) {
    for inline in inlines {
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => {
                collect_inline_image_references(content, block_id, output);
            }
            Inline::Image { asset_id, .. } => output.push(VisualImageReference {
                asset_id: asset_id.clone(),
                block_id: block_id.to_string(),
            }),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn canonical_visual_image_media_type(media_type: &str) -> Option<&'static str> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/webp" => Some("image/webp"),
        "image/gif" => Some("image/gif"),
        "image/svg+xml" => Some("image/svg+xml"),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct PreparedVisualImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) media_type: String,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn prepare_visual_image(
    payload: VisualAssetPayload,
    media_type: &'static str,
    profile: &RenderProfile,
    control: &RenderControl,
) -> Result<PreparedVisualImage> {
    control.checkpoint().map_err(anyhow::Error::new)?;
    if media_type == "image/svg+xml" {
        return rasterize_svg(&payload.bytes, profile, false, control);
    }

    let expected_format = match media_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        "image/gif" => image::ImageFormat::Gif,
        _ => anyhow::bail!("视觉图片 MIME 不受支持：{media_type}"),
    };
    let detected_format = image::guess_format(&payload.bytes).context("无法识别视觉图片格式")?;
    ensure!(
        detected_format == expected_format,
        "视觉图片 MIME 与文件签名不一致"
    );
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_RASTER_DIMENSION);
    limits.max_image_height = Some(MAX_RASTER_DIMENSION);
    limits.max_alloc = Some(MAX_RASTER_PIXELS.saturating_mul(4));
    let mut reader = image::ImageReader::with_format(Cursor::new(&payload.bytes), expected_format);
    reader.limits(limits);
    let decoded = reader.decode().context("无法解码视觉图片")?;
    let (width, height) = (decoded.width(), decoded.height());
    validate_raster_dimensions(width, height)?;
    drop(decoded);
    control.checkpoint().map_err(anyhow::Error::new)?;
    Ok(PreparedVisualImage {
        width,
        height,
        media_type: media_type.to_string(),
        bytes: payload.bytes,
    })
}

pub(crate) fn render_structural_png(
    lines: &[String],
    profile: &RenderProfile,
    control: &RenderControl,
) -> Result<PreparedVisualImage> {
    let svg = render_svg_page(lines, profile);
    rasterize_svg(svg.as_bytes(), profile, true, control)
}

pub(crate) fn render_slide_png(
    lines: &[String],
    images: Vec<PreparedVisualImage>,
    profile: &RenderProfile,
    control: &RenderControl,
) -> Result<PreparedVisualImage> {
    control.checkpoint().map_err(anyhow::Error::new)?;
    let svg = render_slide_svg_page(lines, profile, images.len());
    let background = rasterize_svg(svg.as_bytes(), profile, true, control)?;
    if images.is_empty() {
        return Ok(background);
    }

    let mut canvas =
        image::load_from_memory_with_format(&background.bytes, image::ImageFormat::Png)
            .context("无法解码幻灯片背景")?
            .to_rgba8();
    let count = images.len();
    let columns = (count as f64).sqrt().ceil().max(1.0) as u32;
    let rows = (count as u32).div_ceil(columns);
    let margin = (canvas.width() / 40).clamp(8, 32);
    let image_top = canvas.height().saturating_mul(52) / 100;
    let available_width = canvas.width().saturating_sub(margin.saturating_mul(2));
    let available_height = canvas
        .height()
        .saturating_sub(image_top)
        .saturating_sub(margin);
    let cell_width = (available_width / columns).max(1);
    let cell_height = (available_height / rows).max(1);

    for (index, prepared) in images.into_iter().enumerate() {
        if index % 8 == 0 {
            control.checkpoint().map_err(anyhow::Error::new)?;
        }
        let source = decode_prepared_visual_image(&prepared)?;
        let inner_width = cell_width.saturating_sub(margin).max(1);
        let inner_height = cell_height.saturating_sub(margin).max(1);
        let scale = (f64::from(inner_width) / f64::from(source.width()))
            .min(f64::from(inner_height) / f64::from(source.height()))
            .min(1.0);
        let width = (f64::from(source.width()) * scale).round().max(1.0) as u32;
        let height = (f64::from(source.height()) * scale).round().max(1.0) as u32;
        let resized = image::imageops::resize(
            &source,
            width,
            height,
            image::imageops::FilterType::Triangle,
        );
        let column = index as u32 % columns;
        let row = index as u32 / columns;
        let x = margin + column * cell_width + cell_width.saturating_sub(width) / 2;
        let y = image_top + row * cell_height + cell_height.saturating_sub(height) / 2;
        image::imageops::overlay(&mut canvas, &resized, i64::from(x), i64::from(y));
    }
    control.checkpoint().map_err(anyhow::Error::new)?;
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut output, image::ImageFormat::Png)
        .context("无法编码合成幻灯片 PNG")?;
    let bytes = output.into_inner();
    ensure!(
        bytes.len() <= MAX_VISUAL_IMAGE_BYTES,
        "合成幻灯片 PNG 超过 {MAX_VISUAL_IMAGE_BYTES} 字节上限"
    );
    ensure!(bytes.starts_with(PNG_SIGNATURE), "幻灯片没有输出 PNG");
    Ok(PreparedVisualImage {
        width: background.width,
        height: background.height,
        media_type: "image/png".to_string(),
        bytes,
    })
}

pub(crate) fn decode_prepared_visual_image(
    prepared: &PreparedVisualImage,
) -> Result<image::RgbaImage> {
    let format = match prepared.media_type.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        "image/gif" => image::ImageFormat::Gif,
        other => anyhow::bail!("无法合成视觉图片 MIME：{other}"),
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_RASTER_DIMENSION);
    limits.max_image_height = Some(MAX_RASTER_DIMENSION);
    limits.max_alloc = Some(MAX_RASTER_PIXELS.saturating_mul(4));
    let mut reader = image::ImageReader::with_format(Cursor::new(&prepared.bytes), format);
    reader.limits(limits);
    Ok(reader
        .decode()
        .context("无法解码待合成的视觉图片")?
        .to_rgba8())
}

pub(crate) fn rasterize_svg(
    svg: &[u8],
    profile: &RenderProfile,
    allow_upscale: bool,
    control: &RenderControl,
) -> Result<PreparedVisualImage> {
    ensure!(!svg.is_empty(), "SVG 视觉图片不能为空");
    ensure!(
        svg.len() <= MAX_STRUCTURAL_SVG_BYTES,
        "SVG 视觉图片超过 {MAX_STRUCTURAL_SVG_BYTES} 字节上限"
    );
    ensure!(
        !svg.starts_with(&[0x1f, 0x8b]),
        "不支持压缩 SVG，避免解压后绕过视觉页面大小上限"
    );
    control.checkpoint().map_err(anyhow::Error::new)?;
    let options = isolated_svg_options();
    let tree = resvg::usvg::Tree::from_data(svg, &options).context("无法解析视觉页面 SVG")?;
    let source_size = tree.size();
    let (width, height) = fitted_raster_dimensions(
        f64::from(source_size.width()),
        f64::from(source_size.height()),
        profile,
        allow_upscale,
    )?;
    control.checkpoint().map_err(anyhow::Error::new)?;
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(width, height).context("无法分配视觉页面像素")?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        width as f32 / source_size.width(),
        height as f32 / source_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    control.checkpoint().map_err(anyhow::Error::new)?;
    let bytes = pixmap.encode_png().context("无法编码视觉页面 PNG")?;
    ensure!(
        bytes.len() <= MAX_VISUAL_IMAGE_BYTES,
        "视觉页面 PNG 超过 {MAX_VISUAL_IMAGE_BYTES} 字节上限"
    );
    ensure!(bytes.starts_with(PNG_SIGNATURE), "视觉页面没有输出 PNG");
    control.checkpoint().map_err(anyhow::Error::new)?;
    Ok(PreparedVisualImage {
        width,
        height,
        media_type: "image/png".to_string(),
        bytes,
    })
}

pub(crate) fn isolated_svg_options() -> resvg::usvg::Options<'static> {
    static SYSTEM_FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    let fonts = SYSTEM_FONTS.get_or_init(|| {
        let mut database = resvg::usvg::fontdb::Database::new();
        database.load_system_fonts();
        Arc::new(database)
    });
    resvg::usvg::Options {
        resources_dir: None,
        font_family: "sans-serif".to_string(),
        languages: vec!["zh-CN".to_string(), "zh".to_string(), "en".to_string()],
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: Arc::clone(fonts),
        ..resvg::usvg::Options::default()
    }
}

pub(crate) fn fitted_raster_dimensions(
    source_width: f64,
    source_height: f64,
    profile: &RenderProfile,
    allow_upscale: bool,
) -> Result<(u32, u32)> {
    ensure!(
        source_width.is_finite()
            && source_width > 0.0
            && source_height.is_finite()
            && source_height > 0.0,
        "SVG 页面尺寸无效"
    );
    // Large coordinate systems can make path processing unexpectedly costly
    // even when the destination pixmap is bounded.
    ensure!(
        source_width <= 1_000_000.0 && source_height <= 1_000_000.0,
        "SVG 页面坐标尺寸超过安全上限"
    );
    let target_width = (f64::from(profile.viewport_width) * profile.scale())
        .clamp(1.0, f64::from(MAX_RASTER_DIMENSION));
    let target_height = (f64::from(profile.viewport_height) * profile.scale())
        .clamp(1.0, f64::from(MAX_RASTER_DIMENSION));
    let mut fit = (target_width / source_width).min(target_height / source_height);
    if !allow_upscale {
        fit = fit.min(1.0);
    }
    let mut width = (source_width * fit).round().max(1.0) as u32;
    let mut height = (source_height * fit).round().max(1.0) as u32;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_RASTER_PIXELS {
        let pixel_scale = (MAX_RASTER_PIXELS as f64 / pixels as f64).sqrt();
        width = (f64::from(width) * pixel_scale).floor().max(1.0) as u32;
        height = (f64::from(height) * pixel_scale).floor().max(1.0) as u32;
    }
    validate_raster_dimensions(width, height)?;
    Ok((width, height))
}

pub(crate) fn validate_raster_dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(width > 0 && height > 0, "视觉图片尺寸无效");
    ensure!(
        width <= MAX_RASTER_DIMENSION && height <= MAX_RASTER_DIMENSION,
        "视觉图片尺寸超过 {MAX_RASTER_DIMENSION} 像素边长上限"
    );
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS,
        "视觉图片超过 {MAX_RASTER_PIXELS} 像素上限"
    );
    Ok(())
}
