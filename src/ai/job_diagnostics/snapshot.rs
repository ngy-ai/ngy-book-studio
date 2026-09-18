//! The snapshot handed to the UI and the database-facing record.

use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackgroundJobLogSnapshot {
    /// Oldest to newest, limited to the most recent 500 persisted records.
    pub entries: Vec<BackgroundJobLogEntry>,
    /// Some earlier rows for this job have been removed by retention.
    pub truncated: bool,
}

/// Best-effort, bounded local writes run at stage boundaries, never per token.
/// A storage error only emits a fixed category and cannot fail the owning job.
pub fn record_for_database(
    database_path: &Path,
    job_id: &str,
    event: JobLogEvent,
    metrics: JobLogMetrics,
) {
    if let Err(error) = JobDiagnosticStore::for_database(database_path)
        .and_then(|store| store.record(job_id, event, metrics))
    {
        tracing::warn!(
            target: "ngy_ai",
            error_kind = crate::ai::ai_diagnostics::error_kind(&error),
            "background job diagnostic could not be persisted"
        );
    }
}
