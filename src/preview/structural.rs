//! The structural renderer: text pages, slides and the images they carry.

use super::*;

/// Portable, format-aware structural renderer. Flow content is paginated,
/// slides stay atomic, and worksheet tables are tiled in both dimensions.
/// Text is first written to an escaped, resource-isolated SVG and then
/// rasterized on Tokio's blocking pool. Only PNG pages are published.
#[derive(Clone, Copy, Debug, Default)]
pub struct StructuralPngRenderer;

impl VisualRenderer for StructuralPngRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: "ngy-structural-png".to_string(),
            version: concat!(env!("CARGO_PKG_VERSION"), "+resvg-0.45.1").to_string(),
            fidelity: RenderFidelity::Structural,
        }
    }

    fn render_units_resumable<'a>(
        &'a self,
        request: RenderUnitsRequest,
        control: RenderControl,
        emitter: RenderPageEmitter,
    ) -> PreviewFuture<'a, usize> {
        Box::pin(async move {
            request.profile.validate()?;
            request
                .document
                .validate()
                .context("无法渲染无效的图书文档")?;
            let descriptor = self.descriptor();
            let profile_id = request.profile.stable_id();
            let selected = select_units(&request)?;
            let mut total_bytes = 0_u64;

            for unit in selected {
                control.checkpoint().map_err(anyhow::Error::new)?;
                match unit.kind {
                    ContentUnitKind::Slide => {
                        render_structural_slide(
                            &request,
                            unit,
                            &control,
                            &descriptor,
                            &profile_id,
                            &emitter,
                            &mut total_bytes,
                        )
                        .await?;
                    }
                    ContentUnitKind::Worksheet => {
                        render_structural_worksheet(
                            &request,
                            unit,
                            &control,
                            &descriptor,
                            &profile_id,
                            &emitter,
                            &mut total_bytes,
                        )
                        .await?;
                    }
                    _ => {
                        render_structural_flow(
                            &request,
                            unit,
                            &control,
                            &descriptor,
                            &profile_id,
                            &emitter,
                            &mut total_bytes,
                        )
                        .await?;
                    }
                }
            }
            let total_pages = emitter.total_pages();
            ensure!(
                emitter.resume_from() <= total_pages,
                "视觉任务断点超过当前 renderer 的页面总数"
            );
            Ok(total_pages)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocatedLine {
    pub(crate) text: String,
    pub(crate) block_id: Option<String>,
    pub(crate) start_byte: u64,
    pub(crate) end_byte: u64,
}

#[derive(Debug)]
pub(crate) struct LoadedVisualImage {
    pub(crate) reference: VisualImageReference,
    pub(crate) prepared: PreparedVisualImage,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn render_structural_flow(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
    descriptor: &RendererDescriptor,
    profile_id: &str,
    emitter: &RenderPageEmitter,
    total_bytes: &mut u64,
) -> Result<()> {
    let render_unit = unit.clone();
    let viewport_width = request.profile.viewport_width;
    let wrap_control = control.clone();
    let source_lines = tokio::task::spawn_blocking(move || {
        located_unit_lines(&render_unit, viewport_width, &wrap_control)
    })
    .await
    .context("结构化页面排版线程异常退出")??;
    let lines_per_page =
        ((request.profile.viewport_height.saturating_sub(160)) / 32).max(1) as usize;
    ensure!(
        source_lines.len().div_ceil(lines_per_page) <= MAX_STRUCTURAL_PAGES_PER_UNIT,
        "单个内容单元的结构化页面超过 {MAX_STRUCTURAL_PAGES_PER_UNIT} 页上限"
    );
    for (unit_page, lines) in source_lines.chunks(lines_per_page).enumerate() {
        control.checkpoint().map_err(anyhow::Error::new)?;
        let (page_index, should_render) = emitter.claim_page();
        if !should_render {
            continue;
        }
        let page_lines = lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let locator = locator_for_lines(&request.document.id, unit, lines);
        let profile = request.profile.clone();
        let raster_control = control.clone();
        let raster = tokio::task::spawn_blocking(move || {
            render_structural_png(&page_lines, &profile, &raster_control)
        })
        .await
        .context("结构化页面 PNG 渲染线程异常退出")??;
        add_visual_page_bytes(total_bytes, raster.bytes.len())?;
        let metadata = render_metadata(request, unit, descriptor, profile_id);
        let id_seed = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            request.document.id,
            request.document.revision.get(),
            unit.id,
            unit_page,
            metadata.renderer_version,
            metadata.profile_id
        );
        emitter
            .emit(RenderedVisualPage {
                id: deterministic_id("visual-page", id_seed),
                page_index,
                content_unit_id: Some(unit.id.clone()),
                width: raster.width,
                height: raster.height,
                media_type: "image/png".to_string(),
                bytes: raster.bytes,
                locator,
                metadata,
            })
            .await?;
        tokio::task::yield_now().await;
    }
    append_structural_image_pages(
        request,
        unit,
        control,
        descriptor,
        profile_id,
        emitter,
        total_bytes,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn render_structural_slide(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
    descriptor: &RendererDescriptor,
    profile_id: &str,
    emitter: &RenderPageEmitter,
    total_bytes: &mut u64,
) -> Result<()> {
    let render_unit = unit.clone();
    let viewport_width = request.profile.viewport_width;
    let wrap_control = control.clone();
    let source_lines = tokio::task::spawn_blocking(move || {
        located_unit_lines(&render_unit, viewport_width, &wrap_control)
    })
    .await
    .context("幻灯片结构化排版线程异常退出")??;
    let locator = locator_for_lines(&request.document.id, unit, &source_lines);
    ensure!(
        visual_image_references(&unit.document.blocks).len() <= MAX_SLIDE_IMAGES,
        "单张幻灯片图片超过 {MAX_SLIDE_IMAGES} 张上限"
    );
    let (page_index, should_render) = emitter.claim_page();
    if !should_render {
        return Ok(());
    }
    let loaded = load_visual_images(request, unit, control).await?;
    let mut image_digest = blake3::Hasher::new();
    for image in &loaded {
        image_digest.update(&image.prepared.bytes);
    }
    let image_digest = image_digest.finalize();
    let page_lines = source_lines
        .iter()
        .map(|line| line.text.clone())
        .collect::<Vec<_>>();
    let images = loaded
        .into_iter()
        .map(|image| image.prepared)
        .collect::<Vec<_>>();
    let profile = request.profile.clone();
    let raster_control = control.clone();
    let raster = tokio::task::spawn_blocking(move || {
        render_slide_png(&page_lines, images, &profile, &raster_control)
    })
    .await
    .context("幻灯片结构化 PNG 渲染线程异常退出")??;
    add_visual_page_bytes(total_bytes, raster.bytes.len())?;
    let metadata = render_metadata(request, unit, descriptor, profile_id);
    let id_seed = format!(
        "{}\0{}\0{}\0slide\0{}\0{}\0{}",
        request.document.id,
        request.document.revision.get(),
        unit.id,
        metadata.renderer_version,
        metadata.profile_id,
        image_digest.to_hex()
    );
    emitter
        .emit(RenderedVisualPage {
            id: deterministic_id("visual-page", id_seed),
            page_index,
            content_unit_id: Some(unit.id.clone()),
            width: raster.width,
            height: raster.height,
            media_type: "image/png".to_string(),
            bytes: raster.bytes,
            locator,
            metadata,
        })
        .await?;
    tokio::task::yield_now().await;
    Ok(())
}
