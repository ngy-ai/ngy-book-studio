//! Rendering the slides and the enhanced PDF pages.

use super::*;

pub(crate) async fn render_slides(
    request: RenderUnitsRequest,
    slides: Vec<crate::office_preview::EnhancedSlideImage>,
    descriptor: RendererDescriptor,
    control: RenderControl,
    emitter: RenderPageEmitter,
) -> Result<usize> {
    ensure!(
        !slides.is_empty() && slides.len() <= MAX_ENHANCED_PAGES,
        "Office 增强幻灯片数量无效"
    );
    let (units, requested) = office_units(&request)?;
    let profile_id = request.profile.stable_id();
    let mut total_bytes = 0_u64;
    for unit in units {
        if !requested.is_empty() && !requested.contains(&unit.id) {
            continue;
        }
        control.checkpoint().map_err(anyhow::Error::new)?;
        let Some(SourceLocator::Slide { index }) = unit.source_locator.clone() else {
            anyhow::bail!("PowerPoint 内容单元 {} 缺少幻灯片定位", unit.id);
        };
        let slide_index = usize::try_from(index.saturating_sub(1))
            .context("PowerPoint 幻灯片序号超出支持范围")?;
        let slide = slides
            .get(slide_index)
            .with_context(|| format!("Office 没有导出第 {index} 张幻灯片"))?;
        let (page_index, should_render) = emitter.claim_page();
        if !should_render {
            continue;
        }
        ensure!(
            slide.bytes.len() <= MAX_ENHANCED_SLIDE_BYTES,
            "Office 导出的第 {index} 张幻灯片超过视觉模型页面上限"
        );
        total_bytes = total_bytes
            .checked_add(slide.bytes.len() as u64)
            .context("Office 幻灯片输出总大小溢出")?;
        ensure!(
            total_bytes <= MAX_ENHANCED_BATCH_BYTES,
            "Office 幻灯片输出超过批次总大小上限"
        );
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&slide.bytes))
            .with_guessed_format()
            .context("无法识别 Office 幻灯片图片格式")?
            .into_dimensions()
            .with_context(|| format!("无法解析 Office 导出的第 {index} 张幻灯片"))?;
        ensure!(
            width > 0
                && height > 0
                && width <= MAX_ENHANCED_SLIDE_DIMENSION
                && height <= MAX_ENHANCED_SLIDE_DIMENSION
                && u64::from(width) * u64::from(height) <= MAX_ENHANCED_SLIDE_PIXELS,
            "Office 导出的幻灯片尺寸无效或超过安全上限"
        );
        let id_seed = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            request.document.id,
            request.source_id,
            request.document.revision.get(),
            unit.id,
            descriptor.version,
            profile_id,
        );
        emitter
            .emit(RenderedVisualPage {
                id: deterministic_id("visual-page", id_seed),
                page_index,
                content_unit_id: Some(unit.id.clone()),
                width,
                height,
                media_type: slide.media_type.clone(),
                bytes: slide.bytes.clone(),
                locator: DocumentLocator::unit(&request.document.id, &unit.id)
                    .with_source(SourceLocator::slide(index)),
                metadata: RenderMetadata {
                    renderer: descriptor.renderer.clone(),
                    renderer_version: descriptor.version.clone(),
                    fidelity: descriptor.fidelity,
                    document_revision: request.document.revision,
                    unit_revision: unit.revision,
                    profile_id: profile_id.clone(),
                },
            })
            .await?;
    }
    let total_pages = emitter.total_pages();
    ensure!(total_pages > 0, "Office 增强视觉任务没有选中的幻灯片");
    ensure!(
        emitter.resume_from() <= total_pages,
        "Office 增强视觉任务断点超过当前幻灯片总数"
    );
    Ok(total_pages)
}

pub(crate) async fn render_office_pdf(
    request: RenderUnitsRequest,
    pdf: Vec<u8>,
    descriptor: RendererDescriptor,
    control: RenderControl,
    emitter: RenderPageEmitter,
) -> Result<usize> {
    let (pdf, page_count) = tokio::task::spawn_blocking(move || {
        let page_count = lopdf::Document::load_mem(&pdf)
            .context("无法读取 Office 增强 PDF")?
            .get_pages()
            .len();
        Ok::<_, anyhow::Error>((pdf, page_count))
    })
    .await
    .context("Office 增强 PDF 分页线程异常退出")??;
    ensure!(
        page_count > 0 && page_count <= MAX_ENHANCED_PDF_PAGES,
        "Office 增强 PDF 页数无效"
    );
    let (units, requested) = office_units(&request)?;
    ensure!(
        requested.is_empty() || requested.len() == units.len(),
        "Word/Excel 整本 PDF 增强预览不支持按内容单元筛选"
    );
    let preview_unit_id = office_preview_unit_id(&request.document.id, &request.source_id);
    let mut targets = Vec::new();
    for enhanced_page_index in 0..page_count {
        control.checkpoint().map_err(anyhow::Error::new)?;
        let enhanced_page = u32::try_from(enhanced_page_index + 1).unwrap_or(u32::MAX);
        let source_locator = office_pdf_page_locator(enhanced_page);
        targets.push(PdfRasterTarget {
            content_unit_id: None,
            unit_revision: None,
            pdf_page: enhanced_page,
            locator: DocumentLocator::unit(&request.document.id, &preview_unit_id)
                .with_source(source_locator),
        });
    }
    ensure!(
        !targets.is_empty(),
        "Office 增强视觉任务没有选中的 PDF 页面"
    );
    rasterize_pdf_bytes(
        request.document.id.clone(),
        request.document.revision,
        targets,
        request.profile,
        descriptor,
        pdf,
        control,
        emitter,
    )
    .await
}

pub(crate) fn office_pdf_page_locator(enhanced_page: u32) -> SourceLocator {
    // Word section and Excel sheet pagination is not exposed by the
    // whole-document Office PDF export. Even equal page/unit counts are not
    // evidence of a page-to-unit mapping, so retain only the exact coordinate
    // in the enhanced artifact.
    SourceLocator::office_rendered_page(enhanced_page)
}
