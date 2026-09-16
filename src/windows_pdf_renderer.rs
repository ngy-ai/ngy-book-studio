//! Native Windows PDF rasterization for visual understanding.
//!
//! The renderer consumes only book-scoped immutable source bytes. Windows
//! Runtime objects and PDF raster work stay on Tokio's blocking pool, never on
//! the GPUI thread or an async runtime worker.

use std::collections::HashSet;

use anyhow::{Context as _, Result, ensure};
use windows::{
    Data::Pdf::{PdfDocument, PdfPageRenderOptions},
    Storage::Streams::{
        Buffer, DataReader, DataWriter, InMemoryRandomAccessStream, InputStreamOptions,
    },
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
};

use crate::{
    document::{
        BookFormat, BookSource, DocumentLocator, Revision, SourceLocator, deterministic_id,
    },
    formats::{MAX_PDF_PAGES, MAX_PDF_SOURCE_BYTES},
    preview::{
        PreviewFuture, RenderControl, RenderFidelity, RenderMetadata, RenderPageEmitter,
        RenderProfile, RenderUnitsRequest, RenderedVisualPage, RendererDescriptor, VisualRenderer,
        VisualSourcePayload,
    },
};

const MAX_RASTER_DIMENSION: u32 = 4_096;
const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PAGE_PNG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_PNG_BYTES: u64 = 512 * 1024 * 1024;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsPdfRenderer;

impl VisualRenderer for WindowsPdfRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: "ngy-windows-pdf-png".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
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
                .context("无法渲染无效的 PDF 文档")?;
            ensure!(
                !request.source_id.trim().is_empty(),
                "PDF 视觉任务缺少来源 ID"
            );
            ensure!(
                matches!(
                    request.document.source,
                    BookSource::Imported {
                        format: BookFormat::Pdf,
                        ..
                    }
                ),
                "Windows PDF renderer 只能渲染导入的 PDF"
            );
            let units = selected_pdf_units(&request)?;
            ensure!(
                units.len() <= MAX_PDF_PAGES as usize,
                "PDF 视觉任务超过 {MAX_PDF_PAGES} 页上限"
            );
            control.checkpoint().map_err(anyhow::Error::new)?;
            let source = request
                .asset_source
                .as_ref()
                .context("PDF 视觉任务缺少受图书范围约束的来源")?
                .load_source(
                    request.document.id.clone(),
                    request.source_id.clone(),
                    request.document.revision,
                )
                .await
                .context("无法加载 PDF 原始来源")?;
            validate_pdf_source(&source)?;
            control.checkpoint().map_err(anyhow::Error::new)?;

            rasterize_pdf_bytes(
                request.document.id.clone(),
                request.document.revision,
                units,
                request.profile,
                self.descriptor(),
                source.bytes,
                control,
                emitter,
            )
            .await
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PdfRasterTarget {
    pub(crate) content_unit_id: Option<String>,
    pub(crate) unit_revision: Option<Revision>,
    pub(crate) pdf_page: u32,
    pub(crate) locator: DocumentLocator,
}

fn selected_pdf_units(request: &RenderUnitsRequest) -> Result<Vec<PdfRasterTarget>> {
    let requested = request.unit_ids.iter().cloned().collect::<HashSet<_>>();
    ensure!(
        requested.len() == request.unit_ids.len(),
        "PDF 视觉任务不能重复选择内容单元"
    );
    let mut selected = Vec::new();
    for unit in &request.document.units {
        if !requested.is_empty() && !requested.contains(&unit.id) {
            continue;
        }
        let Some(SourceLocator::PdfPage { page }) = unit.source_locator.as_ref() else {
            anyhow::bail!("内容单元 {} 缺少 PDF 页定位", unit.id);
        };
        ensure!(*page > 0, "内容单元 {} 的 PDF 页码无效", unit.id);
        selected.push(PdfRasterTarget {
            content_unit_id: Some(unit.id.clone()),
            unit_revision: Some(unit.revision),
            pdf_page: *page,
            locator: DocumentLocator::unit(&request.document.id, &unit.id)
                .with_source(SourceLocator::pdf_page(*page)),
        });
    }
    ensure!(
        requested.is_empty() || selected.len() == requested.len(),
        "PDF 视觉任务包含不存在的内容单元"
    );
    ensure!(!selected.is_empty(), "PDF 视觉任务没有可渲染页面");
    Ok(selected)
}

fn validate_pdf_source(source: &VisualSourcePayload) -> Result<()> {
    ensure!(source.format == "pdf", "视觉任务来源格式不是 PDF");
    ensure!(
        source.source_kind == "original",
        "规范化编辑后的文档不能继续使用原始 PDF 视觉页面"
    );
    ensure!(
        source.media_type.eq_ignore_ascii_case("application/pdf"),
        "视觉任务来源 MIME 不是 application/pdf"
    );
    validate_pdf_bytes(&source.bytes)
}

fn validate_pdf_bytes(bytes: &[u8]) -> Result<()> {
    ensure!(!bytes.is_empty(), "PDF 来源内容为空");
    ensure!(
        bytes.len() <= MAX_PDF_SOURCE_BYTES,
        "PDF 来源超过 {MAX_PDF_SOURCE_BYTES} 字节上限"
    );
    ensure!(bytes.starts_with(b"%PDF-"), "PDF 来源缺少文件签名");
    Ok(())
}

/// Rasterizes already-authorized PDF bytes for a caller-provided set of
/// durable document targets. Office enhancement uses this lower-level path
/// after COM exported a temporary PDF; no temporary path or PDF-specific
/// proxy document escapes into the domain model.
pub(crate) async fn rasterize_pdf_bytes(
    document_id: String,
    document_revision: Revision,
    targets: Vec<PdfRasterTarget>,
    profile: RenderProfile,
    descriptor: RendererDescriptor,
    source_bytes: Vec<u8>,
    control: RenderControl,
    emitter: RenderPageEmitter,
) -> Result<usize> {
    profile.validate()?;
    descriptor.validate()?;
    ensure!(!document_id.trim().is_empty(), "PDF 渲染缺少图书 ID");
    ensure!(!targets.is_empty(), "PDF 视觉任务没有可渲染页面");
    ensure!(
        targets.len() <= MAX_PDF_PAGES as usize,
        "PDF 视觉任务超过 {MAX_PDF_PAGES} 页上限"
    );
    for target in &targets {
        ensure!(target.pdf_page > 0, "PDF 页面目标无效");
        ensure!(target.locator.book_id == document_id, "PDF 页面目标越权");
        match target.content_unit_id.as_deref() {
            Some(unit_id) => ensure!(
                !unit_id.trim().is_empty()
                    && target.unit_revision.is_some()
                    && target.locator.unit_id == unit_id,
                "PDF 页面目标 locator 不匹配"
            ),
            None => ensure!(
                target.unit_revision.is_none()
                    && matches!(
                        target.locator.source.as_ref(),
                        Some(SourceLocator::OfficeRenderedPage { .. })
                    ),
                "无内容单元的 PDF 页面目标必须是 Office preview-only 页面"
            ),
        }
        target
            .locator
            .validate()
            .context("PDF 页面目标 locator 无效")?;
    }
    validate_pdf_bytes(&source_bytes)?;
    control.checkpoint().map_err(anyhow::Error::new)?;
    tokio::task::spawn_blocking(move || {
        render_pdf_blocking(
            document_id,
            document_revision,
            targets,
            profile,
            descriptor,
            source_bytes,
            control,
            emitter,
        )
    })
    .await
    .context("Windows PDF 渲染线程异常退出")?
}

fn render_pdf_blocking(
    document_id: String,
    document_revision: Revision,
    targets: Vec<PdfRasterTarget>,
    profile: RenderProfile,
    descriptor: RendererDescriptor,
    source_bytes: Vec<u8>,
    control: RenderControl,
    emitter: RenderPageEmitter,
) -> Result<usize> {
    control.checkpoint().map_err(anyhow::Error::new)?;
    let _apartment = WinRtApartment::initialize_mta()?;
    let input = memory_stream_from_bytes(&source_bytes).context("无法创建 PDF 内存流")?;
    control.checkpoint().map_err(anyhow::Error::new)?;
    let pdf = PdfDocument::LoadFromStreamAsync(&input)
        .context("Windows 无法开始解析 PDF")?
        .get()
        .context("Windows 无法解析 PDF")?;
    let page_count = pdf.PageCount().context("Windows 无法读取 PDF 页数")?;
    ensure!(page_count > 0, "PDF 不包含页面");
    ensure!(
        page_count <= MAX_PDF_PAGES,
        "PDF 超过 {MAX_PDF_PAGES} 页上限"
    );

    let profile_id = profile.stable_id();
    let mut total_bytes = 0_u64;
    for target in targets {
        control.checkpoint().map_err(anyhow::Error::new)?;
        ensure!(
            target.pdf_page <= page_count,
            "PDF 页面目标指向不存在的第 {} 页",
            target.pdf_page
        );
        let (page_index, should_render) = emitter.claim_page();
        if !should_render {
            continue;
        }
        let page = pdf
            .GetPage(target.pdf_page - 1)
            .with_context(|| format!("Windows 无法打开 PDF 第 {} 页", target.pdf_page))?;
        let page_size = page
            .Size()
            .with_context(|| format!("Windows 无法读取 PDF 第 {} 页尺寸", target.pdf_page))?;
        let (width, height) = raster_dimensions(page_size.Width, page_size.Height, &profile)?;
        let output = InMemoryRandomAccessStream::new().context("无法创建 PDF 页面输出流")?;
        let options = PdfPageRenderOptions::new().context("无法创建 PDF 页面渲染配置")?;
        options
            .SetDestinationWidth(width)
            .context("无法设置 PDF 页面渲染宽度")?;
        options
            .SetDestinationHeight(height)
            .context("无法设置 PDF 页面渲染高度")?;
        control.checkpoint().map_err(anyhow::Error::new)?;
        page.RenderWithOptionsToStreamAsync(&output, &options)
            .with_context(|| format!("Windows 无法开始渲染 PDF 第 {} 页", target.pdf_page))?
            .get()
            .with_context(|| format!("Windows 无法渲染 PDF 第 {} 页", target.pdf_page))?;
        control.checkpoint().map_err(anyhow::Error::new)?;
        let bytes = bytes_from_memory_stream(&output, MAX_PAGE_PNG_BYTES)
            .with_context(|| format!("无法读取 PDF 第 {} 页渲染结果", target.pdf_page))?;
        ensure!(
            bytes.starts_with(PNG_SIGNATURE),
            "Windows PDF 第 {} 页没有输出 PNG",
            target.pdf_page
        );
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .context("PDF 页面输出总大小溢出")?;
        ensure!(
            total_bytes <= MAX_TOTAL_PNG_BYTES,
            "PDF 页面输出超过 {MAX_TOTAL_PNG_BYTES} 字节总上限"
        );
        let id_seed = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            document_id,
            document_revision.get(),
            target.content_unit_id.as_deref().unwrap_or("preview-only"),
            target.pdf_page,
            descriptor.version,
            profile_id
        );
        emitter.emit_blocking(RenderedVisualPage {
            id: deterministic_id("visual-page", id_seed),
            page_index,
            content_unit_id: target.content_unit_id,
            width,
            height,
            media_type: "image/png".to_string(),
            bytes,
            locator: target.locator,
            metadata: RenderMetadata {
                renderer: descriptor.renderer.clone(),
                renderer_version: descriptor.version.clone(),
                fidelity: descriptor.fidelity,
                document_revision,
                unit_revision: target.unit_revision.unwrap_or(Revision::INITIAL),
                profile_id: profile_id.clone(),
            },
        })?;
        let _ = page.Close();
        let _ = output.Close();
    }
    let _ = input.Close();
    let total_pages = emitter.total_pages();
    ensure!(
        emitter.resume_from() <= total_pages,
        "PDF 视觉任务断点超过当前页面总数"
    );
    Ok(total_pages)
}

fn raster_dimensions(
    page_width: f32,
    page_height: f32,
    profile: &RenderProfile,
) -> Result<(u32, u32)> {
    ensure!(
        page_width.is_finite() && page_width > 0.0 && page_height.is_finite() && page_height > 0.0,
        "PDF 页面尺寸无效"
    );
    let max_width = ((f64::from(profile.viewport_width) * profile.scale()).round() as u32)
        .clamp(1, MAX_RASTER_DIMENSION);
    let max_height = ((f64::from(profile.viewport_height) * profile.scale()).round() as u32)
        .clamp(1, MAX_RASTER_DIMENSION);
    let scale = (f64::from(max_width) / f64::from(page_width))
        .min(f64::from(max_height) / f64::from(page_height));
    let mut width = (f64::from(page_width) * scale).round().max(1.0) as u32;
    let mut height = (f64::from(page_height) * scale).round().max(1.0) as u32;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_RASTER_PIXELS {
        let pixel_scale = (MAX_RASTER_PIXELS as f64 / pixels as f64).sqrt();
        width = (f64::from(width) * pixel_scale).floor().max(1.0) as u32;
        height = (f64::from(height) * pixel_scale).floor().max(1.0) as u32;
    }
    ensure!(
        width <= MAX_RASTER_DIMENSION
            && height <= MAX_RASTER_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS,
        "PDF 页面目标尺寸超过安全上限"
    );
    Ok((width, height))
}

fn memory_stream_from_bytes(bytes: &[u8]) -> Result<InMemoryRandomAccessStream> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    let stored = writer.StoreAsync()?.get()?;
    ensure!(stored as usize == bytes.len(), "PDF 内存流写入不完整");
    writer.FlushAsync()?.get()?;
    writer.DetachStream()?;
    writer.Close()?;
    stream.Seek(0)?;
    Ok(stream)
}

fn bytes_from_memory_stream(
    stream: &InMemoryRandomAccessStream,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let size = stream.Size()?;
    ensure!(size > 0, "PDF 页面输出为空");
    ensure!(size <= max_bytes, "PDF 页面输出超过 {max_bytes} 字节上限");
    let size = u32::try_from(size).context("PDF 页面输出无法放入 WinRT buffer")?;
    stream.Seek(0)?;
    let buffer = Buffer::Create(size)?;
    let read = stream
        .ReadAsync(&buffer, size, InputStreamOptions::None)?
        .get()?;
    ensure!(read.Length()? == size, "PDF 页面输出读取不完整");
    let reader = DataReader::FromBuffer(&read)?;
    let mut bytes = vec![0_u8; size as usize];
    reader.ReadBytes(&mut bytes)?;
    reader.Close()?;
    Ok(bytes)
}

struct WinRtApartment;

impl WinRtApartment {
    fn initialize_mta() -> Result<Self> {
        // SAFETY: This is paired with exactly one `RoUninitialize` on the same
        // blocking worker thread through `Drop` below.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.context("无法初始化 Windows Runtime MTA")?;
        Ok(Self)
    }
}

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        // SAFETY: `initialize_mta` succeeded on this thread and the guard never
        // crosses a thread boundary.
        unsafe { RoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::ensure;
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };

    use super::*;
    use crate::{
        document::{
            AssetRef, AssetRole, Block, BlockDocument, BookDocument, ContentUnit, ContentUnitKind,
        },
        preview::{PreviewFuture, VisualDocumentSource},
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn renders_real_pdf_pages_as_png_and_honors_unit_selection() {
        let bytes = make_pdf(2);
        let document = test_document(&bytes, 2);
        let source: Arc<dyn VisualDocumentSource> = Arc::new(TestSource {
            document: document.clone(),
            payload: VisualSourcePayload {
                format: "pdf".to_string(),
                source_kind: "original".to_string(),
                media_type: "application/pdf".to_string(),
                bytes,
            },
        });
        let renderer = WindowsPdfRenderer;
        let pages = renderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document.clone()),
                    source_id: "source-pdf".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile::default(),
                    asset_source: Some(Arc::clone(&source)),
                },
                RenderControl::default(),
            )
            .await
            .expect("render PDF pages");
        assert_eq!(pages.len(), 2);
        for (index, page) in pages.iter().enumerate() {
            assert!(page.bytes.starts_with(PNG_SIGNATURE));
            assert_eq!(page.media_type, "image/png");
            assert_eq!(page.page_index, index);
            assert_eq!(
                page.locator,
                DocumentLocator::unit("book-pdf", format!("unit-{}", index + 1))
                    .with_source(SourceLocator::pdf_page((index + 1) as u32))
            );
            let decoded = image::load_from_memory_with_format(&page.bytes, image::ImageFormat::Png)
                .expect("decode native PNG");
            assert_eq!(decoded.width(), page.width);
            assert_eq!(decoded.height(), page.height);
        }

        let selected = renderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document),
                    source_id: "source-pdf".to_string(),
                    unit_ids: vec!["unit-2".to_string()],
                    profile: RenderProfile::default(),
                    asset_source: Some(source),
                },
                RenderControl::default(),
            )
            .await
            .expect("render selected PDF page");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].content_unit_id.as_deref(), Some("unit-2"));
        assert_eq!(selected[0].page_index, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_invalid_or_normalized_pdf_sources() {
        let valid = make_pdf(1);
        let document = test_document(&valid, 1);
        for (source_kind, bytes) in [
            ("original", b"%PDF-1.7\ninvalid".to_vec()),
            ("normalized", valid),
        ] {
            let source: Arc<dyn VisualDocumentSource> = Arc::new(TestSource {
                document: document.clone(),
                payload: VisualSourcePayload {
                    format: "pdf".to_string(),
                    source_kind: source_kind.to_string(),
                    media_type: "application/pdf".to_string(),
                    bytes,
                },
            });
            let error = WindowsPdfRenderer
                .render_units(
                    RenderUnitsRequest {
                        document: Arc::new(document.clone()),
                        source_id: "source-pdf".to_string(),
                        unit_ids: Vec::new(),
                        profile: RenderProfile::default(),
                        asset_source: Some(source),
                    },
                    RenderControl::default(),
                )
                .await
                .expect_err("invalid source must fail");
            let detail = format!("{error:#}");
            if source_kind == "original" {
                assert!(detail.contains("Windows 无法解析 PDF"), "{detail}");
            } else {
                assert!(detail.contains("规范化编辑后"), "{detail}");
            }
        }
    }

    struct TestSource {
        document: BookDocument,
        payload: VisualSourcePayload,
    }

    impl VisualDocumentSource for TestSource {
        fn load_document(
            &self,
            book_id: String,
            revision: Revision,
        ) -> PreviewFuture<'_, BookDocument> {
            let document = self.document.clone();
            Box::pin(async move {
                ensure!(document.id == book_id, "wrong book");
                ensure!(document.revision == revision, "wrong revision");
                Ok(document)
            })
        }

        fn load_source(
            &self,
            book_id: String,
            source_id: String,
            revision: Revision,
        ) -> PreviewFuture<'_, VisualSourcePayload> {
            let expected_book = self.document.id.clone();
            let expected_revision = self.document.revision;
            let payload = self.payload.clone();
            Box::pin(async move {
                ensure!(book_id == expected_book, "wrong book");
                ensure!(source_id == "source-pdf", "wrong source");
                ensure!(revision == expected_revision, "wrong revision");
                Ok(payload)
            })
        }
    }

    fn test_document(bytes: &[u8], page_count: u32) -> BookDocument {
        let original = AssetRef::from_bytes(
            AssetRole::OriginalSource,
            "application/pdf",
            Some("sample.pdf".to_string()),
            bytes,
        );
        let mut document = BookDocument::new(
            "book-pdf",
            "PDF visual test",
            BookSource::imported(
                BookFormat::Pdf,
                original.id.clone(),
                Some("sample.pdf".to_string()),
            ),
        );
        document.revision = Revision::new(7);
        document.assets.push(original);
        for page in 1..=page_count {
            let mut unit = ContentUnit::new(
                format!("unit-{page}"),
                ContentUnitKind::Page,
                format!("Page {page}"),
                format!("Page {page}"),
                BlockDocument::new(vec![Block::paragraph(
                    format!("block-{page}"),
                    format!("Page {page}"),
                )]),
            )
            .with_source_locator(SourceLocator::pdf_page(page));
            unit.revision = Revision::new(7);
            document.units.push(unit);
        }
        document.validate().expect("valid PDF document");
        document
    }

    fn make_pdf(page_count: u32) -> Vec<u8> {
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
        let mut page_ids = Vec::new();
        for page in 1..=page_count {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 18.into()]),
                    Operation::new("Td", vec![72.into(), 720.into()]),
                    Operation::new("Tj", vec![Object::string_literal(format!("Page {page}"))]),
                    Operation::new("ET", vec![]),
                ],
            };
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                content.encode().expect("encode PDF content"),
            ));
            page_ids.push(document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            }));
        }
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.into_iter().map(Into::into).collect::<Vec<Object>>(),
                "Count" => page_count,
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
        document.save_to(&mut bytes).expect("write PDF fixture");
        bytes
    }
}
