//! Pure-Rust DjVu page rasterization for the visual pipeline.
//!
//! The renderer consumes only book-scoped immutable source bytes, decodes the
//! retained DjVu original and publishes one PNG per page into the durable
//! `visual_pages` store. Page and content unit are strictly 1:1, so citations,
//! reading progress and the page-image window can address an exact source page.
//!
//! Unlike the Windows PDF/Office renderers this one is portable: `djvu-rs` is
//! safe Rust with no platform API. Raster work stays on Tokio's blocking pool
//! and never runs on the GPUI thread or an async worker.

use std::collections::HashSet;

use anyhow::{Context as _, Result, ensure};
use djvu_rs::{Document, Pixmap};

use crate::{
    document::{
        BookFormat, BookSource, DocumentLocator, Revision, SourceLocator, deterministic_id,
    },
    formats::{MAX_DJVU_PAGES, MAX_DJVU_SOURCE_BYTES, djvu_form_type},
    preview::{
        PreviewFuture, RenderControl, RenderFidelity, RenderMetadata, RenderPageEmitter,
        RenderProfile, RenderUnitsRequest, RenderedVisualPage, RendererDescriptor, VisualRenderer,
        VisualSourcePayload,
    },
};

/// Stable renderer identity; `db::transactions` selects it for DjVu sources.
pub const DJVU_RENDERER_NAME: &str = "moye-djvu-png";

const MAX_RASTER_DIMENSION: u32 = 4_096;
const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PAGE_PNG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_PNG_BYTES: u64 = 512 * 1024 * 1024;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone, Copy, Debug, Default)]
pub struct DjvuPngRenderer;

impl VisualRenderer for DjvuPngRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: DJVU_RENDERER_NAME.to_string(),
            version: concat!(env!("CARGO_PKG_VERSION"), "+djvu-rs-0.32.1").to_string(),
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
                .context("无法渲染无效的 DjVu 文档")?;
            ensure!(
                !request.source_id.trim().is_empty(),
                "DjVu 视觉任务缺少来源 ID"
            );
            ensure!(
                matches!(
                    request.document.source,
                    BookSource::Imported {
                        format: BookFormat::Djvu,
                        ..
                    }
                ),
                "DjVu renderer 只能渲染导入的 DjVu"
            );
            let targets = selected_djvu_units(&request)?;
            control.checkpoint().map_err(anyhow::Error::new)?;
            let source = request
                .asset_source
                .as_ref()
                .context("DjVu 视觉任务缺少受图书范围约束的来源")?
                .load_source(
                    request.document.id.clone(),
                    request.source_id.clone(),
                    request.document.revision,
                )
                .await
                .context("无法加载 DjVu 原始来源")?;
            validate_djvu_source(&source)?;
            control.checkpoint().map_err(anyhow::Error::new)?;

            let document_id = request.document.id.clone();
            let document_revision = request.document.revision;
            let profile = request.profile;
            let descriptor = self.descriptor();
            tokio::task::spawn_blocking(move || {
                render_djvu_blocking(
                    document_id,
                    document_revision,
                    targets,
                    profile,
                    descriptor,
                    source.bytes,
                    control,
                    emitter,
                )
            })
            .await
            .context("DjVu 渲染线程异常退出")?
        })
    }
}

#[derive(Clone, Debug)]
struct DjvuRasterTarget {
    content_unit_id: String,
    unit_revision: Revision,
    page: u32,
    locator: DocumentLocator,
}

fn selected_djvu_units(request: &RenderUnitsRequest) -> Result<Vec<DjvuRasterTarget>> {
    let requested = request.unit_ids.iter().cloned().collect::<HashSet<_>>();
    ensure!(
        requested.len() == request.unit_ids.len(),
        "DjVu 视觉任务不能重复选择内容单元"
    );
    let mut selected = Vec::new();
    for unit in &request.document.units {
        if !requested.is_empty() && !requested.contains(&unit.id) {
            continue;
        }
        let Some(SourceLocator::DjvuPage { page }) = unit.source_locator.as_ref() else {
            anyhow::bail!("内容单元 {} 缺少 DjVu 页定位", unit.id);
        };
        ensure!(*page > 0, "内容单元 {} 的 DjVu 页码无效", unit.id);
        selected.push(DjvuRasterTarget {
            content_unit_id: unit.id.clone(),
            unit_revision: unit.revision,
            page: *page,
            locator: DocumentLocator::unit(&request.document.id, &unit.id)
                .with_source(SourceLocator::djvu_page(*page)),
        });
    }
    ensure!(
        requested.is_empty() || selected.len() == requested.len(),
        "DjVu 视觉任务包含不存在的内容单元"
    );
    ensure!(!selected.is_empty(), "DjVu 视觉任务没有可渲染页面");
    ensure!(
        selected.len() <= MAX_DJVU_PAGES,
        "DjVu 视觉任务超过 {MAX_DJVU_PAGES} 页上限"
    );
    let mut pages = selected
        .iter()
        .map(|target| target.page)
        .collect::<Vec<_>>();
    pages.sort_unstable();
    pages.dedup();
    ensure!(
        pages.len() == selected.len(),
        "DjVu 视觉任务包含重复的源页码"
    );
    Ok(selected)
}

fn validate_djvu_source(source: &VisualSourcePayload) -> Result<()> {
    ensure!(source.format == "djvu", "视觉任务来源格式不是 DjVu");
    ensure!(
        source.source_kind == "original",
        "规范化编辑后的文档不能继续使用原始 DjVu 视觉页面"
    );
    ensure!(
        source.media_type.eq_ignore_ascii_case("image/vnd.djvu"),
        "视觉任务来源 MIME 不是 image/vnd.djvu"
    );
    validate_djvu_bytes(&source.bytes)
}

pub(crate) fn validate_djvu_bytes(bytes: &[u8]) -> Result<()> {
    ensure!(!bytes.is_empty(), "DjVu 来源内容为空");
    ensure!(
        bytes.len() <= MAX_DJVU_SOURCE_BYTES,
        "DjVu 来源超过 {MAX_DJVU_SOURCE_BYTES} 字节上限"
    );
    ensure!(
        djvu_form_type(bytes).is_some(),
        "DjVu 来源缺少 IFF FORM 签名"
    );
    Ok(())
}

fn render_djvu_blocking(
    document_id: String,
    document_revision: Revision,
    targets: Vec<DjvuRasterTarget>,
    profile: RenderProfile,
    descriptor: RendererDescriptor,
    source_bytes: Vec<u8>,
    control: RenderControl,
    emitter: RenderPageEmitter,
) -> Result<usize> {
    control.checkpoint().map_err(anyhow::Error::new)?;
    validate_djvu_bytes(&source_bytes)?;
    let document = Document::from_bytes(source_bytes).context("djvu-rs 无法解析 DjVu 来源")?;
    let page_count = u32::try_from(document.page_count()).unwrap_or(u32::MAX);
    ensure!(page_count > 0, "DjVu 不包含页面");
    ensure!(
        page_count <= MAX_DJVU_PAGES as u32,
        "DjVu 超过 {MAX_DJVU_PAGES} 页上限"
    );

    let profile_id = profile.stable_id();
    let mut total_bytes = 0_u64;
    for target in targets {
        control.checkpoint().map_err(anyhow::Error::new)?;
        ensure!(
            target.page <= page_count,
            "DjVu 页面目标指向不存在的第 {} 页",
            target.page
        );
        let (page_index, should_render) = emitter.claim_page();
        if !should_render {
            continue;
        }
        let page = document
            .page((target.page - 1) as usize)
            .with_context(|| format!("无法打开 DjVu 第 {} 页", target.page))?;
        let (width, height) =
            raster_dimensions(page.display_width(), page.display_height(), &profile)?;
        control.checkpoint().map_err(anyhow::Error::new)?;
        let pixmap = page
            .render_to_size(width, height)
            .with_context(|| format!("无法渲染 DjVu 第 {} 页", target.page))?;
        control.checkpoint().map_err(anyhow::Error::new)?;
        ensure!(
            pixmap.width == width && pixmap.height == height,
            "DjVu 第 {} 页渲染尺寸与目标不一致",
            target.page
        );
        let bytes =
            encode_png(pixmap).with_context(|| format!("无法编码 DjVu 第 {} 页", target.page))?;
        ensure!(
            bytes.starts_with(PNG_SIGNATURE),
            "DjVu 第 {} 页没有输出 PNG",
            target.page
        );
        ensure!(
            bytes.len() as u64 <= MAX_PAGE_PNG_BYTES,
            "DjVu 第 {} 页超过 {MAX_PAGE_PNG_BYTES} 字节单页上限",
            target.page
        );
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .context("DjVu 页面输出总大小溢出")?;
        ensure!(
            total_bytes <= MAX_TOTAL_PNG_BYTES,
            "DjVu 页面输出超过 {MAX_TOTAL_PNG_BYTES} 字节总上限"
        );
        let id_seed = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            document_id,
            document_revision.get(),
            target.content_unit_id,
            target.page,
            descriptor.version,
            profile_id
        );
        emitter.emit_blocking(RenderedVisualPage {
            id: deterministic_id("visual-page", id_seed),
            page_index,
            content_unit_id: Some(target.content_unit_id),
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
                unit_revision: target.unit_revision,
                profile_id: profile_id.clone(),
            },
        })?;
    }
    let total_pages = emitter.total_pages();
    ensure!(
        emitter.resume_from() <= total_pages,
        "DjVu 视觉任务断点超过当前页面总数"
    );
    Ok(total_pages)
}

fn encode_png(pixmap: Pixmap) -> Result<Vec<u8>> {
    let image = image::RgbaImage::from_raw(pixmap.width, pixmap.height, pixmap.data)
        .context("DjVu 页面像素缓冲大小与渲染尺寸不一致")?;
    let mut output = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .context("无法编码 DjVu 页面 PNG")?;
    Ok(output.into_inner())
}

/// Fits the page into the profile viewport with the same sizing rule as the
/// Windows PDF renderer, then clamps the result to the shared raster ceilings.
fn raster_dimensions(
    page_width: u32,
    page_height: u32,
    profile: &RenderProfile,
) -> Result<(u32, u32)> {
    ensure!(
        page_width > 0 && page_height > 0,
        "DjVu 页面尺寸无效（{page_width}x{page_height}）"
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
        "DjVu 页面目标尺寸超过安全上限"
    );
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::ensure;
    use djvu_rs::{Bitmap, jb2_encode::BUNDLE_DEFAULT_DPI};

    use super::*;
    use crate::{
        document::{
            AssetRef, AssetRole, Block, BlockDocument, BookDocument, ContentUnit, ContentUnitKind,
        },
        preview::{PreviewFuture, VisualDocumentSource},
    };

    fn djvm_fixture(page_count: u32) -> Vec<u8> {
        let pages = (0..page_count)
            .map(|index| {
                let mut page = Bitmap::new(64, 48);
                for y in 4..44 {
                    page.set_black(index % 60, y);
                    page.set_black(y % 60, 8);
                }
                page
            })
            .collect::<Vec<_>>();
        djvu_rs::jb2_encode::encode_djvm_bundle_jb2(
            &pages,
            page_count as usize + 1,
            BUNDLE_DEFAULT_DPI,
        )
    }

    fn test_document(bytes: &[u8], page_count: u32) -> BookDocument {
        let original = AssetRef::from_bytes(
            AssetRole::OriginalSource,
            "image/vnd.djvu",
            Some("sample.djvu".to_string()),
            bytes,
        );
        let mut document = BookDocument::new(
            "book-djvu",
            "DjVu visual test",
            BookSource::imported(
                BookFormat::Djvu,
                original.id.clone(),
                Some("sample.djvu".to_string()),
            ),
        );
        document.revision = Revision::new(9);
        document.assets.push(original);
        for page in 1..=page_count {
            let mut unit = ContentUnit::new(
                format!("unit-{page}"),
                ContentUnitKind::Page,
                format!("第 {page} 页"),
                format!("<p>page {page}</p>"),
                BlockDocument::new(vec![Block::paragraph(
                    format!("block-{page}"),
                    format!("page {page}"),
                )]),
            )
            .with_source_locator(SourceLocator::djvu_page(page));
            unit.revision = Revision::new(9);
            document.units.push(unit);
        }
        document.validate().expect("valid DjVu document");
        document
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
                ensure!(source_id == "source-djvu", "wrong source");
                ensure!(revision == expected_revision, "wrong revision");
                Ok(payload)
            })
        }
    }

    fn request(
        document: BookDocument,
        bytes: Vec<u8>,
        unit_ids: Vec<String>,
    ) -> RenderUnitsRequest {
        let source: Arc<dyn VisualDocumentSource> = Arc::new(TestSource {
            document: document.clone(),
            payload: VisualSourcePayload {
                format: "djvu".to_string(),
                source_kind: "original".to_string(),
                media_type: "image/vnd.djvu".to_string(),
                bytes,
            },
        });
        RenderUnitsRequest {
            document: Arc::new(document),
            source_id: "source-djvu".to_string(),
            unit_ids,
            profile: RenderProfile::default(),
            asset_source: Some(source),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn renders_one_png_per_djvu_page_with_a_one_to_one_unit_mapping() {
        let bytes = djvm_fixture(2);
        let document = test_document(&bytes, 2);
        let pages = DjvuPngRenderer
            .render_units(
                request(document, bytes, Vec::new()),
                RenderControl::default(),
            )
            .await
            .expect("render DjVu pages");
        assert_eq!(pages.len(), 2);
        for (index, page) in pages.iter().enumerate() {
            assert!(page.bytes.starts_with(PNG_SIGNATURE));
            assert_eq!(page.media_type, "image/png");
            assert_eq!(page.page_index, index);
            assert_eq!(
                page.content_unit_id.as_deref(),
                Some(format!("unit-{}", index + 1).as_str())
            );
            assert_eq!(
                page.locator,
                DocumentLocator::unit("book-djvu", format!("unit-{}", index + 1))
                    .with_source(SourceLocator::djvu_page((index + 1) as u32))
            );
            assert_eq!(page.metadata.renderer, DJVU_RENDERER_NAME);
            let decoded = image::load_from_memory_with_format(&page.bytes, image::ImageFormat::Png)
                .expect("decode rendered PNG");
            assert_eq!(decoded.width(), page.width);
            assert_eq!(decoded.height(), page.height);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn honors_unit_selection_and_rejects_invalid_sources() {
        let bytes = djvm_fixture(2);
        let document = test_document(&bytes, 2);
        let selected = DjvuPngRenderer
            .render_units(
                request(document.clone(), bytes.clone(), vec!["unit-2".to_string()]),
                RenderControl::default(),
            )
            .await
            .expect("render the selected DjVu page");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].content_unit_id.as_deref(), Some("unit-2"));
        assert_eq!(selected[0].page_index, 0);
        assert_eq!(
            selected[0].locator.source,
            Some(SourceLocator::djvu_page(2))
        );

        for payload in [
            VisualSourcePayload {
                format: "pdf".to_string(),
                source_kind: "original".to_string(),
                media_type: "image/vnd.djvu".to_string(),
                bytes: bytes.clone(),
            },
            VisualSourcePayload {
                format: "djvu".to_string(),
                source_kind: "normalized".to_string(),
                media_type: "image/vnd.djvu".to_string(),
                bytes: bytes.clone(),
            },
            VisualSourcePayload {
                format: "djvu".to_string(),
                source_kind: "original".to_string(),
                media_type: "image/vnd.djvu".to_string(),
                bytes: b"not a djvu".to_vec(),
            },
        ] {
            let source: Arc<dyn VisualDocumentSource> = Arc::new(TestSource {
                document: document.clone(),
                payload,
            });
            let error = DjvuPngRenderer
                .render_units(
                    RenderUnitsRequest {
                        document: Arc::new(document.clone()),
                        source_id: "source-djvu".to_string(),
                        unit_ids: Vec::new(),
                        profile: RenderProfile::default(),
                        asset_source: Some(source),
                    },
                    RenderControl::default(),
                )
                .await
                .expect_err("invalid DjVu sources must be rejected");
            let detail = format!("{error:#}");
            assert!(
                detail.contains("不是 DjVu")
                    || detail.contains("规范化编辑后")
                    || detail.contains("IFF FORM"),
                "{detail}"
            );
        }
    }

    #[test]
    fn raster_dimensions_fit_the_viewport_and_respect_the_ceilings() {
        // The default profile is 1200x1600, matching the Windows PDF renderer's
        // sizing rule: a scanned page is downscaled proportionally to fit.
        let profile = RenderProfile::default();
        let (width, height) = raster_dimensions(4_000, 6_000, &profile).unwrap();
        assert!(width <= profile.viewport_width && height <= profile.viewport_height);
        assert!(u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS);
        assert_eq!((width, height), (1_067, 1_600));
        // A page smaller than the viewport is scaled up to it, exactly like the
        // PDF renderer, so a tiny DjVu page is still readable.
        assert_eq!(raster_dimensions(60, 40, &profile).unwrap(), (1_200, 800));
        assert!(raster_dimensions(0, 40, &profile).is_err());

        // A very large viewport hits the per-dimension cap and then the total
        // pixel cap.
        let huge = RenderProfile {
            viewport_width: 16_384,
            viewport_height: 16_384,
            ..RenderProfile::default()
        };
        let (width, height) = raster_dimensions(10_000, 10_000, &huge).unwrap();
        assert!(width <= MAX_RASTER_DIMENSION && height <= MAX_RASTER_DIMENSION);
        assert!(u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS);
    }
}
