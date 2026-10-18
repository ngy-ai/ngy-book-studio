//! Row shapes of the `index_jobs` table: the status enum, the job row, and the
//! mapping between them.

use super::*;

pub(crate) const VISION_WAITING_FOR_PAGES_ERROR: &str =
    "视觉页面尚未生成；页面渲染完成后可自动或手动重试";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexJobStatus {
    Queued,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

impl IndexJobStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(rusqlite::Error::InvalidColumnType(
                4,
                "status".to_string(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexJob {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) kind: String,
    pub(crate) status: IndexJobStatus,
    pub(crate) pause_requested: bool,
    pub(crate) cancel_requested: bool,
    pub(crate) attempts: u32,
    pub(crate) cursor_json: String,
    pub(crate) error: Option<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) started_at: Option<u64>,
    pub(crate) finished_at: Option<u64>,
}

pub(super) const SELECT: &str = "SELECT id, book_id, source_id, kind, status, pause_requested,
    cancel_requested, attempts, cursor_json, error, created_at, updated_at, started_at,
    finished_at FROM index_jobs";

pub(super) fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexJob> {
    let status = row.get::<_, String>(4)?;
    Ok(IndexJob {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        kind: row.get(3)?,
        status: IndexJobStatus::parse(&status)?,
        pause_requested: row.get(5)?,
        cancel_requested: row.get(6)?,
        attempts: row.get(7)?,
        cursor_json: row.get(8)?,
        error: row.get(9)?,
        created_at: row.get::<_, i64>(10)? as u64,
        updated_at: row.get::<_, i64>(11)? as u64,
        started_at: row.get::<_, Option<i64>>(12)?.map(|value| value as u64),
        finished_at: row.get::<_, Option<i64>>(13)?.map(|value| value as u64),
    })
}
