//! Shared fixtures and test doubles for the preview tests.

mod coordinator;

mod persistence;

mod render;

use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::*;
use crate::document::{
    AssetRef, AssetRole, Block, BlockDocument, ContentUnit, ContentUnitKind, SourceLocator,
    TableCell, TableRow,
};

fn sample_document() -> BookDocument {
    let mut document = BookDocument::created("book-visual", "视觉测试");
    document.revision = Revision::new(3);
    document.units.push(ContentUnit::new(
        "unit-1",
        ContentUnitKind::Chapter,
        "第一章",
        "<h1>第一章</h1>",
        BlockDocument::new(vec![Block::paragraph(
            "block-1",
            "包含 <script> 和中文的正文。",
        )]),
    ));
    document
}

fn png_fixture(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, image::ImageFormat::Png)
        .expect("encode PNG fixture");
    output.into_inner()
}

struct ImageDocumentSource {
    document: BookDocument,
    payload: VisualAssetPayload,
}

impl VisualDocumentSource for ImageDocumentSource {
    fn load_document(
        &self,
        book_id: String,
        revision: Revision,
    ) -> PreviewFuture<'_, BookDocument> {
        let document = self.document.clone();
        Box::pin(async move {
            ensure!(document.id == book_id, "wrong test book");
            ensure!(document.revision == revision, "wrong test revision");
            Ok(document)
        })
    }

    fn load_asset(
        &self,
        book_id: String,
        asset_id: String,
    ) -> PreviewFuture<'_, VisualAssetPayload> {
        let expected_book_id = self.document.id.clone();
        let payload = self.payload.clone();
        Box::pin(async move {
            ensure!(book_id == expected_book_id, "wrong image book");
            ensure!(asset_id == "asset-image", "wrong image asset");
            Ok(payload)
        })
    }
}

struct MemoryDocumentSource(BookDocument);

impl VisualDocumentSource for MemoryDocumentSource {
    fn load_document(
        &self,
        book_id: String,
        revision: Revision,
    ) -> PreviewFuture<'_, BookDocument> {
        let document = self.0.clone();
        Box::pin(async move {
            ensure!(document.id == book_id, "wrong test book");
            ensure!(document.revision == revision, "wrong test revision");
            Ok(document)
        })
    }
}

#[derive(Default)]
struct MemoryPageSink {
    store: Arc<MemoryVisualJobStore>,
    published: Mutex<HashMap<String, Vec<RenderedVisualPage>>>,
    staging: Mutex<HashMap<String, Vec<RenderedVisualPage>>>,
    checkpoints: Mutex<Vec<(String, usize)>>,
}

impl MemoryPageSink {
    fn new(store: Arc<MemoryVisualJobStore>) -> Self {
        Self {
            store,
            ..Self::default()
        }
    }
}

impl VisualPageSink for MemoryPageSink {
    fn reset_staging(&self, spec: VisualJobSpec) -> PreviewFuture<'_, ()> {
        Box::pin(async move {
            self.staging
                .lock()
                .map_err(|_| anyhow::anyhow!("test staging sink poisoned"))?
                .remove(&spec.id);
            Ok(())
        })
    }

    fn checkpoint_page(
        &self,
        spec: VisualJobSpec,
        page: RenderedVisualPage,
        completed_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        Box::pin(async move {
            let mut jobs = self
                .store
                .jobs
                .lock()
                .map_err(|_| anyhow::anyhow!("test job store poisoned"))?;
            let record = jobs
                .get_mut(&spec.id)
                .context("test checkpoint job is missing")?;
            ensure!(record.spec == spec, "test checkpoint spec changed");
            ensure!(
                record.state == VisualJobState::Running,
                "test checkpoint job is not running"
            );
            let mut staging = self
                .staging
                .lock()
                .map_err(|_| anyhow::anyhow!("test staging sink poisoned"))?;
            let pages = staging.entry(spec.id.clone()).or_default();
            ensure!(
                page.page_index == pages.len() && completed_pages == pages.len() + 1,
                "test staging prefix is not contiguous"
            );
            pages.push(page);
            record.completed_pages = completed_pages;
            self.checkpoints
                .lock()
                .map_err(|_| anyhow::anyhow!("test checkpoint log poisoned"))?
                .push((spec.id, completed_pages - 1));
            Ok(())
        })
    }

    fn commit_pages(&self, spec: VisualJobSpec, total_pages: usize) -> PreviewFuture<'_, ()> {
        Box::pin(async move {
            let mut jobs = self
                .store
                .jobs
                .lock()
                .map_err(|_| anyhow::anyhow!("test job store poisoned"))?;
            let record = jobs
                .get_mut(&spec.id)
                .context("test publication job is missing")?;
            ensure!(record.spec == spec, "test publication spec changed");
            ensure!(
                record.state == VisualJobState::Running
                    && !record.pause_requested
                    && !record.cancel_requested
                    && record.completed_pages == total_pages,
                "test publication job state changed"
            );
            let mut staging = self
                .staging
                .lock()
                .map_err(|_| anyhow::anyhow!("test staging sink poisoned"))?;
            let pages = staging
                .remove(&spec.id)
                .context("test publication staging is missing")?;
            ensure!(
                pages.len() == total_pages
                    && pages
                        .iter()
                        .enumerate()
                        .all(|(index, page)| page.page_index == index),
                "test publication staging is incomplete"
            );
            self.published
                .lock()
                .map_err(|_| anyhow::anyhow!("test publication sink poisoned"))?
                .insert(spec.id, pages);
            record.state = VisualJobState::Succeeded;
            record.error = None;
            record.finished_at = Some(record.updated_at);
            Ok(())
        })
    }
}

struct SlowRenderer;

impl VisualRenderer for SlowRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: "slow-test".to_string(),
            version: "1".to_string(),
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
            for _ in 0..50 {
                control.checkpoint().map_err(anyhow::Error::new)?;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let descriptor = self.descriptor();
            for _ in 0..3 {
                control.checkpoint().map_err(anyhow::Error::new)?;
                let (page_index, should_render) = emitter.claim_page();
                if should_render {
                    emitter
                        .emit(test_rendered_page(&request, &descriptor, page_index))
                        .await?;
                }
            }
            Ok(emitter.total_pages())
        })
    }
}

struct GatedRenderer {
    permits: tokio::sync::Semaphore,
    emitted: Mutex<Vec<usize>>,
}

impl GatedRenderer {
    fn new() -> Self {
        Self {
            permits: tokio::sync::Semaphore::new(0),
            emitted: Mutex::new(Vec::new()),
        }
    }
}

impl VisualRenderer for GatedRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: "gated-test".to_string(),
            version: "1".to_string(),
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
            let descriptor = self.descriptor();
            for _ in 0..3 {
                control.checkpoint().map_err(anyhow::Error::new)?;
                let (page_index, should_render) = emitter.claim_page();
                if !should_render {
                    continue;
                }
                self.permits
                    .acquire()
                    .await
                    .context("test renderer gate closed")?
                    .forget();
                emitter
                    .emit(test_rendered_page(&request, &descriptor, page_index))
                    .await?;
                self.emitted
                    .lock()
                    .map_err(|_| anyhow::anyhow!("test renderer log poisoned"))?
                    .push(page_index);
            }
            Ok(emitter.total_pages())
        })
    }
}

struct FailOnceRenderer {
    failed: AtomicBool,
    calls: AtomicUsize,
    emitted: Mutex<Vec<usize>>,
}

impl VisualRenderer for FailOnceRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: "fail-once-test".to_string(),
            version: "1".to_string(),
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            let fail_this_attempt = !self.failed.swap(true, Ordering::SeqCst);
            let descriptor = self.descriptor();
            for _ in 0..3 {
                control.checkpoint().map_err(anyhow::Error::new)?;
                let (page_index, should_render) = emitter.claim_page();
                if should_render {
                    emitter
                        .emit(test_rendered_page(&request, &descriptor, page_index))
                        .await?;
                    self.emitted
                        .lock()
                        .map_err(|_| anyhow::anyhow!("test renderer log poisoned"))?
                        .push(page_index);
                }
                if fail_this_attempt && page_index == 0 {
                    anyhow::bail!("intentional first failure");
                }
            }
            Ok(emitter.total_pages())
        })
    }
}

fn test_rendered_page(
    request: &RenderUnitsRequest,
    descriptor: &RendererDescriptor,
    page_index: usize,
) -> RenderedVisualPage {
    let unit = &request.document.units[0];
    RenderedVisualPage {
        id: format!("{}-page-{page_index}", descriptor.renderer),
        page_index,
        content_unit_id: Some(unit.id.clone()),
        width: 10,
        height: 10,
        media_type: "image/png".to_string(),
        bytes: vec![page_index as u8 + 1],
        locator: DocumentLocator::unit(request.document.id.clone(), unit.id.clone()),
        metadata: RenderMetadata {
            renderer: descriptor.renderer.clone(),
            renderer_version: descriptor.version.clone(),
            fidelity: descriptor.fidelity,
            document_revision: request.document.revision,
            unit_revision: unit.revision,
            profile_id: request.profile.stable_id(),
        },
    }
}

fn spec(id: &str, renderer: &dyn VisualRenderer) -> VisualJobSpec {
    VisualJobSpec::from_renderer(
        id,
        "book-visual",
        "source-visual",
        Revision::new(3),
        Vec::new(),
        RenderProfile::default(),
        renderer,
    )
}

async fn wait_for_completed_pages(
    coordinator: &VisualJobCoordinator,
    job_id: &str,
    expected: usize,
) -> VisualJobRecord {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let record = coordinator
            .status(job_id)
            .await
            .expect("read visual test job")
            .expect("visual test job exists");
        if record.completed_pages == expected {
            return record;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "visual test job did not reach checkpoint {expected}: {record:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
