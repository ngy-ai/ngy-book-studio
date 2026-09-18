//! The enhanced Office renderer and its trait implementation.

use super::*;

impl VisualRenderer for OfficeEnhancedRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: OFFICE_ENHANCED_RENDERER_NAME.to_string(),
            version: concat!(env!("CARGO_PKG_VERSION"), "+office-com-v1").to_string(),
            fidelity: RenderFidelity::OfficeEnhanced,
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
                .context("无法增强渲染无效的 Office 文档")?;
            ensure!(
                !request.source_id.trim().is_empty(),
                "Office 增强视觉任务缺少来源 ID"
            );
            let format = imported_office_format(&request.document)?;
            if matches!(
                format,
                BookFormat::Doc | BookFormat::Docx | BookFormat::Xlsx
            ) {
                let (units, requested) = office_units(&request)?;
                ensure!(
                    requested.is_empty() || requested.len() == units.len(),
                    "Word/Excel 整本 PDF 增强预览不支持按内容单元筛选"
                );
            }
            let source = request
                .asset_source
                .as_ref()
                .context("Office 增强视觉任务缺少受图书范围约束的来源")?
                .load_source(
                    request.document.id.clone(),
                    request.source_id.clone(),
                    request.document.revision,
                )
                .await
                .context("无法加载 Office 原始来源")?;
            validate_office_source(&source, format)?;
            control.checkpoint().map_err(anyhow::Error::new)?;

            let cancellation = OfficeCancellation::default();
            let output = enhance_with_control(
                &self.preview,
                format,
                Arc::new(source.bytes),
                cancellation,
                &control,
            )
            .await?;
            control.checkpoint().map_err(anyhow::Error::new)?;

            let descriptor = self.descriptor();
            match (format, output) {
                (BookFormat::Pptx, EnhancedOfficePreview::Slides(slides)) => {
                    render_slides(request, slides, descriptor, control, emitter).await
                }
                (
                    BookFormat::Doc | BookFormat::Docx | BookFormat::Xlsx,
                    EnhancedOfficePreview::Pdf(pdf),
                ) => render_office_pdf(request, pdf, descriptor, control, emitter).await,
                _ => anyhow::bail!("Office 增强器返回了与源格式不匹配的产物"),
            }
        })
    }
}
