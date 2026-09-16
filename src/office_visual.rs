//! Persistent visual renderer backed by the optional Microsoft Office COM
//! enhancement. The renderer consumes only the byte-exact, book-scoped
//! original supplied by [`VisualDocumentSource`]; temporary Office artifacts
//! never become document-domain state.

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, ensure};

use crate::{
    document::{
        BookDocument, BookFormat, BookSource, DocumentLocator, Revision, SourceLocator,
        deterministic_id,
    },
    office_com::{OfficeCancellation, OfficeEnhancer},
    office_preview::{EnhancedOfficePreview, OfficePreviewService},
    preview::{
        PreviewFuture, RenderControl, RenderFidelity, RenderMetadata, RenderPageEmitter,
        RenderUnitsRequest, RenderedVisualPage, RendererDescriptor, VisualRenderer,
        VisualSourcePayload, office_preview_unit_id,
    },
    windows_pdf_renderer::{PdfRasterTarget, rasterize_pdf_bytes},
};

pub(crate) const OFFICE_ENHANCED_RENDERER_NAME: &str = "ngy-office-com-enhanced";
const MAX_ENHANCED_PAGES: usize = 20_000;
const MAX_ENHANCED_PDF_PAGES: usize = 4_096;
const MAX_ENHANCED_SLIDE_BYTES: usize = 12 * 1024 * 1024;
const MAX_ENHANCED_BATCH_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENHANCED_SLIDE_DIMENSION: u32 = 8_192;
const MAX_ENHANCED_SLIDE_PIXELS: u64 = 32 * 1024 * 1024;
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct OfficeEnhancedRenderer {
    preview: OfficePreviewService,
}

impl std::fmt::Debug for OfficeEnhancedRenderer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OfficeEnhancedRenderer")
            .field("descriptor", &self.descriptor())
            .finish_non_exhaustive()
    }
}

impl OfficeEnhancedRenderer {
    pub fn new(enhancer: Arc<dyn OfficeEnhancer>) -> Self {
        Self {
            preview: OfficePreviewService::new(enhancer),
        }
    }
}

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

async fn enhance_with_control(
    service: &OfficePreviewService,
    format: BookFormat,
    original: Arc<Vec<u8>>,
    cancellation: OfficeCancellation,
    control: &RenderControl,
) -> Result<EnhancedOfficePreview> {
    let future = service.enhance(format, original, cancellation.clone());
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return output,
            _ = tokio::time::sleep(CANCELLATION_POLL_INTERVAL) => {
                if let Err(interrupted) = control.checkpoint() {
                    cancellation.cancel();
                    // Let the STA worker observe cancellation before its
                    // temporary workspace is dropped.
                    let _ = future.await;
                    return Err(anyhow::Error::new(interrupted));
                }
            }
        }
    }
}

fn imported_office_format(document: &BookDocument) -> Result<BookFormat> {
    match document.source {
        BookSource::Imported { format, .. }
            if matches!(
                format,
                BookFormat::Doc | BookFormat::Docx | BookFormat::Pptx | BookFormat::Xlsx
            ) =>
        {
            Ok(format)
        }
        _ => anyhow::bail!("Office 增强 renderer 只能处理导入的 Office 文档"),
    }
}

fn validate_office_source(source: &VisualSourcePayload, format: BookFormat) -> Result<()> {
    ensure!(
        source.source_kind == "original",
        "规范化编辑后的文档不能继续使用原始 Office 增强页面"
    );
    ensure!(
        source.format == format_name(format),
        "Office 增强来源格式与当前文档不一致"
    );
    ensure!(!source.bytes.is_empty(), "Office 增强来源内容为空");
    Ok(())
}

fn format_name(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Doc => "doc",
        BookFormat::Docx => "docx",
        BookFormat::Pptx => "pptx",
        BookFormat::Xlsx => "xlsx",
        _ => "unsupported",
    }
}

#[derive(Clone, Debug)]
struct OfficeUnit {
    id: String,
    revision: Revision,
    source_locator: Option<SourceLocator>,
}

fn office_units(request: &RenderUnitsRequest) -> Result<(Vec<OfficeUnit>, HashSet<String>)> {
    let requested = request.unit_ids.iter().cloned().collect::<HashSet<_>>();
    ensure!(
        requested.len() == request.unit_ids.len(),
        "Office 增强视觉任务不能重复选择内容单元"
    );
    let all = request
        .document
        .units
        .iter()
        .map(|unit| OfficeUnit {
            id: unit.id.clone(),
            revision: unit.revision,
            source_locator: unit.source_locator.clone(),
        })
        .collect::<Vec<_>>();
    ensure!(!all.is_empty(), "Office 增强视觉任务没有内容单元");
    if !requested.is_empty() {
        ensure!(
            requested
                .iter()
                .all(|unit_id| all.iter().any(|unit| unit.id == *unit_id)),
            "Office 增强视觉任务包含不存在的内容单元"
        );
    }
    Ok((all, requested))
}

async fn render_slides(
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

async fn render_office_pdf(
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

fn office_pdf_page_locator(enhanced_page: u32) -> SourceLocator {
    // Word section and Excel sheet pagination is not exposed by the
    // whole-document Office PDF export. Even equal page/unit counts are not
    // evidence of a page-to-unit mapping, so retain only the exact coordinate
    // in the enhanced artifact.
    SourceLocator::office_rendered_page(enhanced_page)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        document::{AssetRef, AssetRole, BlockDocument, ContentUnit, ContentUnitKind},
        office_com::{
            OfficeEnhanceOutput, OfficeEnhanceRequest, OfficeEnhancementKind, OfficeFuture,
        },
        preview::{RenderProfile, VisualAssetPayload, VisualDocumentSource},
    };
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };

    #[derive(Clone, Copy)]
    struct FakeSlides;

    impl OfficeEnhancer for FakeSlides {
        fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
            Box::pin(async move {
                assert_eq!(request.kind, OfficeEnhancementKind::PowerPointImages);
                let paths = ["Slide10.png", "Slide2.png", "Slide1.png"]
                    .into_iter()
                    .map(|name| {
                        let path = request.target.join(name);
                        std::fs::write(&path, tiny_png())?;
                        Ok(path)
                    })
                    .collect::<Result<Vec<PathBuf>>>()?;
                Ok(OfficeEnhanceOutput::Images(paths))
            })
        }
    }

    #[derive(Clone)]
    struct PptxSource(Vec<u8>);

    impl VisualDocumentSource for PptxSource {
        fn load_document(
            &self,
            _book_id: String,
            _revision: Revision,
        ) -> PreviewFuture<'_, BookDocument> {
            Box::pin(async { anyhow::bail!("not used") })
        }

        fn load_asset(
            &self,
            _book_id: String,
            _asset_id: String,
        ) -> PreviewFuture<'_, VisualAssetPayload> {
            Box::pin(async { anyhow::bail!("not used") })
        }

        fn load_source(
            &self,
            _book_id: String,
            _source_id: String,
            _revision: Revision,
        ) -> PreviewFuture<'_, VisualSourcePayload> {
            let bytes = self.0.clone();
            Box::pin(async move {
                Ok(VisualSourcePayload {
                    format: "pptx".to_string(),
                    source_kind: "original".to_string(),
                    media_type:
                        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                            .to_string(),
                    bytes,
                })
            })
        }
    }

    #[derive(Clone, Copy)]
    struct FakeWordPdf;

    impl OfficeEnhancer for FakeWordPdf {
        fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
            Box::pin(async move {
                assert_eq!(request.kind, OfficeEnhancementKind::WordPdf);
                std::fs::write(&request.target, test_pdf())?;
                Ok(OfficeEnhanceOutput::Pdf(request.target))
            })
        }
    }

    #[derive(Clone)]
    struct DocxSource(Vec<u8>);

    impl VisualDocumentSource for DocxSource {
        fn load_document(
            &self,
            _book_id: String,
            _revision: Revision,
        ) -> PreviewFuture<'_, BookDocument> {
            Box::pin(async { anyhow::bail!("not used") })
        }

        fn load_source(
            &self,
            _book_id: String,
            _source_id: String,
            _revision: Revision,
        ) -> PreviewFuture<'_, VisualSourcePayload> {
            let bytes = self.0.clone();
            Box::pin(async move {
                Ok(VisualSourcePayload {
                    format: "docx".to_string(),
                    source_kind: "original".to_string(),
                    media_type:
                        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                            .to_string(),
                    bytes,
                })
            })
        }
    }

    #[test]
    fn powerpoint_pages_are_naturally_ordered_and_keep_native_locators() {
        let original_asset = AssetRef::new(
            "asset-original",
            vec![AssetRole::OriginalSource],
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            Some("slides.pptx".to_string()),
            4,
            blake3::hash(b"pptx").to_hex().to_string(),
        );
        let mut document = BookDocument::new(
            "book-office",
            "Slides",
            BookSource::imported(
                BookFormat::Pptx,
                original_asset.id.clone(),
                Some("slides.pptx".into()),
            ),
        );
        document.assets.push(original_asset);
        for index in 1..=3 {
            document.units.push(
                ContentUnit::new(
                    format!("unit-{index}"),
                    ContentUnitKind::Slide,
                    format!("Slide {index}"),
                    String::new(),
                    BlockDocument::default(),
                )
                .with_source_locator(SourceLocator::slide(index)),
            );
        }
        document.validate().unwrap();
        let renderer = OfficeEnhancedRenderer::new(Arc::new(FakeSlides));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let pages = runtime
            .block_on(renderer.render_units(
                RenderUnitsRequest {
                    document: Arc::new(document),
                    source_id: "source-office".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile::default(),
                    asset_source: Some(Arc::new(PptxSource(b"pptx".to_vec()))),
                },
                RenderControl::default(),
            ))
            .unwrap();

        assert_eq!(pages.len(), 3);
        assert!(pages.iter().enumerate().all(|(index, page)| {
            page.page_index == index
                && page.content_unit_id.as_deref() == Some(format!("unit-{}", index + 1).as_str())
                && page.metadata.fidelity == RenderFidelity::OfficeEnhanced
                && page.metadata.renderer == OFFICE_ENHANCED_RENDERER_NAME
                && page.locator.source == Some(SourceLocator::slide((index + 1) as u32))
        }));
    }

    #[test]
    fn word_pdf_is_rasterized_to_png_with_office_metadata() {
        let original_asset = AssetRef::new(
            "asset-original-docx",
            vec![AssetRole::OriginalSource],
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Some("document.docx".to_string()),
            4,
            blake3::hash(b"docx").to_hex().to_string(),
        );
        let mut document = BookDocument::new(
            "book-word",
            "Word",
            BookSource::imported(
                BookFormat::Docx,
                original_asset.id.clone(),
                Some("document.docx".into()),
            ),
        );
        document.assets.push(original_asset);
        document.units.push(
            ContentUnit::new(
                "unit-word-1",
                ContentUnitKind::Chapter,
                "Section 1",
                String::new(),
                BlockDocument::default(),
            )
            .with_source_locator(SourceLocator::office_section(1)),
        );
        document.units.push(
            ContentUnit::new(
                "unit-word-2",
                ContentUnitKind::Chapter,
                "Section 2",
                String::new(),
                BlockDocument::default(),
            )
            .with_source_locator(SourceLocator::office_section(2)),
        );
        document.validate().unwrap();
        let renderer = OfficeEnhancedRenderer::new(Arc::new(FakeWordPdf));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let pages = runtime
            .block_on(renderer.render_units(
                RenderUnitsRequest {
                    document: Arc::new(document),
                    source_id: "source-word".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile::default(),
                    asset_source: Some(Arc::new(DocxSource(b"docx".to_vec()))),
                },
                RenderControl::default(),
            ))
            .unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].content_unit_id, None);
        assert_eq!(pages[0].metadata.unit_revision, Revision::INITIAL);
        assert_eq!(pages[0].media_type, "image/png");
        assert!(pages[0].bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(pages[0].metadata.fidelity, RenderFidelity::OfficeEnhanced);
        assert_eq!(
            pages[0].locator.source,
            Some(SourceLocator::office_rendered_page(1))
        );
    }

    #[test]
    fn repaginated_office_pdf_uses_a_distinct_rendered_page_locator() {
        assert_eq!(
            office_pdf_page_locator(2),
            SourceLocator::office_rendered_page(2)
        );
        assert_eq!(
            office_pdf_page_locator(1),
            SourceLocator::office_rendered_page(1)
        );
    }

    fn tiny_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn test_pdf() -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 18.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal("Office PDF")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }
}
