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
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics, classify_error, record_for_database},
    db,
    document::{
        Block, BookDocument, ContentUnit, ContentUnitKind, DocumentLocator, Inline, Revision,
        SourceLocator, TableRow, deterministic_id,
    },
};

mod coordinator;

mod image_data;

mod job;

mod page;

mod structural;

mod worksheet;

pub(crate) use image_data::*;

pub(crate) use job::*;

pub(crate) use page::*;

pub(crate) use structural::*;

pub(crate) use worksheet::*;

// The public preview surface, unchanged by the split.
pub use coordinator::VisualJobCoordinator;
pub use job::{
    MemoryVisualJobStore, SqliteVisualJobStore, VisualAssetPayload, VisualDocumentSource,
    VisualJobRecord, VisualJobSpec, VisualJobState, VisualJobStore, VisualPageSink,
    VisualSourcePayload,
};
pub use structural::StructuralPngRenderer;

#[cfg(test)]
mod tests;
pub const PDFJS_VERSION: &str = "5.7.284";
pub const PDFJS_VIEWER_HTML: &str = include_str!("../../assets/pdfjs/viewer.html");
pub const PDFJS_VIEWER_SCRIPT: &str = include_str!("../../assets/pdfjs/viewer.mjs");
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
