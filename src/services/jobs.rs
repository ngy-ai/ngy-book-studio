//! The projections the background-job window reads.

use super::*;

/// Stable, UI-facing state for all persisted derivative work. Keeping this
/// type in the service layer prevents GPUI code from depending on database
/// rows or renderer-specific cursor JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundJobStatus {
    Queued,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundJobAction {
    Pause,
    Resume,
    Retry,
    Cancel,
    /// Re-runs a whole-book translation from the first block, discarding the
    /// persisted译文 of that book and language.
    Retranslate,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackgroundJobProgress {
    pub completed: usize,
    pub total: Option<usize>,
    /// 整本翻译此刻打开在窗口里的文本块（升序块号）。块级明细的「处理中」只认这一份
    /// 读数：原始游标对运行中的任务是不下发的（见 `cursor_json`），所以窗口必须由
    /// 服务层解析后交出去，界面不自己读内部 JSON。其它任务逐项走、窗口恒空。
    pub processing: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundJobSnapshot {
    pub id: String,
    pub book_id: String,
    /// Foreign key into `book_sources`. `None` for legacy tasks created before
    /// sources became first-class. The details UI uses this to look up the
    /// owning source kind without exposing raw identifiers beyond the service
    /// layer.
    pub source_id: Option<String>,
    pub kind: String,
    pub status: BackgroundJobStatus,
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub attempts: u32,
    pub progress: BackgroundJobProgress,
    pub error: Option<String>,
    /// Unix seconds (UTC) — populated by the service layer so the UI can
    /// render a stable local timestamp without re-querying the database.
    pub created_at: u64,
    pub updated_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    /// Raw renderer/provider cursor JSON. Only kept for `Failed` jobs and
    /// for `visual_render` jobs so users can see which renderer profile and
    /// units were used; otherwise the cursor can be very large and noisy.
    /// A running whole-book translation therefore does *not* ship its cursor:
    /// the live block window it needs is projected into
    /// [`BackgroundJobProgress::processing`] instead.
    pub cursor_json: Option<String>,
}

/// One current translation of a text block, keyed for the reader's DOM
/// matching. `source` is the canonical source text used as a matching hint when
/// the chapter HTML carries no block identifiers.
///
/// `manual` marks a block whose segments are the reader's own edit rather than
/// the model output; the reading window renders the same payload either way and
/// only uses the flag for the "已手工修改 / 恢复机器译文" affordances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslatedBlock {
    pub key: String,
    pub source: String,
    pub segments: Vec<crate::ai::translation::TranslationSegment>,
    pub manual: bool,
}

/// Whether a book's declared language already satisfies a target tag. Compares
/// only the primary subtag so `en-US` matches `en` and `zh` matches `zh-Hans`.
pub fn language_is_target(source: Option<&str>, target: &str) -> bool {
    pub(crate) fn primary(tag: &str) -> String {
        tag.trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
    match source {
        Some(source) => {
            let source = primary(source);
            !source.is_empty() && source == primary(target)
        }
        None => false,
    }
}

/// Which persisted visual page set a reader window consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualPageSourceKind {
    /// Microsoft Office COM enhancement pages (Windows only).
    OfficeEnhanced,
    /// Imported DjVu scans rasterized by the portable `ngy-djvu-png` renderer.
    Djvu,
}

/// One verified page loaded from the managed object store.
///
/// Object keys and filesystem paths deliberately stay private to the service
/// layer; callers receive only stable document coordinates and owned bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedVisualPage {
    pub file_name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub content_unit_id: Option<String>,
    pub locator: crate::document::DocumentLocator,
    /// Pixel size of the published page image, used for reader zoom.
    pub width: u32,
    pub height: u32,
}

pub(crate) fn background_job_snapshot(
    conn: &rusqlite::Connection,
    job: db::index_jobs::IndexJob,
) -> Result<BackgroundJobSnapshot> {
    let cursor = serde_json::from_str::<serde_json::Value>(&job.cursor_json).ok();
    let completed = match job.kind.as_str() {
        "visual_render" => cursor
            .as_ref()
            .and_then(|value| json_usize(value.get("completed_pages")))
            .unwrap_or_default(),
        "embedding" | "vision" | "translation" => cursor
            .as_ref()
            .and_then(|value| json_usize(value.get("next_ordinal")))
            .unwrap_or_default(),
        _ => 0,
    };
    let total = match (job.kind.as_str(), job.source_id.as_deref()) {
        ("embedding", Some(source_id)) => {
            Some(db::search_chunks::count_for_source(conn, source_id)?)
        }
        ("vision", Some(source_id)) => Some(db::visual_pages::count_for_source(conn, source_id)?),
        // A content unit can render to several pages. The final page count
        // is known only after publication succeeds; unit_ids is not a page total.
        ("visual_render", _) if job.status == db::index_jobs::IndexJobStatus::Succeeded => {
            Some(completed)
        }
        ("visual_render", _) => None,
        _ => None,
    };
    let processing = match job.kind.as_str() {
        "translation" => in_flight_ordinals(cursor.as_ref()),
        _ => Vec::new(),
    };
    let kept_cursor = snapshot_cursor_json(&job.kind, job.status, &job.cursor_json);
    Ok(BackgroundJobSnapshot {
        id: job.id,
        book_id: job.book_id,
        source_id: job.source_id,
        kind: job.kind,
        status: match job.status {
            db::index_jobs::IndexJobStatus::Queued => BackgroundJobStatus::Queued,
            db::index_jobs::IndexJobStatus::Running => BackgroundJobStatus::Running,
            db::index_jobs::IndexJobStatus::Paused => BackgroundJobStatus::Paused,
            db::index_jobs::IndexJobStatus::Succeeded => BackgroundJobStatus::Succeeded,
            db::index_jobs::IndexJobStatus::Failed => BackgroundJobStatus::Failed,
            db::index_jobs::IndexJobStatus::Cancelled => BackgroundJobStatus::Cancelled,
        },
        pause_requested: job.pause_requested,
        cancel_requested: job.cancel_requested,
        attempts: job.attempts,
        progress: BackgroundJobProgress {
            completed,
            total,
            processing,
        },
        error: job.error,
        created_at: job.created_at,
        updated_at: job.updated_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        cursor_json: kept_cursor,
    })
}

/// Decide whether to expose the raw cursor JSON to the UI. Visual render jobs
/// keep their cursor so users can inspect the renderer profile and unit list;
/// failed jobs of any kind keep theirs because the cursor often contains the
/// last successful provider response or page index. Other jobs return `None`
/// because the cursor is just an internal offset.
pub(crate) fn snapshot_cursor_json(
    kind: &str,
    status: db::index_jobs::IndexJobStatus,
    cursor: &str,
) -> Option<String> {
    let keep = kind == "visual_render" || matches!(status, db::index_jobs::IndexJobStatus::Failed);
    if keep && !cursor.is_empty() {
        Some(cursor.to_string())
    } else {
        None
    }
}

/// 整本翻译在持久游标里记下的「处理中」窗口，升序、去重。
///
/// 只有整本翻译会填 `inflight`（其它任务逐项走、窗口恒空），所以它是「这本图书现在
/// 有几块在跑」的唯一权威读数。缺字段、写坏的 JSON 或非数字项一律按「没有在飞的块」
/// 处理，绝不猜一个出来：旧格式游标不该凭空显示成处理中。
pub(crate) fn in_flight_ordinals(cursor: Option<&serde_json::Value>) -> Vec<usize> {
    let mut blocks = cursor
        .and_then(|value| value.get("inflight"))
        .and_then(serde_json::Value::as_array)
        .map(|ordinals| {
            ordinals
                .iter()
                .filter_map(|ordinal| ordinal.as_u64())
                .filter_map(|ordinal| usize::try_from(ordinal).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}

pub(crate) fn json_usize(value: Option<&serde_json::Value>) -> Option<usize> {
    value
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

pub(crate) fn background_kind_order(kind: &str) -> u8 {
    match kind {
        "visual_render" => 0,
        "vision" => 1,
        "embedding" => 2,
        "translation" => 3,
        _ => 4,
    }
}
