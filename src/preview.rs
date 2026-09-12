//! Visual rendering contracts and resumable background coordination.
//!
//! The module owns renderer identity and job state, but not document loading,
//! blob persistence, or UI state. That keeps both object-store paths and GPUI
//! entities outside rendering implementations.

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    io::Cursor,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, task::JoinHandle};

use crate::{
    db,
    document::{
        Block, BookDocument, ContentUnit, ContentUnitKind, DocumentLocator, Inline, Revision,
        SourceLocator, TableRow, deterministic_id,
    },
    job_diagnostics::{JobLogEvent, JobLogMetrics, classify_error, record_for_database},
};

pub const PDFJS_VERSION: &str = "5.7.284";
pub const PDFJS_VIEWER_HTML: &str = include_str!("../assets/pdfjs/viewer.html");
pub const PDFJS_VIEWER_SCRIPT: &str = include_str!("../assets/pdfjs/viewer.mjs");
const PERSISTED_VISUAL_SCAN_INTERVAL: Duration = Duration::from_millis(500);
const PERSISTED_VISUAL_SCAN_LIMIT: usize = 32;
const MAX_VISUAL_IMAGE_BYTES: usize = 12 * 1024 * 1024;
const MAX_STRUCTURAL_SVG_BYTES: usize = 4 * 1024 * 1024;
const MAX_RASTER_DIMENSION: u32 = 4_096;
const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_VISUAL_BATCH_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SLIDE_IMAGES: usize = 64;
const MAX_STRUCTURAL_PAGES_PER_UNIT: usize = 10_000;
const MAX_XLSX_COLUMNS: u32 = 16_384;
const MAX_XLSX_ROWS: u32 = 1_048_576;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
static NEXT_VISUAL_LOG_RUN_ID: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    static VISUAL_LOG_RUN: (u64, Instant, u32);
}

mod pdfjs_routes {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/pdfjs/routes.rs"
    ));
}

pub type PreviewFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderFidelity {
    /// Canonical content after editing. This is the authoritative current
    /// visual representation after a source-format document is modified.
    Normalized,
    /// Portable layout assembled from parsed structure on every machine.
    Structural,
    /// Explicit, per-book Microsoft Office COM enhancement.
    OfficeEnhanced,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderProfile {
    pub name: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Device scale encoded as thousandths to keep identifiers independent of
    /// floating-point formatting.
    pub scale_milli: u32,
    pub color_scheme: String,
}

impl Default for RenderProfile {
    fn default() -> Self {
        Self {
            name: "desktop".to_string(),
            viewport_width: 1_200,
            viewport_height: 1_600,
            scale_milli: 1_000,
            color_scheme: "light".to_string(),
        }
    }
}

impl RenderProfile {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "渲染配置名称不能为空");
        ensure!(
            (320..=16_384).contains(&self.viewport_width),
            "渲染宽度必须在 320..=16384 之间"
        );
        ensure!(
            (320..=16_384).contains(&self.viewport_height),
            "渲染高度必须在 320..=16384 之间"
        );
        ensure!(
            (250..=8_000).contains(&self.scale_milli),
            "渲染缩放必须在 0.25..=8.0 之间"
        );
        ensure!(
            matches!(self.color_scheme.as_str(), "light" | "dark"),
            "渲染配色只能是 light 或 dark"
        );
        Ok(())
    }

    pub fn stable_id(&self) -> String {
        let json = serde_json::to_vec(self).expect("render profile serialization is infallible");
        deterministic_id("render-profile", json)
    }

    pub fn scale(&self) -> f64 {
        f64::from(self.scale_milli) / 1_000.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererDescriptor {
    pub renderer: String,
    pub version: String,
    pub fidelity: RenderFidelity,
}

impl RendererDescriptor {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.renderer.trim().is_empty(), "renderer 名称不能为空");
        ensure!(!self.version.trim().is_empty(), "renderer 版本不能为空");
        ensure!(
            !self.renderer.chars().any(char::is_control)
                && !self.version.chars().any(char::is_control),
            "renderer 名称和版本不能包含控制字符"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderMetadata {
    pub renderer: String,
    pub renderer_version: String,
    pub fidelity: RenderFidelity,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub profile_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedVisualPage {
    pub id: String,
    /// Zero-based order across the requested rendering batch.
    pub page_index: usize,
    /// The exact canonical content unit represented by this page. Whole-file
    /// Office PDF previews deliberately leave this empty because Office does
    /// not expose a trustworthy Word-section/worksheet-to-page mapping.
    pub content_unit_id: Option<String>,
    pub width: u32,
    pub height: u32,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub locator: DocumentLocator,
    pub metadata: RenderMetadata,
}

/// Stable, deliberately non-canonical locator identity for a whole-document
/// Office PDF page. It lets the preview UI retain a valid, unique locator
/// shape without pretending that the page belongs to a real content unit.
pub(crate) fn office_preview_unit_id(book_id: &str, source_id: &str) -> String {
    deterministic_id("office-preview", format!("{book_id}\0{source_id}"))
}

/// Page-level output boundary used by resumable renderers. A renderer claims
/// every logical page in stable order, but only rasterizes and emits pages at
/// or after `resume_from`. The coordinator keeps the channel bounded so page
/// bytes cannot accumulate outside the durable staging store.
#[derive(Clone)]
pub struct RenderPageEmitter {
    sender: async_channel::Sender<RenderedVisualPage>,
    resume_from: usize,
    next_page: Arc<AtomicUsize>,
}

impl fmt::Debug for RenderPageEmitter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderPageEmitter")
            .field("resume_from", &self.resume_from)
            .field("next_page", &self.next_page.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl RenderPageEmitter {
    fn bounded(
        resume_from: usize,
        capacity: usize,
    ) -> (Self, async_channel::Receiver<RenderedVisualPage>) {
        let (sender, receiver) = async_channel::bounded(capacity.max(1));
        (
            Self {
                sender,
                resume_from,
                next_page: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
        )
    }

    fn unbounded(resume_from: usize) -> (Self, async_channel::Receiver<RenderedVisualPage>) {
        let (sender, receiver) = async_channel::unbounded();
        (
            Self {
                sender,
                resume_from,
                next_page: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
        )
    }

    /// Reserves the next stable global page index. `false` means the page is
    /// already present in the durable staging prefix and must not be rendered
    /// or emitted again.
    pub fn claim_page(&self) -> (usize, bool) {
        let page_index = self.next_page.fetch_add(1, Ordering::AcqRel);
        (page_index, page_index >= self.resume_from)
    }

    pub fn resume_from(&self) -> usize {
        self.resume_from
    }

    pub fn total_pages(&self) -> usize {
        self.next_page.load(Ordering::Acquire)
    }

    pub async fn emit(&self, page: RenderedVisualPage) -> Result<()> {
        self.validate_emitted_page(&page)?;
        self.sender
            .send(page)
            .await
            .context("视觉页面断点接收端已经关闭")
    }

    /// Synchronous emit for renderers that rasterize on a blocking thread.
    /// The channel is bounded, so this may park the calling worker; callers
    /// must already be on a dedicated blocking thread.
    pub(crate) fn emit_blocking(&self, page: RenderedVisualPage) -> Result<()> {
        self.validate_emitted_page(&page)?;
        self.sender
            .send_blocking(page)
            .context("视觉页面断点接收端已经关闭")
    }

    fn validate_emitted_page(&self, page: &RenderedVisualPage) -> Result<()> {
        ensure!(
            page.page_index >= self.resume_from
                && page.page_index < self.next_page.load(Ordering::Acquire),
            "renderer 发出了未认领或已经完成的视觉页面"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct RenderUnitsRequest {
    pub document: Arc<BookDocument>,
    /// The exact persisted source revision being rendered. Renderers that
    /// consume native source bytes use this opaque ID to avoid accidentally
    /// rendering an older imported original after canonical editing.
    pub source_id: String,
    /// Empty means all content units in linear order.
    pub unit_ids: Vec<String>,
    pub profile: RenderProfile,
    /// Production coordinators provide the same book-scoped source used to
    /// load the canonical document. Standalone renderers may omit it only for
    /// documents that do not contain visual image references.
    pub asset_source: Option<Arc<dyn VisualDocumentSource>>,
}

impl fmt::Debug for RenderUnitsRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderUnitsRequest")
            .field("book_id", &self.document.id)
            .field("revision", &self.document.revision)
            .field("source_id", &self.source_id)
            .field("unit_ids", &self.unit_ids)
            .field("profile", &self.profile)
            .finish()
    }
}

pub trait VisualRenderer: Send + Sync {
    fn descriptor(&self) -> RendererDescriptor;

    /// Renders a stable logical page sequence. Implementations must claim each
    /// page before expensive raster work, skip claims before the emitter's
    /// durable resume offset, emit every remaining page exactly once, and call
    /// [`RenderControl::checkpoint`] between logical pages and expensive
    /// phases. The returned value is the total page count including the
    /// skipped durable prefix.
    fn render_units_resumable<'a>(
        &'a self,
        request: RenderUnitsRequest,
        control: RenderControl,
        emitter: RenderPageEmitter,
    ) -> PreviewFuture<'a, usize>;

    /// Convenience collector for direct previews and renderer tests. Durable
    /// background work uses `render_units_resumable` with a bounded consumer.
    fn render_units<'a>(
        &'a self,
        request: RenderUnitsRequest,
        control: RenderControl,
    ) -> PreviewFuture<'a, Vec<RenderedVisualPage>> {
        Box::pin(async move {
            let (emitter, receiver) = RenderPageEmitter::unbounded(0);
            let total_pages = self
                .render_units_resumable(request, control, emitter)
                .await?;
            let mut pages = Vec::with_capacity(total_pages);
            while let Ok(page) = receiver.try_recv() {
                pages.push(page);
            }
            ensure!(
                pages.len() == total_pages
                    && pages
                        .iter()
                        .enumerate()
                        .all(|(index, page)| page.page_index == index),
                "renderer 收集到的视觉页面不连续"
            );
            Ok(pages)
        })
    }
}

const CONTROL_RUNNING: u8 = 0;
const CONTROL_PAUSED: u8 = 1;
const CONTROL_CANCELLED: u8 = 2;

#[derive(Clone, Debug, Default)]
pub struct RenderControl {
    state: Arc<AtomicU8>,
}

impl RenderControl {
    pub fn request_pause(&self) {
        let _ = self.state.compare_exchange(
            CONTROL_RUNNING,
            CONTROL_PAUSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn request_cancel(&self) {
        self.state.store(CONTROL_CANCELLED, Ordering::Release);
    }

    pub fn checkpoint(&self) -> std::result::Result<(), RenderInterrupted> {
        match self.state.load(Ordering::Acquire) {
            CONTROL_RUNNING => Ok(()),
            CONTROL_PAUSED => Err(RenderInterrupted::Paused),
            _ => Err(RenderInterrupted::Cancelled),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderInterrupted {
    Paused,
    Cancelled,
}

impl fmt::Display for RenderInterrupted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Paused => formatter.write_str("视觉渲染已暂停"),
            Self::Cancelled => formatter.write_str("视觉渲染已取消"),
        }
    }
}

impl Error for RenderInterrupted {}

/// Portable, format-aware structural renderer. Flow content is paginated,
/// slides stay atomic, and worksheet tables are tiled in both dimensions.
/// Text is first written to an escaped, resource-isolated SVG and then
/// rasterized on Tokio's blocking pool. Only PNG pages are published.
#[derive(Clone, Copy, Debug, Default)]
pub struct StructuralPngRenderer;

impl VisualRenderer for StructuralPngRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            renderer: "moye-structural-png".to_string(),
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
struct LocatedLine {
    text: String,
    block_id: Option<String>,
    start_byte: u64,
    end_byte: u64,
}

#[derive(Debug)]
struct LoadedVisualImage {
    reference: VisualImageReference,
    prepared: PreparedVisualImage,
}

#[allow(clippy::too_many_arguments)]
async fn render_structural_flow(
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
async fn render_structural_slide(
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct A1Range {
    start_col: u32,
    start_row: u32,
    end_col: u32,
    end_row: u32,
}

impl A1Range {
    fn from_dimensions(
        start_col: u32,
        start_row: u32,
        columns: usize,
        rows: usize,
    ) -> Result<Self> {
        ensure!(columns > 0 && rows > 0, "工作表视觉区域不能为空");
        let columns = u32::try_from(columns).context("工作表视觉列数过大")?;
        let rows = u32::try_from(rows).context("工作表视觉行数过大")?;
        let end_col = start_col
            .checked_add(columns - 1)
            .context("工作表视觉列范围溢出")?;
        let end_row = start_row
            .checked_add(rows - 1)
            .context("工作表视觉行范围溢出")?;
        ensure!(
            end_col < MAX_XLSX_COLUMNS && end_row < MAX_XLSX_ROWS,
            "工作表视觉区域超过 XLSX 的有效边界"
        );
        Ok(Self {
            start_col,
            start_row,
            end_col,
            end_row,
        })
    }

    fn to_a1(self) -> String {
        format!(
            "{}{}:{}{}",
            a1_column_name(self.start_col),
            self.start_row + 1,
            a1_column_name(self.end_col),
            self.end_row + 1
        )
    }
}

#[allow(clippy::too_many_arguments)]
async fn render_structural_worksheet(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
    descriptor: &RendererDescriptor,
    profile_id: &str,
    emitter: &RenderPageEmitter,
    total_bytes: &mut u64,
) -> Result<()> {
    let Some((table_id, rows)) = worksheet_table_rows(unit) else {
        return render_structural_flow(
            request,
            unit,
            control,
            descriptor,
            profile_id,
            emitter,
            total_bytes,
        )
        .await;
    };
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() || column_count == 0 {
        return render_structural_flow(
            request,
            unit,
            control,
            descriptor,
            profile_id,
            emitter,
            total_bytes,
        )
        .await;
    }

    let (sheet_name, source_start_col, source_start_row) = worksheet_source_origin(unit)?;
    let rows_per_page =
        ((request.profile.viewport_height.saturating_sub(192)) / 32).max(1) as usize;
    let columns_per_page =
        ((request.profile.viewport_width.saturating_sub(120)) / 180).max(1) as usize;
    let row_page_count = rows.len().div_ceil(rows_per_page);
    let column_page_count = column_count.div_ceil(columns_per_page);
    let page_count = row_page_count
        .checked_mul(column_page_count)
        .context("工作表视觉分页数量溢出")?;
    ensure!(
        page_count <= MAX_STRUCTURAL_PAGES_PER_UNIT,
        "单个工作表的结构化页面超过 {MAX_STRUCTURAL_PAGES_PER_UNIT} 页上限"
    );

    let mut unit_page = 0_usize;
    for row_start in (0..rows.len()).step_by(rows_per_page) {
        let row_end = (row_start + rows_per_page).min(rows.len());
        for column_start in (0..column_count).step_by(columns_per_page) {
            control.checkpoint().map_err(anyhow::Error::new)?;
            let (page_index, should_render) = emitter.claim_page();
            if !should_render {
                unit_page += 1;
                continue;
            }
            let column_end = (column_start + columns_per_page).min(column_count);
            let page_range = A1Range::from_dimensions(
                source_start_col
                    .checked_add(u32::try_from(column_start).context("工作表列偏移过大")?)
                    .context("工作表列范围溢出")?,
                source_start_row
                    .checked_add(u32::try_from(row_start).context("工作表行偏移过大")?)
                    .context("工作表行范围溢出")?,
                column_end - column_start,
                row_end - row_start,
            )?;
            let range_text = page_range.to_a1();
            let page_lines = worksheet_slice_lines(
                &unit.title,
                &range_text,
                &rows,
                row_start,
                row_end,
                column_start,
                column_end,
            );
            let profile = request.profile.clone();
            let raster_control = control.clone();
            let raster = tokio::task::spawn_blocking(move || {
                render_structural_png(&page_lines, &profile, &raster_control)
            })
            .await
            .context("工作表结构化 PNG 渲染线程异常退出")??;
            add_visual_page_bytes(total_bytes, raster.bytes.len())?;
            let metadata = render_metadata(request, unit, descriptor, profile_id);
            let locator =
                DocumentLocator::block(&request.document.id, &unit.id, &table_id).with_source(
                    SourceLocator::worksheet(sheet_name.clone(), Some(range_text.clone())),
                );
            let id_seed = format!(
                "{}\0{}\0{}\0worksheet\0{}\0{}\0{}",
                request.document.id,
                request.document.revision.get(),
                unit.id,
                range_text,
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
            unit_page += 1;
            tokio::task::yield_now().await;
        }
    }
    debug_assert_eq!(unit_page, page_count);

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

fn worksheet_table_rows(unit: &ContentUnit) -> Option<(String, Vec<Vec<String>>)> {
    unit.document.blocks.iter().find_map(|block| {
        let Block::Table { id, header, rows } = block else {
            return None;
        };
        let mut output = Vec::with_capacity(rows.len() + usize::from(header.is_some()));
        if let Some(header) = header {
            output.push(worksheet_row_values(header));
        }
        output.extend(rows.iter().map(worksheet_row_values));
        Some((id.clone(), output))
    })
}

fn worksheet_row_values(row: &TableRow) -> Vec<String> {
    row.cells.iter().map(|cell| cell.plain_text()).collect()
}

fn worksheet_source_origin(unit: &ContentUnit) -> Result<(String, u32, u32)> {
    let Some(SourceLocator::Worksheet { name, range }) = unit.source_locator.as_ref() else {
        return Ok((unit.title.clone(), 0, 0));
    };
    let Some(range) = range.as_deref() else {
        return Ok((name.clone(), 0, 0));
    };
    let parsed = parse_a1_range(range)
        .with_context(|| format!("工作表 {} 的来源区域不是有效的 A1 范围：{range}", name))?;
    Ok((name.clone(), parsed.start_col, parsed.start_row))
}

fn worksheet_slice_lines(
    title: &str,
    range: &str,
    rows: &[Vec<String>],
    row_start: usize,
    row_end: usize,
    column_start: usize,
    column_end: usize,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(row_end - row_start + 1);
    lines.push(format!("{title} · {range}"));
    let available_columns = column_end.saturating_sub(column_start).max(1);
    let cell_characters = (72 / available_columns).clamp(6, 36);
    for row in &rows[row_start..row_end] {
        let line = (column_start..column_end)
            .map(|column| {
                truncate_visual_cell(
                    row.get(column).map(String::as_str).unwrap_or_default(),
                    cell_characters,
                )
            })
            .collect::<Vec<_>>()
            .join(" │ ");
        lines.push(line);
    }
    lines
}

fn truncate_visual_cell(value: &str, max_characters: usize) -> String {
    let value = value.replace(['\r', '\n'], " ");
    if value.chars().count() <= max_characters {
        return value;
    }
    let mut output = value
        .chars()
        .take(max_characters.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn parse_a1_range(value: &str) -> Option<A1Range> {
    let mut parts = value.split(':');
    let start = parse_a1_cell(parts.next()?)?;
    let end = match parts.next() {
        Some(value) => parse_a1_cell(value)?,
        None => start,
    };
    if parts.next().is_some() || start.0 > end.0 || start.1 > end.1 {
        return None;
    }
    Some(A1Range {
        start_col: start.0,
        start_row: start.1,
        end_col: end.0,
        end_row: end.1,
    })
}

fn parse_a1_cell(value: &str) -> Option<(u32, u32)> {
    let normalized = value.trim().replace('$', "");
    let digit = normalized.find(|character: char| character.is_ascii_digit())?;
    let (column, row) = normalized.split_at(digit);
    if column.is_empty()
        || row.is_empty()
        || !column.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !row.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut column_index = 0_u32;
    for byte in column.bytes() {
        column_index = column_index
            .checked_mul(26)?
            .checked_add(u32::from(byte.to_ascii_uppercase() - b'A') + 1)?;
    }
    let row = row.parse::<u32>().ok()?;
    if row == 0 || row > MAX_XLSX_ROWS || column_index == 0 || column_index > MAX_XLSX_COLUMNS {
        return None;
    }
    Some((column_index - 1, row - 1))
}

fn a1_column_name(column: u32) -> String {
    let mut value = u64::from(column) + 1;
    let mut bytes = Vec::new();
    while value > 0 {
        value -= 1;
        bytes.push(b'A' + (value % 26) as u8);
        value /= 26;
    }
    bytes.reverse();
    String::from_utf8(bytes).expect("A1 columns are ASCII")
}

#[allow(clippy::too_many_arguments)]
async fn append_structural_image_pages(
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

async fn load_visual_images(
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

async fn load_visual_image(
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

fn render_metadata(
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

fn located_unit_lines(
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

fn locator_for_lines(book_id: &str, unit: &ContentUnit, lines: &[LocatedLine]) -> DocumentLocator {
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

fn wrap_located_text(
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
struct VisualImageReference {
    asset_id: String,
    block_id: String,
}

fn visual_image_references(blocks: &[Block]) -> Vec<VisualImageReference> {
    let mut output = Vec::new();
    collect_block_image_references(blocks, &mut output);
    output
}

fn collect_block_image_references(blocks: &[Block], output: &mut Vec<VisualImageReference>) {
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

fn collect_inline_image_references(
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

fn canonical_visual_image_media_type(media_type: &str) -> Option<&'static str> {
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
struct PreparedVisualImage {
    width: u32,
    height: u32,
    media_type: String,
    bytes: Vec<u8>,
}

fn prepare_visual_image(
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

fn render_structural_png(
    lines: &[String],
    profile: &RenderProfile,
    control: &RenderControl,
) -> Result<PreparedVisualImage> {
    let svg = render_svg_page(lines, profile);
    rasterize_svg(svg.as_bytes(), profile, true, control)
}

fn render_slide_png(
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

fn decode_prepared_visual_image(prepared: &PreparedVisualImage) -> Result<image::RgbaImage> {
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

fn rasterize_svg(
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

fn isolated_svg_options() -> resvg::usvg::Options<'static> {
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

fn fitted_raster_dimensions(
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

fn validate_raster_dimensions(width: u32, height: u32) -> Result<()> {
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

fn add_visual_page_bytes(total: &mut u64, byte_len: usize) -> Result<()> {
    *total = total
        .checked_add(byte_len as u64)
        .context("视觉页面输出总大小溢出")?;
    ensure!(
        *total <= MAX_VISUAL_BATCH_BYTES,
        "视觉页面输出超过 {MAX_VISUAL_BATCH_BYTES} 字节总上限"
    );
    Ok(())
}

fn select_units(request: &RenderUnitsRequest) -> Result<Vec<&crate::document::ContentUnit>> {
    if request.unit_ids.is_empty() {
        return Ok(request.document.units.iter().collect());
    }
    let mut selected = Vec::with_capacity(request.unit_ids.len());
    for unit_id in &request.unit_ids {
        let unit = request
            .document
            .find_unit(unit_id)
            .with_context(|| format!("视觉渲染引用了不存在的内容单元 {unit_id}"))?;
        ensure!(
            !selected
                .iter()
                .any(|selected: &&crate::document::ContentUnit| selected.id == unit.id),
            "视觉渲染不能重复选择同一内容单元 {unit_id}"
        );
        selected.push(unit);
    }
    Ok(selected)
}

fn render_svg_page(lines: &[String], profile: &RenderProfile) -> String {
    let (background, foreground, muted) = if profile.color_scheme == "dark" {
        ("#191919", "#f2efe7", "#a6a29a")
    } else {
        ("#fdfbf6", "#24221f", "#77716a")
    };
    let mut output = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}"><rect width="100%" height="100%" fill="{}"/><g fill="{}" font-family="Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif" font-size="22">"#,
        profile.viewport_width,
        profile.viewport_height,
        profile.viewport_width,
        profile.viewport_height,
        background,
        foreground,
    );
    for (index, line) in lines.iter().enumerate() {
        let y = 80 + index as u32 * 32;
        output.push_str(&format!(
            "<text x=\"60\" y=\"{y}\">{}</text>",
            escape_xml(line)
        ));
    }
    output.push_str(&format!(
        "</g><text x=\"60\" y=\"{}\" fill=\"{}\" font-family=\"Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif\" font-size=\"14\">{}</text></svg>",
        profile.viewport_height.saturating_sub(28),
        muted,
        escape_xml(&profile.name)
    ));
    output
}

fn render_slide_svg_page(lines: &[String], profile: &RenderProfile, image_count: usize) -> String {
    let (background, foreground, muted) = if profile.color_scheme == "dark" {
        ("#191919", "#f2efe7", "#a6a29a")
    } else {
        ("#fdfbf6", "#24221f", "#77716a")
    };
    let text_bottom = if image_count == 0 {
        profile.viewport_height.saturating_sub(48)
    } else {
        profile.viewport_height.saturating_mul(48) / 100
    };
    let line_count = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    let line_height = text_bottom
        .saturating_sub(72)
        .checked_div(line_count)
        .unwrap_or(1)
        .clamp(8, 32);
    let font_size = line_height
        .saturating_mul(3)
        .checked_div(4)
        .unwrap_or(8)
        .max(7);
    let mut output = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}"><rect width="100%" height="100%" fill="{}"/><g fill="{}" font-family="Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif" font-size="{}">"#,
        profile.viewport_width,
        profile.viewport_height,
        profile.viewport_width,
        profile.viewport_height,
        background,
        foreground,
        font_size,
    );
    for (index, line) in lines.iter().enumerate() {
        let y = 52_u32.saturating_add(
            u32::try_from(index)
                .unwrap_or(u32::MAX)
                .saturating_mul(line_height),
        );
        if y >= text_bottom {
            break;
        }
        output.push_str(&format!(
            "<text x=\"60\" y=\"{y}\">{}</text>",
            escape_xml(line)
        ));
    }
    if image_count > 0 {
        output.push_str(&format!(
            "</g><line x1=\"60\" x2=\"{}\" y1=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
            profile.viewport_width.saturating_sub(60),
            profile.viewport_height.saturating_mul(50) / 100,
            profile.viewport_height.saturating_mul(50) / 100,
            muted,
        ));
    } else {
        output.push_str("</g>");
    }
    output.push_str(&format!(
        "<text x=\"60\" y=\"{}\" fill=\"{}\" font-family=\"Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif\" font-size=\"14\">{}</text></svg>",
        profile.viewport_height.saturating_sub(20),
        muted,
        escape_xml(&profile.name)
    ));
    output
}

fn escape_xml(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ if is_xml_character(character) => output.push(character),
            _ => output.push('\u{fffd}'),
        }
    }
    output
}

fn is_xml_character(character: char) -> bool {
    matches!(character, '\u{9}' | '\u{a}' | '\u{d}')
        || matches!(character as u32, 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualJobState {
    Queued,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

impl VisualJobState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualJobSpec {
    pub id: String,
    pub book_id: String,
    pub source_id: String,
    pub document_revision: Revision,
    pub renderer: String,
    pub renderer_version: String,
    pub fidelity: RenderFidelity,
    pub unit_ids: Vec<String>,
    pub profile: RenderProfile,
}

impl VisualJobSpec {
    pub fn from_renderer(
        id: impl Into<String>,
        book_id: impl Into<String>,
        source_id: impl Into<String>,
        document_revision: Revision,
        unit_ids: Vec<String>,
        profile: RenderProfile,
        renderer: &dyn VisualRenderer,
    ) -> Self {
        let descriptor = renderer.descriptor();
        Self {
            id: id.into(),
            book_id: book_id.into(),
            source_id: source_id.into(),
            document_revision,
            renderer: descriptor.renderer,
            renderer_version: descriptor.version,
            fidelity: descriptor.fidelity,
            unit_ids,
            profile,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(!self.id.trim().is_empty(), "视觉任务 ID 不能为空");
        ensure!(!self.book_id.trim().is_empty(), "视觉任务 book ID 不能为空");
        ensure!(
            !self.source_id.trim().is_empty(),
            "视觉任务 source ID 不能为空"
        );
        RendererDescriptor {
            renderer: self.renderer.clone(),
            version: self.renderer_version.clone(),
            fidelity: self.fidelity,
        }
        .validate()?;
        self.profile.validate()?;
        ensure!(
            self.unit_ids.iter().all(|id| !id.trim().is_empty()),
            "视觉任务包含空内容单元 ID"
        );
        let mut sorted = self.unit_ids.clone();
        sorted.sort();
        sorted.dedup();
        ensure!(
            sorted.len() == self.unit_ids.len(),
            "视觉任务不能重复选择内容单元"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualJobRecord {
    pub spec: VisualJobSpec,
    pub state: VisualJobState,
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub attempts: u32,
    pub completed_pages: usize,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
}

impl VisualJobRecord {
    pub fn queued(spec: VisualJobSpec, now: u64) -> Self {
        Self {
            spec,
            state: VisualJobState::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            completed_pages: 0,
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        }
    }
}

pub trait VisualJobStore: Send + Sync {
    /// Diagnostics follow this store's library identity. In-memory/custom
    /// stores opt in explicitly, so independent coordinators cannot mix logs.
    fn record_diagnostic(&self, _job_id: &str, _event: JobLogEvent, _metrics: JobLogMetrics) {}

    fn create(&self, record: &VisualJobRecord) -> Result<()>;
    fn get(&self, job_id: &str) -> Result<Option<VisualJobRecord>>;
    fn queued_ids(&self, limit: usize) -> Result<Vec<String>>;
    fn recover_interrupted(&self, now: u64) -> Result<usize>;
    fn request_pause(&self, job_id: &str, now: u64) -> Result<bool>;
    fn request_cancel(&self, job_id: &str, now: u64) -> Result<bool>;
    fn resume(&self, job_id: &str, now: u64) -> Result<bool>;
    fn retry(&self, job_id: &str, now: u64) -> Result<bool>;
    fn transition(
        &self,
        job_id: &str,
        state: VisualJobState,
        completed_pages: usize,
        error: Option<&str>,
        now: u64,
    ) -> Result<bool>;
}

#[derive(Debug, Default)]
pub struct MemoryVisualJobStore {
    jobs: Mutex<HashMap<String, VisualJobRecord>>,
}

impl VisualJobStore for MemoryVisualJobStore {
    fn create(&self, record: &VisualJobRecord) -> Result<()> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("视觉任务锁已损坏"))?;
        ensure!(!jobs.contains_key(&record.spec.id), "视觉任务 ID 已存在");
        jobs.insert(record.spec.id.clone(), record.clone());
        Ok(())
    }

    fn get(&self, job_id: &str) -> Result<Option<VisualJobRecord>> {
        Ok(self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("视觉任务锁已损坏"))?
            .get(job_id)
            .cloned())
    }

    fn queued_ids(&self, limit: usize) -> Result<Vec<String>> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("视觉任务锁已损坏"))?;
        let mut records = jobs
            .values()
            .filter(|record| record.state == VisualJobState::Queued)
            .collect::<Vec<_>>();
        records.sort_by_key(|record| (record.created_at, record.spec.id.as_str()));
        Ok(records
            .into_iter()
            .take(limit)
            .map(|record| record.spec.id.clone())
            .collect())
    }

    fn recover_interrupted(&self, now: u64) -> Result<usize> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("视觉任务锁已损坏"))?;
        let mut changed = 0;
        for record in jobs.values_mut() {
            if record.state == VisualJobState::Running {
                record.state = VisualJobState::Queued;
                record.updated_at = now;
                record.finished_at = None;
                record.error = Some("应用退出时视觉任务仍在执行，已恢复".to_string());
                changed += 1;
            }
        }
        Ok(changed)
    }

    fn request_pause(&self, job_id: &str, now: u64) -> Result<bool> {
        self.update(job_id, |record| {
            if matches!(
                record.state,
                VisualJobState::Queued | VisualJobState::Running
            ) && !record.cancel_requested
            {
                record.pause_requested = true;
                record.updated_at = now;
                true
            } else {
                false
            }
        })
    }

    fn request_cancel(&self, job_id: &str, now: u64) -> Result<bool> {
        self.update(job_id, |record| {
            if matches!(
                record.state,
                VisualJobState::Queued | VisualJobState::Running | VisualJobState::Paused
            ) {
                record.cancel_requested = true;
                record.pause_requested = false;
                record.updated_at = now;
                true
            } else {
                false
            }
        })
    }

    fn resume(&self, job_id: &str, now: u64) -> Result<bool> {
        self.update(job_id, |record| {
            if record.state == VisualJobState::Paused {
                record.state = VisualJobState::Queued;
                record.pause_requested = false;
                record.cancel_requested = false;
                record.error = None;
                record.finished_at = None;
                record.updated_at = now;
                true
            } else {
                false
            }
        })
    }

    fn retry(&self, job_id: &str, now: u64) -> Result<bool> {
        self.update(job_id, |record| {
            if matches!(
                record.state,
                VisualJobState::Failed | VisualJobState::Cancelled
            ) {
                record.state = VisualJobState::Queued;
                record.pause_requested = false;
                record.cancel_requested = false;
                record.completed_pages = 0;
                record.error = None;
                record.finished_at = None;
                record.updated_at = now;
                true
            } else {
                false
            }
        })
    }

    fn transition(
        &self,
        job_id: &str,
        state: VisualJobState,
        completed_pages: usize,
        error: Option<&str>,
        now: u64,
    ) -> Result<bool> {
        self.update(job_id, |record| {
            if state == VisualJobState::Running && record.state != VisualJobState::Running {
                record.attempts = record.attempts.saturating_add(1);
                record.started_at.get_or_insert(now);
            }
            record.state = state;
            record.completed_pages = completed_pages;
            record.error = error.map(str::to_string);
            record.updated_at = now;
            record.finished_at = state.is_terminal().then_some(now);
            if state == VisualJobState::Paused {
                record.pause_requested = false;
            }
            if state == VisualJobState::Cancelled {
                record.cancel_requested = false;
            }
            true
        })
    }
}

impl MemoryVisualJobStore {
    fn update(
        &self,
        job_id: &str,
        update: impl FnOnce(&mut VisualJobRecord) -> bool,
    ) -> Result<bool> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("视觉任务锁已损坏"))?;
        let Some(record) = jobs.get_mut(job_id) else {
            return Ok(false);
        };
        Ok(update(record))
    }
}

#[derive(Clone, Debug)]
pub struct SqliteVisualJobStore {
    database_path: PathBuf,
}

impl SqliteVisualJobStore {
    pub fn new(database_path: impl Into<PathBuf>) -> Self {
        Self {
            database_path: database_path.into(),
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn connection(&self) -> Result<rusqlite::Connection> {
        db::open_conn(&self.database_path)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedVisualCursor {
    schema_version: u32,
    spec: VisualJobSpec,
    completed_pages: usize,
}

impl PersistedVisualCursor {
    const SCHEMA_VERSION: u32 = 1;

    fn new(spec: VisualJobSpec, completed_pages: usize) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            spec,
            completed_pages,
        }
    }

    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).context("无法序列化视觉任务游标")
    }

    fn decode(value: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(value).context("无法解析视觉任务游标")?;
        ensure!(
            cursor.schema_version == Self::SCHEMA_VERSION,
            "视觉任务游标版本不兼容"
        );
        cursor.spec.validate()?;
        Ok(cursor)
    }
}

pub(crate) fn encode_persisted_visual_job(
    spec: VisualJobSpec,
    completed_pages: usize,
) -> Result<String> {
    PersistedVisualCursor::new(spec, completed_pages).encode()
}

pub(crate) fn decode_persisted_visual_job(value: &str) -> Result<(VisualJobSpec, usize)> {
    let cursor = PersistedVisualCursor::decode(value)?;
    Ok((cursor.spec, cursor.completed_pages))
}

impl VisualJobStore for SqliteVisualJobStore {
    fn record_diagnostic(&self, job_id: &str, event: JobLogEvent, metrics: JobLogMetrics) {
        record_for_database(&self.database_path, job_id, event, metrics);
    }

    fn create(&self, record: &VisualJobRecord) -> Result<()> {
        record.spec.validate()?;
        let cursor =
            PersistedVisualCursor::new(record.spec.clone(), record.completed_pages).encode()?;
        let job = db::index_jobs::IndexJob {
            id: record.spec.id.clone(),
            book_id: record.spec.book_id.clone(),
            source_id: Some(record.spec.source_id.clone()),
            kind: "visual_render".to_string(),
            status: to_db_state(record.state),
            pause_requested: record.pause_requested,
            cancel_requested: record.cancel_requested,
            attempts: record.attempts,
            cursor_json: cursor,
            error: record.error.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            started_at: record.started_at,
            finished_at: record.finished_at,
        };
        ensure!(
            db::index_jobs::insert(&self.connection()?, &job)? == 1,
            "视觉任务没有写入数据库"
        );
        Ok(())
    }

    fn get(&self, job_id: &str) -> Result<Option<VisualJobRecord>> {
        db::index_jobs::get(&self.connection()?, job_id)?
            .map(record_from_db)
            .transpose()
    }

    fn queued_ids(&self, limit: usize) -> Result<Vec<String>> {
        db::index_jobs::list_queued_ids_for_kind(&self.connection()?, "visual_render", limit)
    }

    fn recover_interrupted(&self, now: u64) -> Result<usize> {
        db::index_jobs::recover_interrupted_kind(&self.connection()?, "visual_render", now)
    }

    fn request_pause(&self, job_id: &str, now: u64) -> Result<bool> {
        Ok(db::index_jobs::request_pause(&self.connection()?, job_id, now)? == 1)
    }

    fn request_cancel(&self, job_id: &str, now: u64) -> Result<bool> {
        Ok(db::index_jobs::request_cancel(&self.connection()?, job_id, now)? == 1)
    }

    fn resume(&self, job_id: &str, now: u64) -> Result<bool> {
        Ok(db::index_jobs::resume(&self.connection()?, job_id, now)? == 1)
    }

    fn retry(&self, job_id: &str, now: u64) -> Result<bool> {
        let Some(record) = self.get(job_id)? else {
            return Ok(false);
        };
        if !matches!(
            record.state,
            VisualJobState::Failed | VisualJobState::Cancelled
        ) {
            return Ok(false);
        }
        let cursor = PersistedVisualCursor::new(record.spec, 0).encode()?;
        Ok(db::index_jobs::update_state(
            &self.connection()?,
            job_id,
            db::index_jobs::IndexJobStatus::Queued,
            &cursor,
            None,
            now,
            None,
            None,
        )? == 1)
    }

    fn transition(
        &self,
        job_id: &str,
        state: VisualJobState,
        completed_pages: usize,
        error: Option<&str>,
        now: u64,
    ) -> Result<bool> {
        let Some(record) = self.get(job_id)? else {
            return Ok(false);
        };
        let cursor = PersistedVisualCursor::new(record.spec, completed_pages).encode()?;
        let started_at = (state == VisualJobState::Running).then_some(now);
        let finished_at = state.is_terminal().then_some(now);
        Ok(db::index_jobs::update_state(
            &self.connection()?,
            job_id,
            to_db_state(state),
            &cursor,
            error,
            now,
            started_at,
            finished_at,
        )? == 1)
    }
}

impl SqliteVisualJobStore {
    pub(crate) fn reconcile_registered_renderers(
        &self,
        renderers: &[RendererDescriptor],
        now: u64,
    ) -> Result<db::transactions::VisualRenderReconciliation> {
        db::transactions::reconcile_visual_render_jobs(
            &mut self.connection()?,
            renderers,
            &RenderProfile::default(),
            now,
        )
    }
}

fn record_from_db(job: db::index_jobs::IndexJob) -> Result<VisualJobRecord> {
    ensure!(job.kind == "visual_render", "数据库任务不是视觉渲染任务");
    let cursor = PersistedVisualCursor::decode(&job.cursor_json)?;
    ensure!(cursor.spec.id == job.id, "视觉任务游标 ID 与数据库不一致");
    ensure!(
        cursor.spec.book_id == job.book_id,
        "视觉任务 book ID 与数据库不一致"
    );
    ensure!(
        job.source_id.as_deref() == Some(cursor.spec.source_id.as_str()),
        "视觉任务 source ID 与数据库不一致"
    );
    Ok(VisualJobRecord {
        spec: cursor.spec,
        state: from_db_state(job.status),
        pause_requested: job.pause_requested,
        cancel_requested: job.cancel_requested,
        attempts: job.attempts,
        completed_pages: cursor.completed_pages,
        error: job.error,
        created_at: job.created_at,
        updated_at: job.updated_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
    })
}

fn to_db_state(state: VisualJobState) -> db::index_jobs::IndexJobStatus {
    match state {
        VisualJobState::Queued => db::index_jobs::IndexJobStatus::Queued,
        VisualJobState::Running => db::index_jobs::IndexJobStatus::Running,
        VisualJobState::Paused => db::index_jobs::IndexJobStatus::Paused,
        VisualJobState::Succeeded => db::index_jobs::IndexJobStatus::Succeeded,
        VisualJobState::Failed => db::index_jobs::IndexJobStatus::Failed,
        VisualJobState::Cancelled => db::index_jobs::IndexJobStatus::Cancelled,
    }
}

fn from_db_state(state: db::index_jobs::IndexJobStatus) -> VisualJobState {
    match state {
        db::index_jobs::IndexJobStatus::Queued => VisualJobState::Queued,
        db::index_jobs::IndexJobStatus::Running => VisualJobState::Running,
        db::index_jobs::IndexJobStatus::Paused => VisualJobState::Paused,
        db::index_jobs::IndexJobStatus::Succeeded => VisualJobState::Succeeded,
        db::index_jobs::IndexJobStatus::Failed => VisualJobState::Failed,
        db::index_jobs::IndexJobStatus::Cancelled => VisualJobState::Cancelled,
    }
}

pub trait VisualDocumentSource: Send + Sync {
    fn load_document(&self, book_id: String, revision: Revision)
    -> PreviewFuture<'_, BookDocument>;

    fn load_asset(
        &self,
        _book_id: String,
        _asset_id: String,
    ) -> PreviewFuture<'_, VisualAssetPayload> {
        Box::pin(async { anyhow::bail!("视觉图片来源未实现") })
    }

    /// Loads the immutable bytes backing one exact persisted source revision.
    /// Implementations must verify book ownership, source ID and revision
    /// before returning data.
    fn load_source(
        &self,
        _book_id: String,
        _source_id: String,
        _revision: Revision,
    ) -> PreviewFuture<'_, VisualSourcePayload> {
        Box::pin(async { anyhow::bail!("视觉原始文档来源未实现") })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualAssetPayload {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualSourcePayload {
    pub format: String,
    pub source_kind: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

pub trait VisualPageSink: Send + Sync {
    /// Discards a failed/cancelled job's resumable prefix before retrying it
    /// from page zero. Implementations also reclaim newly unreferenced objects.
    fn reset_staging(&self, spec: VisualJobSpec) -> PreviewFuture<'_, ()>;

    /// Writes one immutable object first, then atomically publishes its staging
    /// reference together with `completed_pages` in the durable job cursor.
    fn checkpoint_page(
        &self,
        spec: VisualJobSpec,
        page: RenderedVisualPage,
        completed_pages: usize,
    ) -> PreviewFuture<'_, ()>;

    /// Atomically replaces the visible page set from a complete contiguous
    /// staging prefix, clears that prefix, and marks the durable job succeeded.
    fn commit_pages(&self, spec: VisualJobSpec, total_pages: usize) -> PreviewFuture<'_, ()>;
}

struct CoordinatorCore {
    store: Arc<dyn VisualJobStore>,
    source: Arc<dyn VisualDocumentSource>,
    sink: Arc<dyn VisualPageSink>,
    renderers: HashMap<String, Arc<dyn VisualRenderer>>,
    controls: Mutex<HashMap<String, RenderControl>>,
}

fn record_visual_event(
    core: &CoordinatorCore,
    job_id: &str,
    event: JobLogEvent,
    mut metrics: JobLogMetrics,
) {
    if let Ok((run_id, _, attempt)) = VISUAL_LOG_RUN.try_with(|context| *context) {
        metrics.run_id = Some(run_id);
        metrics.attempt.get_or_insert(u64::from(attempt));
    }
    core.store.record_diagnostic(job_id, event, metrics);
}

pub struct VisualJobCoordinator {
    core: Arc<CoordinatorCore>,
    sender: async_channel::Sender<String>,
    runtime: Handle,
    worker: JoinHandle<()>,
}

impl fmt::Debug for VisualJobCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VisualJobCoordinator")
            .field("renderer_count", &self.core.renderers.len())
            .finish_non_exhaustive()
    }
}

impl VisualJobCoordinator {
    pub fn new(
        runtime: Handle,
        store: Arc<dyn VisualJobStore>,
        source: Arc<dyn VisualDocumentSource>,
        sink: Arc<dyn VisualPageSink>,
        renderers: Vec<Arc<dyn VisualRenderer>>,
    ) -> Result<Self> {
        ensure!(!renderers.is_empty(), "至少需要一个视觉渲染器");
        let mut renderer_map = HashMap::new();
        for renderer in renderers {
            let descriptor = renderer.descriptor();
            descriptor.validate()?;
            ensure!(
                renderer_map
                    .insert(descriptor.renderer.clone(), renderer)
                    .is_none(),
                "视觉渲染器名称重复：{}",
                descriptor.renderer
            );
        }
        let core = Arc::new(CoordinatorCore {
            store,
            source,
            sink,
            renderers: renderer_map,
            controls: Mutex::new(HashMap::new()),
        });
        let now = unix_timestamp()?;
        core.store.recover_interrupted(now)?;
        let (sender, receiver) = async_channel::unbounded::<String>();
        let worker_core = Arc::clone(&core);
        let worker = runtime.spawn(async move {
            loop {
                let direct_job = tokio::select! {
                    received = receiver.recv() => match received {
                        Ok(job_id) => Some(job_id),
                        Err(_) => break,
                    },
                    _ = tokio::time::sleep(PERSISTED_VISUAL_SCAN_INTERVAL) => None,
                };
                if let Some(job_id) = direct_job
                    && let Err(error) = process_visual_job(Arc::clone(&worker_core), &job_id).await
                {
                    tracing::error!(job_id, %error, "视觉后台任务调度失败");
                }

                let store = Arc::clone(&worker_core.store);
                let queued = match tokio::task::spawn_blocking(move || {
                    store.queued_ids(PERSISTED_VISUAL_SCAN_LIMIT)
                })
                .await
                {
                    Ok(Ok(queued)) => queued,
                    Ok(Err(error)) => {
                        tracing::error!(%error, "扫描持久化视觉任务失败");
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(%error, "视觉任务扫描线程异常退出");
                        continue;
                    }
                };
                for job_id in queued {
                    if let Err(error) = process_visual_job(Arc::clone(&worker_core), &job_id).await
                    {
                        tracing::error!(job_id, %error, "持久化视觉任务调度失败");
                    }
                }
            }
        });
        Ok(Self {
            core,
            sender,
            runtime,
            worker,
        })
    }

    pub async fn submit(&self, spec: VisualJobSpec) -> Result<()> {
        spec.validate()?;
        let descriptor = self
            .core
            .renderers
            .get(&spec.renderer)
            .with_context(|| format!("找不到视觉渲染器 {}", spec.renderer))?
            .descriptor();
        ensure!(
            descriptor.version == spec.renderer_version && descriptor.fidelity == spec.fidelity,
            "视觉任务使用的 renderer 版本或 fidelity 与当前实现不一致"
        );
        let record = VisualJobRecord::queued(spec.clone(), unix_timestamp()?);
        let store = Arc::clone(&self.core.store);
        self.runtime
            .spawn_blocking(move || store.create(&record))
            .await
            .context("视觉任务持久化线程异常退出")??;
        record_visual_event(
            &self.core,
            &spec.id,
            JobLogEvent::Queued,
            JobLogMetrics::default(),
        );
        self.sender
            .send(spec.id)
            .await
            .context("视觉任务队列已经关闭")
    }

    pub async fn status(&self, job_id: &str) -> Result<Option<VisualJobRecord>> {
        let store = Arc::clone(&self.core.store);
        let job_id = job_id.to_string();
        self.runtime
            .spawn_blocking(move || store.get(&job_id))
            .await
            .context("视觉任务查询线程异常退出")?
    }

    pub async fn pause(&self, job_id: &str) -> Result<bool> {
        let changed = self.store_control(job_id, true).await?;
        if changed
            && let Some(control) = self
                .core
                .controls
                .lock()
                .map_err(|_| anyhow::anyhow!("视觉任务控制锁已损坏"))?
                .get(job_id)
                .cloned()
        {
            control.request_pause();
        }
        Ok(changed)
    }

    pub async fn cancel(&self, job_id: &str) -> Result<bool> {
        let changed = self.store_control(job_id, false).await?;
        if changed {
            let control = self
                .core
                .controls
                .lock()
                .map_err(|_| anyhow::anyhow!("视觉任务控制锁已损坏"))?
                .get(job_id)
                .cloned();
            if let Some(control) = control {
                control.request_cancel();
            } else if let Some(record) = self.status(job_id).await?
                && record.state == VisualJobState::Paused
            {
                blocking_transition(
                    Arc::clone(&self.core.store),
                    job_id,
                    VisualJobState::Cancelled,
                    record.completed_pages,
                    None,
                )
                .await?;
            }
        }
        Ok(changed)
    }

    async fn store_control(&self, job_id: &str, pause: bool) -> Result<bool> {
        let store = Arc::clone(&self.core.store);
        let diagnostic_id = job_id.to_string();
        let job_id = job_id.to_string();
        let now = unix_timestamp()?;
        let changed = self
            .runtime
            .spawn_blocking(move || {
                if pause {
                    store.request_pause(&job_id, now)
                } else {
                    store.request_cancel(&job_id, now)
                }
            })
            .await
            .context("视觉任务控制线程异常退出")??;
        if changed {
            record_visual_event(
                &self.core,
                &diagnostic_id,
                if pause {
                    JobLogEvent::PauseRequested
                } else {
                    JobLogEvent::CancelRequested
                },
                JobLogMetrics::default(),
            );
        }
        Ok(changed)
    }

    pub async fn resume(&self, job_id: &str) -> Result<bool> {
        self.requeue(job_id, false).await
    }

    pub async fn retry(&self, job_id: &str) -> Result<bool> {
        self.requeue(job_id, true).await
    }

    async fn requeue(&self, job_id: &str, retry: bool) -> Result<bool> {
        if retry {
            let Some(record) = self.status(job_id).await? else {
                return Ok(false);
            };
            if !matches!(
                record.state,
                VisualJobState::Failed | VisualJobState::Cancelled
            ) {
                return Ok(false);
            }
            self.core.sink.reset_staging(record.spec.clone()).await?;
        }
        let store = Arc::clone(&self.core.store);
        let owned_id = job_id.to_string();
        let now = unix_timestamp()?;
        let changed = self
            .runtime
            .spawn_blocking(move || {
                if retry {
                    store.retry(&owned_id, now)
                } else {
                    store.resume(&owned_id, now)
                }
            })
            .await
            .context("视觉任务重新入队线程异常退出")??;
        if changed {
            record_visual_event(
                &self.core,
                job_id,
                if retry {
                    JobLogEvent::RetryRequested
                } else {
                    JobLogEvent::ResumeRequested
                },
                JobLogMetrics::default(),
            );
            self.sender
                .send(job_id.to_string())
                .await
                .context("视觉任务队列已经关闭")?;
        }
        Ok(changed)
    }

    /// Wakes the worker for an exact job already committed by a transaction.
    /// The persisted-job scanner can win the race and start or finish that job
    /// before this notification. Sending its ID again is harmless: the worker
    /// only executes queued records. Missing/replaced/stopped jobs and a closed
    /// worker queue remain errors; this never creates, resets or retries a job.
    pub(crate) async fn schedule_committed(&self, expected: &VisualJobSpec) -> Result<()> {
        expected.validate()?;
        let record = self
            .status(&expected.id)
            .await?
            .with_context(|| format!("已提交的视觉任务不存在：{}", expected.id))?;
        ensure!(
            record.spec == *expected,
            "视觉任务与本次提交的身份不一致：{}",
            expected.id
        );
        ensure!(
            !record.pause_requested && !record.cancel_requested,
            "已提交的视觉任务已请求暂停或取消：{}",
            expected.id
        );
        ensure!(
            matches!(
                record.state,
                VisualJobState::Queued | VisualJobState::Running | VisualJobState::Succeeded
            ),
            "已提交的视觉任务无法调度：{}，状态 {:?}，原因 {}",
            expected.id,
            record.state,
            record.error.as_deref().unwrap_or("无")
        );
        self.sender
            .send(expected.id.clone())
            .await
            .context("视觉任务队列已经关闭")
    }

    /// Requeues a persisted queued job after application restart. The complete
    /// rendering spec lives in `cursor_json`, so no in-memory document snapshot
    /// is required.
    pub async fn recover(&self, job_id: &str) -> Result<bool> {
        let Some(record) = self.status(job_id).await? else {
            return Ok(false);
        };
        if record.state != VisualJobState::Queued {
            return Ok(false);
        }
        self.sender
            .send(job_id.to_string())
            .await
            .context("视觉任务队列已经关闭")?;
        record_visual_event(
            &self.core,
            job_id,
            JobLogEvent::Recovered,
            JobLogMetrics::default(),
        );
        Ok(true)
    }

    pub async fn wait_for_state(
        &self,
        job_id: &str,
        expected: VisualJobState,
        timeout: Duration,
    ) -> Result<VisualJobRecord> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let record = self
                .status(job_id)
                .await?
                .with_context(|| format!("视觉任务不存在：{job_id}"))?;
            if record.state == expected {
                return Ok(record);
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "等待视觉任务 {job_id} 进入 {expected:?} 超时；当前状态为 {:?}",
                record.state
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for VisualJobCoordinator {
    fn drop(&mut self) {
        self.sender.close();
        self.worker.abort();
    }
}

async fn process_visual_job(core: Arc<CoordinatorCore>, job_id: &str) -> Result<()> {
    let Some(record) = blocking_get(Arc::clone(&core.store), job_id).await? else {
        return Ok(());
    };
    if record.state != VisualJobState::Queued {
        return Ok(());
    }
    if record.cancel_requested {
        blocking_transition(
            Arc::clone(&core.store),
            job_id,
            VisualJobState::Cancelled,
            record.completed_pages,
            None,
        )
        .await?;
        record_visual_event(
            &core,
            job_id,
            JobLogEvent::Cancelled,
            JobLogMetrics::default(),
        );
        return Ok(());
    }
    if record.pause_requested {
        blocking_transition(
            Arc::clone(&core.store),
            job_id,
            VisualJobState::Paused,
            record.completed_pages,
            None,
        )
        .await?;
        record_visual_event(&core, job_id, JobLogEvent::Paused, JobLogMetrics::default());
        return Ok(());
    }

    blocking_transition(
        Arc::clone(&core.store),
        job_id,
        VisualJobState::Running,
        record.completed_pages,
        None,
    )
    .await?;
    let run_id = NEXT_VISUAL_LOG_RUN_ID.fetch_add(1, Ordering::Relaxed);
    VISUAL_LOG_RUN
        .scope(
            (run_id, Instant::now(), record.attempts.saturating_add(1)),
            async {
                record_visual_event(
                    &core,
                    job_id,
                    JobLogEvent::RunStarted,
                    JobLogMetrics {
                        ordinal: Some(record.completed_pages as u64),
                        ..JobLogMetrics::default()
                    },
                );
                let control = RenderControl::default();
                core.controls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("视觉任务控制锁已损坏"))?
                    .insert(job_id.to_string(), control.clone());

                // A pause/cancel request can land after the queued-state read but before
                // the in-memory control is published. Re-read persisted flags once the
                // control is visible so that narrow race cannot lose an accepted request.
                if let Some(latest) = blocking_get(Arc::clone(&core.store), job_id).await? {
                    if latest.cancel_requested {
                        control.request_cancel();
                    } else if latest.pause_requested {
                        control.request_pause();
                    }
                }

                let result =
                    execute_visual_job(&core, &record.spec, record.completed_pages, control).await;
                core.controls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("视觉任务控制锁已损坏"))?
                    .remove(job_id);

                match result {
                    // `commit_pages` is the single success commit point: production sinks
                    // publish final pages, clear staging, and mark the job succeeded in one
                    // SQLite transaction. A second state write here could turn a committed
                    // render into a reported failure.
                    Ok(page_count) => {
                        record_visual_event(
                            &core,
                            job_id,
                            JobLogEvent::RunSucceeded,
                            JobLogMetrics {
                                actual_count: Some(page_count as u64),
                                duration_ms: VISUAL_LOG_RUN
                                    .try_with(|(_, started, _)| {
                                        started.elapsed().as_millis() as u64
                                    })
                                    .ok(),
                                ..JobLogMetrics::default()
                            },
                        );
                    }
                    Err(error) => {
                        let state = match error.downcast_ref::<RenderInterrupted>() {
                            Some(RenderInterrupted::Paused) => VisualJobState::Paused,
                            Some(RenderInterrupted::Cancelled) => VisualJobState::Cancelled,
                            None => VisualJobState::Failed,
                        };
                        let detail = truncate_error(&format!("{error:#}"));
                        let completed_pages = blocking_get(Arc::clone(&core.store), job_id)
                            .await?
                            .map(|latest| latest.completed_pages)
                            .unwrap_or(record.completed_pages);
                        blocking_transition(
                            Arc::clone(&core.store),
                            job_id,
                            state,
                            completed_pages,
                            (state == VisualJobState::Failed).then_some(detail.as_str()),
                        )
                        .await?;
                        record_visual_event(
                            &core,
                            job_id,
                            match state {
                                VisualJobState::Paused => JobLogEvent::Paused,
                                VisualJobState::Cancelled => JobLogEvent::Cancelled,
                                _ => JobLogEvent::RunFailed,
                            },
                            JobLogMetrics {
                                ordinal: Some(completed_pages as u64),
                                duration_ms: VISUAL_LOG_RUN
                                    .try_with(|(_, started, _)| {
                                        started.elapsed().as_millis() as u64
                                    })
                                    .ok(),
                                error_kind: (state == VisualJobState::Failed)
                                    .then(|| classify_error(&error)),
                                ..JobLogMetrics::for_error(&error)
                            },
                        );
                    }
                }
                Ok(())
            },
        )
        .await
}

async fn execute_visual_job(
    core: &CoordinatorCore,
    spec: &VisualJobSpec,
    completed_pages: usize,
    control: RenderControl,
) -> Result<usize> {
    let renderer = core
        .renderers
        .get(&spec.renderer)
        .with_context(|| format!("找不到视觉渲染器 {}", spec.renderer))?;
    let descriptor = renderer.descriptor();
    ensure!(
        descriptor.version == spec.renderer_version && descriptor.fidelity == spec.fidelity,
        "renderer 已变化；请重建视觉任务"
    );
    control.checkpoint().map_err(anyhow::Error::new)?;
    let document = core
        .source
        .load_document(spec.book_id.clone(), spec.document_revision)
        .await?;
    ensure!(document.id == spec.book_id, "视觉任务加载了错误的图书 ID");
    ensure!(
        document.revision == spec.document_revision,
        "视觉任务 revision 已过期；请重新投递"
    );
    record_visual_event(
        core,
        &spec.id,
        JobLogEvent::SourceLoaded,
        JobLogMetrics {
            ordinal: Some(completed_pages as u64),
            expected_count: Some(spec.unit_ids.len() as u64),
            ..JobLogMetrics::default()
        },
    );
    let (emitter, receiver) = RenderPageEmitter::bounded(completed_pages, 1);
    let render = renderer.render_units_resumable(
        RenderUnitsRequest {
            document: Arc::new(document),
            source_id: spec.source_id.clone(),
            unit_ids: spec.unit_ids.clone(),
            profile: spec.profile.clone(),
            asset_source: Some(Arc::clone(&core.source)),
        },
        control.clone(),
        emitter,
    );
    tokio::pin!(render);
    let mut render_result = None;
    let mut next_page = completed_pages;
    let mut page_started = Instant::now();
    let mut control_poll = tokio::time::interval(Duration::from_millis(10));
    control_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let received = if render_result.is_some() {
            tokio::select! {
                _ = control_poll.tick() => {
                    control.checkpoint().map_err(anyhow::Error::new)?;
                    continue;
                }
                received = receiver.recv() => received,
            }
        } else {
            tokio::select! {
                _ = control_poll.tick() => {
                    control.checkpoint().map_err(anyhow::Error::new)?;
                    continue;
                }
                result = &mut render => {
                    render_result = Some(result);
                    continue;
                }
                received = receiver.recv() => received,
            }
        };
        let Ok(page) = received else {
            break;
        };
        record_visual_event(
            core,
            &spec.id,
            JobLogEvent::ItemStarted,
            JobLogMetrics {
                ordinal: Some(next_page as u64),
                ..JobLogMetrics::default()
            },
        );
        validate_rendered_page(spec, &page, next_page)?;
        let checkpoint = next_page.checked_add(1).context("视觉页面断点数量溢出")?;
        let response_bytes = page.bytes.len() as u64;
        core.sink
            .checkpoint_page(spec.clone(), page, checkpoint)
            .await?;
        record_visual_event(
            core,
            &spec.id,
            JobLogEvent::ItemSaved,
            JobLogMetrics {
                ordinal: Some(next_page as u64),
                response_bytes: Some(response_bytes),
                duration_ms: Some(page_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );
        next_page = checkpoint;
        page_started = Instant::now();
        control.checkpoint().map_err(anyhow::Error::new)?;
    }
    let page_count = match render_result {
        Some(result) => result?,
        None => render.await?,
    };
    ensure!(page_count == next_page, "renderer 页面总数与持久断点不一致");
    control.checkpoint().map_err(anyhow::Error::new)?;
    record_visual_event(
        core,
        &spec.id,
        JobLogEvent::PublicationStarted,
        JobLogMetrics {
            actual_count: Some(page_count as u64),
            ..JobLogMetrics::default()
        },
    );
    core.sink.commit_pages(spec.clone(), page_count).await?;
    Ok(page_count)
}

fn validate_rendered_page(
    spec: &VisualJobSpec,
    page: &RenderedVisualPage,
    expected_index: usize,
) -> Result<()> {
    let profile_id = spec.profile.stable_id();
    ensure!(page.page_index == expected_index, "视觉页面序号不连续");
    ensure!(page.width > 0 && page.height > 0, "视觉页面尺寸无效");
    ensure!(!page.bytes.is_empty(), "视觉页面内容为空");
    page.locator.validate().context("视觉页面 locator 无效")?;
    ensure!(
        page.locator.book_id == spec.book_id,
        "视觉页面 locator 越权"
    );
    match page.content_unit_id.as_deref() {
        Some(unit_id) => ensure!(
            page.locator.unit_id == unit_id,
            "视觉页面 locator 与内容单元不匹配"
        ),
        None => ensure!(
            matches!(
                page.locator.source.as_ref(),
                Some(SourceLocator::OfficeRenderedPage { .. })
            ) && page.locator.block_id.is_none()
                && page.locator.text_range.is_none()
                && page.locator.region.is_none()
                && page.locator.unit_id == office_preview_unit_id(&spec.book_id, &spec.source_id)
                && page.metadata.fidelity == RenderFidelity::OfficeEnhanced
                && page.metadata.unit_revision == Revision::INITIAL,
            "无内容单元的视觉页面必须是 preview-only Office 页面"
        ),
    }
    ensure!(
        page.metadata.renderer == spec.renderer
            && page.metadata.renderer_version == spec.renderer_version
            && page.metadata.fidelity == spec.fidelity
            && page.metadata.document_revision == spec.document_revision
            && page.metadata.profile_id == profile_id,
        "视觉页面渲染元数据与任务不一致"
    );
    Ok(())
}

async fn blocking_get(
    store: Arc<dyn VisualJobStore>,
    job_id: &str,
) -> Result<Option<VisualJobRecord>> {
    let job_id = job_id.to_string();
    tokio::task::spawn_blocking(move || store.get(&job_id))
        .await
        .context("视觉任务查询线程异常退出")?
}

async fn blocking_transition(
    store: Arc<dyn VisualJobStore>,
    job_id: &str,
    state: VisualJobState,
    completed_pages: usize,
    error: Option<&str>,
) -> Result<()> {
    let job_id = job_id.to_string();
    let error = error.map(str::to_string);
    let now = unix_timestamp()?;
    let changed = tokio::task::spawn_blocking(move || {
        store.transition(&job_id, state, completed_pages, error.as_deref(), now)
    })
    .await
    .context("视觉任务状态更新线程异常退出")??;
    ensure!(changed, "视觉任务状态更新失败：任务不存在");
    Ok(())
}

fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix epoch")
        .map(|duration| duration.as_secs())
}

fn truncate_error(error: &str) -> String {
    const MAX_ERROR_CHARS: usize = 4_096;
    if error.chars().count() <= MAX_ERROR_CHARS {
        error.to_string()
    } else {
        error.chars().take(MAX_ERROR_CHARS).collect()
    }
}

/// Returns a checked-in PDF.js asset and MIME type. Exact route matching keeps
/// encoded traversal and host filesystem paths outside the WebView protocol.
pub fn bundled_pdfjs_asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
        || path.chars().any(char::is_control)
    {
        return None;
    }
    pdfjs_routes::get(path)
}

#[cfg(test)]
mod tests {
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_renderer_publishes_deterministic_png_and_is_interruptible() {
        let renderer = StructuralPngRenderer;
        let request = RenderUnitsRequest {
            document: Arc::new(sample_document()),
            source_id: "source-test".to_string(),
            unit_ids: Vec::new(),
            profile: RenderProfile::default(),
            asset_source: None,
        };
        let first = renderer
            .render_units(request.clone(), RenderControl::default())
            .await
            .expect("render PNG");
        let second = renderer
            .render_units(request, RenderControl::default())
            .await
            .expect("repeat PNG");
        assert_eq!(first, second);
        assert_eq!(first[0].media_type, "image/png");
        assert!(first[0].bytes.starts_with(PNG_SIGNATURE));
        let decoded = image::load_from_memory_with_format(&first[0].bytes, image::ImageFormat::Png)
            .expect("valid structural PNG");
        assert_eq!(decoded.width(), first[0].width);
        assert_eq!(decoded.height(), first[0].height);
        let svg = render_svg_page(
            &["包含 <script>、& 和中文。".to_string()],
            &RenderProfile::default(),
        );
        assert!(svg.contains("&lt;script&gt;"));
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&amp;"));
        assert!(svg.contains("中文"));
        assert_eq!(first[0].metadata.document_revision, Revision::new(3));

        let control = RenderControl::default();
        control.request_pause();
        let error = renderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(sample_document()),
                    source_id: "source-test".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile::default(),
                    asset_source: None,
                },
                control,
            )
            .await
            .expect_err("paused render must stop");
        assert_eq!(
            error.downcast_ref::<RenderInterrupted>(),
            Some(&RenderInterrupted::Paused)
        );
    }

    #[tokio::test]
    async fn structural_renderer_adds_owned_image_pages_with_block_locators() {
        let bytes = png_fixture(2, 3);
        let asset = AssetRef {
            id: "asset-image".to_string(),
            roles: vec![AssetRole::ContentImage],
            media_type: "image/png".to_string(),
            original_file_name: Some("picture.png".to_string()),
            byte_len: bytes.len() as u64,
            content_hash: blake3::hash(&bytes).to_hex().to_string(),
        };
        let mut document = BookDocument::created("book-image", "图片视觉测试");
        document.assets.push(asset);
        document.units.push(ContentUnit::new(
            "unit-image",
            ContentUnitKind::Chapter,
            "图片章节",
            "<img src=\"moye-asset:asset-image\" alt=\"说明\">",
            BlockDocument::new(vec![Block::Image {
                id: "block-image".to_string(),
                asset_id: "asset-image".to_string(),
                alt: "说明".to_string(),
                title: None,
                caption: Vec::new(),
            }]),
        ));
        document.validate().expect("valid image document");
        let source: Arc<dyn VisualDocumentSource> = Arc::new(ImageDocumentSource {
            document: document.clone(),
            payload: VisualAssetPayload {
                media_type: "image/png".to_string(),
                bytes: bytes.clone(),
            },
        });

        let pages = StructuralPngRenderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document),
                    source_id: "source-image".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile::default(),
                    asset_source: Some(source),
                },
                RenderControl::default(),
            )
            .await
            .expect("render text and image pages");

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].media_type, "image/png");
        assert_eq!(pages[1].bytes, bytes);
        assert_eq!(
            pages[1].locator,
            DocumentLocator::block("book-image", "unit-image", "block-image")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_slide_merges_text_and_images_into_exactly_one_page() {
        let bytes = png_fixture(40, 30);
        let asset = AssetRef {
            id: "asset-image".to_string(),
            roles: vec![AssetRole::ContentImage],
            media_type: "image/png".to_string(),
            original_file_name: Some("slide.png".to_string()),
            byte_len: bytes.len() as u64,
            content_hash: blake3::hash(&bytes).to_hex().to_string(),
        };
        let mut document = BookDocument::created("book-slide", "幻灯片视觉测试");
        document.assets.push(asset);
        document.units.push(
            ContentUnit::new(
                "unit-slide",
                ContentUnitKind::Slide,
                "第三张幻灯片",
                "slide source",
                BlockDocument::new(vec![
                    Block::paragraph("block-slide-text", "一段需要和图片位于同一页的文字。"),
                    Block::Image {
                        id: "block-image".to_string(),
                        asset_id: "asset-image".to_string(),
                        alt: "示意图".to_string(),
                        title: None,
                        caption: Vec::new(),
                    },
                ]),
            )
            .with_source_locator(SourceLocator::slide(3)),
        );
        document.validate().expect("valid slide document");
        let source: Arc<dyn VisualDocumentSource> = Arc::new(ImageDocumentSource {
            document: document.clone(),
            payload: VisualAssetPayload {
                media_type: "image/png".to_string(),
                bytes: bytes.clone(),
            },
        });

        let pages = StructuralPngRenderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document.clone()),
                    source_id: "source-slide".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile {
                        viewport_width: 640,
                        viewport_height: 480,
                        ..RenderProfile::default()
                    },
                    asset_source: Some(source),
                },
                RenderControl::default(),
            )
            .await
            .expect("render one composite slide");

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].media_type, "image/png");
        assert_ne!(pages[0].bytes, bytes, "the page must be a composite PNG");
        assert_eq!(pages[0].locator.source, Some(SourceLocator::slide(3)));
        assert_eq!(
            pages[0].locator.block_id.as_deref(),
            Some("block-slide-text")
        );
        document
            .validate_locator(&pages[0].locator)
            .expect("composite slide locator is valid");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_worksheet_uses_two_dimensional_a1_page_ranges() {
        let header = TableRow::new(
            (0..7)
                .map(|column| TableCell::text(format!("H{column}")))
                .collect(),
        );
        let rows = (1..6)
            .map(|row| {
                TableRow::new(
                    (0..7)
                        .map(|column| TableCell::text(format!("R{row}C{column}")))
                        .collect(),
                )
            })
            .collect();
        let mut document = BookDocument::created("book-sheet", "工作表视觉测试");
        document.units.push(
            ContentUnit::new(
                "unit-sheet",
                ContentUnitKind::Worksheet,
                "Sales",
                "table source",
                BlockDocument::new(vec![Block::Table {
                    id: "block-table".to_string(),
                    header: Some(header),
                    rows,
                }]),
            )
            .with_source_locator(SourceLocator::worksheet(
                "Sales",
                Some("C5:I10".to_string()),
            )),
        );
        document.validate().expect("valid worksheet document");

        let pages = StructuralPngRenderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document.clone()),
                    source_id: "source-sheet".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile {
                        viewport_width: 500,
                        viewport_height: 400,
                        ..RenderProfile::default()
                    },
                    asset_source: None,
                },
                RenderControl::default(),
            )
            .await
            .expect("render worksheet tiles");

        let ranges = pages
            .iter()
            .map(|page| match page.locator.source.as_ref() {
                Some(SourceLocator::Worksheet {
                    name,
                    range: Some(range),
                }) => {
                    assert_eq!(name, "Sales");
                    range.as_str()
                }
                other => panic!("unexpected worksheet locator: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(ranges, ["C5:D10", "E5:F10", "G5:H10", "I5:I10"]);
        assert!(pages.iter().all(|page| {
            page.locator.block_id.as_deref() == Some("block-table")
                && page.locator.text_range.is_none()
                && page.bytes.starts_with(PNG_SIGNATURE)
        }));
        for page in &pages {
            document
                .validate_locator(&page.locator)
                .expect("worksheet tile locator is valid");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_text_pages_retain_block_utf8_ranges() {
        let text = "中文段落".repeat(160);
        let mut document = BookDocument::created("book-ranges", "文本定位测试");
        document.units.push(
            ContentUnit::new(
                "unit-ranges",
                ContentUnitKind::Chapter,
                "章节",
                text.clone(),
                BlockDocument::new(vec![Block::paragraph("block-ranges", text)]),
            )
            .with_source_locator(SourceLocator::created()),
        );
        document.validate().expect("valid ranged document");

        let pages = StructuralPngRenderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document.clone()),
                    source_id: "source-ranges".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile {
                        viewport_width: 320,
                        viewport_height: 320,
                        ..RenderProfile::default()
                    },
                    asset_source: None,
                },
                RenderControl::default(),
            )
            .await
            .expect("render ranged text pages");

        assert!(pages.len() > 1);
        let mut previous_end = 0;
        for page in &pages {
            assert_eq!(page.locator.block_id.as_deref(), Some("block-ranges"));
            assert_eq!(page.locator.source, Some(SourceLocator::created()));
            let range = page.locator.text_range.expect("page text range");
            assert!(range.end_byte > range.start_byte);
            assert!(range.start_byte >= previous_end);
            previous_end = range.end_byte;
            document
                .validate_locator(&page.locator)
                .expect("text page locator is valid");
        }
    }

    #[test]
    fn supported_raster_image_pages_keep_their_original_bytes() {
        let rgba = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            3,
            image::Rgba([10, 20, 30, 255]),
        ));
        let rgb = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            3,
            image::Rgb([10, 20, 30]),
        ));
        for (media_type, format, image) in [
            ("image/png", image::ImageFormat::Png, &rgba),
            ("image/jpeg", image::ImageFormat::Jpeg, &rgb),
            ("image/webp", image::ImageFormat::WebP, &rgba),
            ("image/gif", image::ImageFormat::Gif, &rgba),
        ] {
            let mut source = Cursor::new(Vec::new());
            image
                .write_to(&mut source, format)
                .expect("encode raster fixture");
            let source = source.into_inner();
            let prepared = prepare_visual_image(
                VisualAssetPayload {
                    media_type: media_type.to_string(),
                    bytes: source.clone(),
                },
                media_type,
                &RenderProfile::default(),
                &RenderControl::default(),
            )
            .expect("validate raster page");
            assert_eq!(prepared.media_type, media_type);
            assert_eq!(prepared.bytes, source);
            assert_eq!((prepared.width, prepared.height), (2, 3));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_renderer_rasterizes_svg_assets_before_publication() {
        let bytes = r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32" fill="white"/><text x="2" y="20">中文 &amp; safe</text></svg>"#
            .as_bytes()
            .to_vec();
        let asset = AssetRef {
            id: "asset-image".to_string(),
            roles: vec![AssetRole::ContentImage],
            media_type: "image/svg+xml".to_string(),
            original_file_name: Some("picture.svg".to_string()),
            byte_len: bytes.len() as u64,
            content_hash: blake3::hash(&bytes).to_hex().to_string(),
        };
        let mut document = BookDocument::created("book-image", "SVG 图片视觉测试");
        document.assets.push(asset);
        document.units.push(ContentUnit::new(
            "unit-image",
            ContentUnitKind::Chapter,
            "SVG 图片章节",
            "<img src=\"moye-asset:asset-image\" alt=\"说明\">",
            BlockDocument::new(vec![Block::Image {
                id: "block-image".to_string(),
                asset_id: "asset-image".to_string(),
                alt: "说明".to_string(),
                title: None,
                caption: Vec::new(),
            }]),
        ));
        document.validate().expect("valid SVG image document");
        let source: Arc<dyn VisualDocumentSource> = Arc::new(ImageDocumentSource {
            document: document.clone(),
            payload: VisualAssetPayload {
                media_type: "image/svg+xml".to_string(),
                bytes,
            },
        });

        let pages = StructuralPngRenderer
            .render_units(
                RenderUnitsRequest {
                    document: Arc::new(document),
                    source_id: "source-image".to_string(),
                    unit_ids: Vec::new(),
                    profile: RenderProfile::default(),
                    asset_source: Some(source),
                },
                RenderControl::default(),
            )
            .await
            .expect("rasterize SVG asset");

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].media_type, "image/png");
        assert!(pages[1].bytes.starts_with(PNG_SIGNATURE));
        assert_eq!((pages[1].width, pages[1].height), (64, 32));
    }

    #[test]
    fn svg_rasterization_disables_external_resources_and_rejects_size_bypasses() {
        let external = br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image width="32" height="32" href="https://example.invalid/tracker.png"/></svg>"#;
        let rendered = rasterize_svg(
            external,
            &RenderProfile::default(),
            false,
            &RenderControl::default(),
        )
        .expect("ignore an external image without fetching it");
        let decoded = image::load_from_memory_with_format(&rendered.bytes, image::ImageFormat::Png)
            .expect("valid resource-isolated PNG")
            .to_rgba8();
        assert!(decoded.pixels().all(|pixel| pixel.0[3] == 0));

        let oversized = vec![b' '; MAX_STRUCTURAL_SVG_BYTES + 1];
        assert!(
            rasterize_svg(
                &oversized,
                &RenderProfile::default(),
                false,
                &RenderControl::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("字节上限")
        );
        assert!(
            rasterize_svg(
                &[0x1f, 0x8b, 0x08, 0x00],
                &RenderProfile::default(),
                false,
                &RenderControl::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("压缩 SVG")
        );
    }

    #[test]
    fn raster_limits_bound_dimensions_pixels_and_total_output() {
        let profile = RenderProfile {
            viewport_width: 16_384,
            viewport_height: 16_384,
            scale_milli: 8_000,
            ..RenderProfile::default()
        };
        let (width, height) = fitted_raster_dimensions(16_384.0, 16_384.0, &profile, true)
            .expect("fit an extreme valid profile");
        assert!(width <= MAX_RASTER_DIMENSION);
        assert!(height <= MAX_RASTER_DIMENSION);
        assert!(u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS);
        assert!(validate_raster_dimensions(MAX_RASTER_DIMENSION + 1, 1).is_err());

        let mut total = MAX_VISUAL_BATCH_BYTES;
        assert!(add_visual_page_bytes(&mut total, 1).is_err());

        let oversized_raster = png_fixture(MAX_RASTER_DIMENSION + 1, 1);
        assert!(
            prepare_visual_image(
                VisualAssetPayload {
                    media_type: "image/png".to_string(),
                    bytes: oversized_raster,
                },
                "image/png",
                &RenderProfile::default(),
                &RenderControl::default(),
            )
            .is_err()
        );
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn committed_visual_job_rejects_missing_replaced_stopped_and_closed_queue() {
        let store = Arc::new(MemoryVisualJobStore::default());
        let source = Arc::new(MemoryDocumentSource(sample_document()));
        let sink = Arc::new(MemoryPageSink::new(Arc::clone(&store)));
        let renderer: Arc<dyn VisualRenderer> = Arc::new(SlowRenderer);
        let expected = spec("committed-job", renderer.as_ref());
        let mut coordinator = VisualJobCoordinator::new(
            Handle::current(),
            store.clone(),
            source,
            sink,
            vec![renderer],
        )
        .unwrap();

        let error = coordinator.schedule_committed(&expected).await.unwrap_err();
        assert!(error.to_string().contains("不存在"));

        // A same-ID record alone is insufficient: every part of the frozen
        // rendering identity must still agree, even after successful execution.
        for field in [
            "book", "source", "revision", "renderer", "version", "fidelity", "units", "profile",
        ] {
            let mut record = VisualJobRecord::queued(expected.clone(), 1);
            record.state = VisualJobState::Succeeded;
            match field {
                "book" => record.spec.book_id = "other-book".to_string(),
                "source" => record.spec.source_id = "other-source".to_string(),
                "revision" => record.spec.document_revision = Revision::new(4),
                "renderer" => record.spec.renderer = "other-renderer".to_string(),
                "version" => record.spec.renderer_version = "2".to_string(),
                "fidelity" => record.spec.fidelity = RenderFidelity::Normalized,
                "units" => record.spec.unit_ids = vec!["unit-1".to_string()],
                "profile" => record.spec.profile.viewport_width += 1,
                _ => unreachable!(),
            }
            store
                .jobs
                .lock()
                .unwrap()
                .insert(expected.id.clone(), record);
            let error = coordinator.schedule_committed(&expected).await.unwrap_err();
            assert!(error.to_string().contains("身份不一致"), "{field}: {error}");
        }

        for state in [
            VisualJobState::Paused,
            VisualJobState::Cancelled,
            VisualJobState::Failed,
        ] {
            let mut record = VisualJobRecord::queued(expected.clone(), 1);
            record.state = state;
            record.error = Some("injected renderer failure".to_string());
            store
                .jobs
                .lock()
                .unwrap()
                .insert(expected.id.clone(), record);
            let error = coordinator.schedule_committed(&expected).await.unwrap_err();
            assert!(error.to_string().contains("无法调度"), "{state:?}: {error}");
            assert!(error.to_string().contains("injected renderer failure"));
        }

        for pause in [false, true] {
            let mut record = VisualJobRecord::queued(expected.clone(), 1);
            record.state = VisualJobState::Running;
            record.pause_requested = pause;
            record.cancel_requested = !pause;
            store
                .jobs
                .lock()
                .unwrap()
                .insert(expected.id.clone(), record);
            let error = coordinator.schedule_committed(&expected).await.unwrap_err();
            assert!(error.to_string().contains("已请求暂停或取消"));
        }

        coordinator.sender.close();
        (&mut coordinator.worker).await.unwrap();
        for state in [
            VisualJobState::Queued,
            VisualJobState::Running,
            VisualJobState::Succeeded,
        ] {
            let mut record = VisualJobRecord::queued(expected.clone(), 1);
            record.state = state;
            store
                .jobs
                .lock()
                .unwrap()
                .insert(expected.id.clone(), record);
            let error = coordinator.schedule_committed(&expected).await.unwrap_err();
            assert!(
                error.to_string().contains("队列已经关闭"),
                "{state:?}: {error}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coordinator_supports_pause_resume_cancel_and_retry() {
        let store = Arc::new(MemoryVisualJobStore::default());
        let source = Arc::new(MemoryDocumentSource(sample_document()));
        let sink = Arc::new(MemoryPageSink::new(Arc::clone(&store)));
        let slow: Arc<dyn VisualRenderer> = Arc::new(SlowRenderer);
        let gated_impl = Arc::new(GatedRenderer::new());
        let gated: Arc<dyn VisualRenderer> = gated_impl.clone();
        let fail_once_impl = Arc::new(FailOnceRenderer {
            failed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            emitted: Mutex::new(Vec::new()),
        });
        let fail_once: Arc<dyn VisualRenderer> = fail_once_impl.clone();
        let coordinator = VisualJobCoordinator::new(
            Handle::current(),
            store,
            source,
            sink.clone(),
            vec![slow.clone(), gated.clone(), fail_once.clone()],
        )
        .expect("create coordinator");

        gated_impl.permits.add_permits(1);
        coordinator
            .submit(spec("pause-job", gated.as_ref()))
            .await
            .expect("submit pause job");
        wait_for_completed_pages(&coordinator, "pause-job", 1).await;
        assert!(coordinator.pause("pause-job").await.expect("pause job"));
        coordinator
            .wait_for_state("pause-job", VisualJobState::Paused, Duration::from_secs(2))
            .await
            .expect("job pauses");
        gated_impl.permits.add_permits(2);
        assert!(coordinator.resume("pause-job").await.expect("resume job"));
        let succeeded = coordinator
            .wait_for_state(
                "pause-job",
                VisualJobState::Succeeded,
                Duration::from_secs(3),
            )
            .await
            .expect("resumed job succeeds");
        assert_eq!(succeeded.attempts, 2);
        assert_eq!(
            *gated_impl.emitted.lock().expect("gated renderer log"),
            vec![0, 1, 2],
            "resume must not emit the durable prefix again"
        );

        coordinator
            .submit(spec("paused-cancel-job", slow.as_ref()))
            .await
            .expect("submit paused cancellation job");
        coordinator
            .wait_for_state(
                "paused-cancel-job",
                VisualJobState::Running,
                Duration::from_secs(2),
            )
            .await
            .expect("paused cancellation job starts");
        assert!(
            coordinator
                .pause("paused-cancel-job")
                .await
                .expect("pause before cancellation")
        );
        coordinator
            .wait_for_state(
                "paused-cancel-job",
                VisualJobState::Paused,
                Duration::from_secs(2),
            )
            .await
            .expect("job reaches paused state");
        assert!(
            coordinator
                .cancel("paused-cancel-job")
                .await
                .expect("cancel paused job")
        );
        coordinator
            .wait_for_state(
                "paused-cancel-job",
                VisualJobState::Cancelled,
                Duration::from_secs(2),
            )
            .await
            .expect("paused job cancels without being resumed");

        coordinator
            .submit(spec("cancel-job", slow.as_ref()))
            .await
            .expect("submit cancel job");
        coordinator
            .wait_for_state(
                "cancel-job",
                VisualJobState::Running,
                Duration::from_secs(2),
            )
            .await
            .expect("cancel job starts");
        assert!(coordinator.cancel("cancel-job").await.expect("cancel job"));
        coordinator
            .wait_for_state(
                "cancel-job",
                VisualJobState::Cancelled,
                Duration::from_secs(2),
            )
            .await
            .expect("job cancels");

        coordinator
            .submit(spec("retry-job", fail_once.as_ref()))
            .await
            .expect("submit retry job");
        let failed = coordinator
            .wait_for_state("retry-job", VisualJobState::Failed, Duration::from_secs(2))
            .await
            .expect("first attempt fails");
        assert!(
            failed
                .error
                .as_deref()
                .is_some_and(|error| error.contains("intentional"))
        );
        assert!(coordinator.retry("retry-job").await.expect("retry job"));
        coordinator
            .wait_for_state(
                "retry-job",
                VisualJobState::Succeeded,
                Duration::from_secs(2),
            )
            .await
            .expect("retry succeeds");
        assert_eq!(fail_once_impl.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            *fail_once_impl.emitted.lock().expect("fail-once log"),
            vec![0, 0, 1, 2],
            "explicit retry must discard staging and start at page zero"
        );
        assert!(
            sink.published
                .lock()
                .expect("test sink")
                .contains_key("retry-job")
        );
        assert!(
            !sink
                .staging
                .lock()
                .expect("test staging")
                .contains_key("retry-job")
        );
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coordinator_recovers_running_job_without_reemitting_durable_prefix() {
        let store = Arc::new(MemoryVisualJobStore::default());
        let source = Arc::new(MemoryDocumentSource(sample_document()));
        let sink = Arc::new(MemoryPageSink::new(Arc::clone(&store)));
        let renderer_impl = Arc::new(GatedRenderer::new());
        let renderer: Arc<dyn VisualRenderer> = renderer_impl.clone();

        renderer_impl.permits.add_permits(1);
        let first = VisualJobCoordinator::new(
            Handle::current(),
            store.clone(),
            source.clone(),
            sink.clone(),
            vec![renderer.clone()],
        )
        .expect("create first coordinator");
        first
            .submit(spec("recover-job", renderer.as_ref()))
            .await
            .expect("submit recoverable job");
        wait_for_completed_pages(&first, "recover-job", 1).await;
        drop(first);
        tokio::time::sleep(Duration::from_millis(20)).await;

        renderer_impl.permits.add_permits(2);
        let recovered = VisualJobCoordinator::new(
            Handle::current(),
            store,
            source,
            sink.clone(),
            vec![renderer],
        )
        .expect("create recovered coordinator");
        let _ = recovered.recover("recover-job").await;
        recovered
            .wait_for_state(
                "recover-job",
                VisualJobState::Succeeded,
                Duration::from_secs(3),
            )
            .await
            .expect("recovered job succeeds");

        assert_eq!(
            *renderer_impl.emitted.lock().expect("recovery renderer log"),
            vec![0, 1, 2],
            "process recovery must not emit the durable prefix again"
        );
        let published = sink.published.lock().expect("published pages");
        assert_eq!(published["recover-job"].len(), 3);
        assert!(sink.staging.lock().expect("staged pages").is_empty());
    }

    #[test]
    fn sqlite_job_store_persists_spec_and_state_transitions() {
        let temp = tempfile::tempdir().expect("temporary database directory");
        let database_path = temp.path().join("library.db");
        let connection = db::open_or_recreate(&database_path).expect("create current database");
        db::blobs::insert(
            &connection,
            &db::blobs::BlobRecord {
                object_key: "objects/test-source".to_string(),
                media_type: "application/epub+zip".to_string(),
                byte_len: 4,
                hash: "sqlite-preview-source-hash".to_string(),
                created_at: 1,
            },
        )
        .expect("insert source blob");
        db::books::insert(
            &connection,
            &db::books::BookRecord {
                id: "book-visual".to_string(),
                title: "视觉测试".to_string(),
                author: String::new(),
                language: Some("zh-CN".to_string()),
                description: None,
                format: "epub".to_string(),
                revision: 3,
                source_object_key: "objects/test-source".to_string(),
                cover_asset_id: None,
                cover_object_key: None,
                cover_mime: None,
                added_at: 1,
                updated_at: 1,
                last_spine: 0,
                group_id: None,
            },
        )
        .expect("insert test book");
        db::book_sources::insert(
            &connection,
            &db::book_sources::BookSource {
                id: "source-visual".to_string(),
                book_id: "book-visual".to_string(),
                revision: 3,
                format: "epub".to_string(),
                source_kind: "original".to_string(),
                object_key: "objects/test-source".to_string(),
                source_name: Some("test.epub".to_string()),
                created_at: 1,
            },
        )
        .expect("insert test source");
        drop(connection);

        let renderer = StructuralPngRenderer;
        let store = SqliteVisualJobStore::new(&database_path);
        let record = VisualJobRecord::queued(spec("sqlite-job", &renderer), 10);
        store.create(&record).expect("persist queued job");
        assert_eq!(store.get("sqlite-job").expect("read job"), Some(record));

        assert!(
            store
                .request_pause("sqlite-job", 11)
                .expect("request pause")
        );
        store
            .transition("sqlite-job", VisualJobState::Paused, 2, None, 12)
            .expect("persist paused state");
        let paused = store
            .get("sqlite-job")
            .expect("read paused job")
            .expect("paused job exists");
        assert_eq!(paused.state, VisualJobState::Paused);
        assert_eq!(paused.completed_pages, 2);
        assert!(!paused.pause_requested);

        assert!(
            store
                .resume("sqlite-job", 13)
                .expect("resume persisted job")
        );
        store
            .transition("sqlite-job", VisualJobState::Running, 2, None, 14)
            .expect("persist running state");
        store
            .transition(
                "sqlite-job",
                VisualJobState::Failed,
                2,
                Some("test failure"),
                15,
            )
            .expect("persist failure");
        let failed = store
            .get("sqlite-job")
            .expect("read failed job")
            .expect("failed job exists");
        assert_eq!(failed.attempts, 1);
        assert_eq!(failed.error.as_deref(), Some("test failure"));

        assert!(store.retry("sqlite-job", 16).expect("retry persisted job"));
        let retried = store
            .get("sqlite-job")
            .expect("read retried job")
            .expect("retried job exists");
        assert_eq!(retried.state, VisualJobState::Queued);
        assert_eq!(retried.completed_pages, 0);
        assert_eq!(retried.error, None);
    }

    #[test]
    fn pdfjs_bundle_is_local_and_hardened() {
        assert!(PDFJS_VIEWER_HTML.contains("connect-src 'self'"));
        assert!(PDFJS_VIEWER_HTML.contains("worker-src 'self' blob:"));
        assert!(PDFJS_VIEWER_SCRIPT.contains("isEvalSupported: false"));
        assert!(PDFJS_VIEWER_SCRIPT.contains("enableXfa: false"));
        assert!(PDFJS_VIEWER_SCRIPT.contains("useWorkerFetch: false"));
        // The reader lays the whole document out in one scrollable column, so
        // every page is drawn on demand and released again once it is far away.
        assert!(PDFJS_VIEWER_SCRIPT.contains("IntersectionObserver("));
        assert!(PDFJS_VIEWER_SCRIPT.contains("moye-pdf-page-changed"));
        assert!(PDFJS_VIEWER_SCRIPT.contains("moye-pdf-selection-changed"));
        assert!(PDFJS_VIEWER_SCRIPT.contains("moye-pdf-request-page"));
        // The compact reading preference reaches the viewer through one URL
        // parameter and one document attribute, and stale bundle assets would
        // silently ignore it.
        assert!(PDFJS_VIEWER_SCRIPT.contains("\"data-pdf-compact\""));
        assert!(PDFJS_VIEWER_HTML.contains("data-pdf-compact"));
        assert!(!PDFJS_VIEWER_SCRIPT.contains("https://"));
        let (mime, library) = bundled_pdfjs_asset("pdf.mjs").expect("local PDF.js library");
        assert_eq!(mime, "text/javascript; charset=utf-8");
        assert!(library.len() > 100_000, "PDF.js must not be a stub");
        assert!(bundled_pdfjs_asset("../pdf.mjs").is_none());
        assert!(bundled_pdfjs_asset("%2e%2e/pdf.mjs").is_none());
    }

    #[test]
    fn persisted_cursor_round_trips_without_document_bytes() {
        let renderer = StructuralPngRenderer;
        let spec = spec("persisted-job", &renderer);
        let encoded = PersistedVisualCursor::new(spec.clone(), 7)
            .encode()
            .expect("encode cursor");
        let decoded = PersistedVisualCursor::decode(&encoded).expect("decode cursor");
        assert_eq!(decoded.spec, spec);
        assert_eq!(decoded.completed_pages, 7);
        assert!(!encoded.contains("包含 <script>"));
    }
}
