//! Bounded, content-free diagnostics for durable background jobs.
//!
//! This is a separate SQLite file under the owning library's data directory.
//! Recording never changes a job's outcome. Only fixed events, fixed error
//! categories and numeric measurements can enter the log; book text, responses,
//! credentials, URLs and arbitrary error strings have no input field.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub const BACKGROUND_JOB_LOG_LIMIT: usize = 500;
pub const BACKGROUND_JOB_LOG_TOTAL_LIMIT: usize = 50_000;
const MAX_DATABASE_PAGES: u64 = 16_384;
const MAX_PAYLOAD_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobLogLevel {
    Info,
    Warning,
    Error,
}

impl JobLogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warning => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// The event is the complete message vocabulary. Do not replace this with a
/// free-form string API: providers can echo private input in any error field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobLogEvent {
    Queued,
    Recovered,
    RunStarted,
    SourceLoaded,
    ItemStarted,
    ModelRequested,
    ModelCompleted,
    ProtocolRejected,
    ProtocolCorrection,
    ProtocolSkipped,
    ItemSaved,
    PublicationStarted,
    RunSucceeded,
    RunFailed,
    StepFailed,
    PauseRequested,
    ResumeRequested,
    RetryRequested,
    CancelRequested,
    RetranslateRequested,
    Paused,
    Cancelled,
    Superseded,
    Skipped,
    WaitingForPages,
    CacheHit,
    NoTranslatableSegments,
    PreviewOnlySkipped,
}

impl JobLogEvent {
    fn presentation(self) -> (JobLogLevel, &'static str, &'static str) {
        use JobLogLevel::{Error, Info, Warning};
        match self {
            Self::Queued => (Info, "queue", "任务已加入执行队列"),
            Self::Recovered => (Info, "queue", "已从持久化队列恢复任务调度"),
            Self::RunStarted => (Info, "run", "任务开始执行"),
            Self::SourceLoaded => (Info, "source", "已加载并校验任务来源"),
            Self::ItemStarted => (Info, "item", "开始处理当前单元"),
            Self::ModelRequested => (Info, "model", "已发起模型请求"),
            Self::ModelCompleted => (Info, "model", "模型响应接收完成"),
            Self::ProtocolRejected => (Warning, "protocol", "模型响应未通过协议校验"),
            Self::ProtocolCorrection => (Info, "protocol", "使用冻结输入发起一次协议纠正"),
            Self::ProtocolSkipped => (
                Warning,
                "protocol",
                "模型响应在自动纠正后仍不符合分段协议，已跳过当前文本块并保留原文",
            ),
            Self::ItemSaved => (Info, "persist", "当前单元结果已持久化"),
            Self::PublicationStarted => (Info, "publish", "开始发布完整任务结果"),
            Self::RunSucceeded => (Info, "run", "任务执行完成"),
            Self::RunFailed => (Error, "run", "任务执行失败"),
            Self::StepFailed => (Error, "item", "当前处理步骤失败"),
            Self::PauseRequested => (Info, "control", "已接受暂停请求"),
            Self::ResumeRequested => (Info, "control", "已接受继续请求"),
            Self::RetryRequested => (Info, "control", "已接受重试请求"),
            Self::CancelRequested => (Info, "control", "已接受取消请求"),
            Self::RetranslateRequested => (Info, "control", "已清除旧译文并重排翻译任务"),
            Self::Paused => (Info, "run", "任务已暂停，保留执行进度"),
            Self::Cancelled => (Info, "run", "任务已取消"),
            Self::Superseded => (Warning, "identity", "任务身份已变化，停止本次执行"),
            Self::Skipped => (Info, "item", "当前单元无需处理"),
            Self::WaitingForPages => (Warning, "dependency", "等待视觉页面生成后重试"),
            Self::CacheHit => (Info, "cache", "已复用通过身份校验的缓存结果"),
            Self::NoTranslatableSegments => (Info, "item", "当前单元没有需要翻译的文字片段"),
            Self::PreviewOnlySkipped => (Info, "item", "当前单元仅用于预览，跳过本次处理"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobLogErrorKind {
    Provider,
    Database,
    Io,
    InvalidData,
    Timeout,
    Translation,
    ContextWindowExceeded,
    StreamProtocol,
    Cancelled,
    InvalidJson,
    IncompleteJson,
    InvalidSchema,
    IncompleteReasoning,
    AmbiguousJson,
    MissingJson,
    SegmentCountMismatch,
    UnknownSegmentId,
    DuplicateSegmentId,
    MissingSegmentId,
    EmptySegmentText,
    InvalidSegmentText,
    Unknown,
}

/// Inspect typed errors through the existing safe classifier; never inspect or
/// persist `Display`, a context message, an HTTP body or a URL.
pub fn classify_error(error: &anyhow::Error) -> JobLogErrorKind {
    use JobLogErrorKind::*;
    match crate::ai_diagnostics::error_kind(error) {
        "database" => Database,
        "io" | "json_io" => Io,
        "http_timeout" | "tool_timeout" => Timeout,
        "http_rejected" | "http_connect" | "http_body" | "http_decode" | "http_transport" => {
            Provider
        }
        "context_window_exceeded" => ContextWindowExceeded,
        "stream_protocol" | "incomplete_tool_arguments" => StreamProtocol,
        "cancelled" => Cancelled,
        "invalid_json" | "json_syntax" => InvalidJson,
        "incomplete_json" | "json_eof" => IncompleteJson,
        "invalid_schema" | "json_data" => InvalidSchema,
        "incomplete_reasoning" => IncompleteReasoning,
        "ambiguous_json" => AmbiguousJson,
        "missing_json" => MissingJson,
        "segment_count_mismatch" => SegmentCountMismatch,
        "unknown_segment_id" => UnknownSegmentId,
        "duplicate_segment_id" => DuplicateSegmentId,
        "missing_segment_id" => MissingSegmentId,
        "empty_segment_text" => EmptySegmentText,
        "invalid_segment_text" => InvalidSegmentText,
        "agent_configuration" | "tool_arguments" | "agent_limit" | "scope_violation" => InvalidData,
        _ => Unknown,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobLogMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_column: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<JobLogErrorKind>,
}

impl JobLogMetrics {
    pub fn for_error(error: &anyhow::Error) -> Self {
        let http_status = error
            .downcast_ref::<crate::ai::ProviderHttpError>()
            .map(|error| u64::from(error.status))
            .or_else(|| {
                error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(reqwest::Error::status)
                    .map(|status| u64::from(status.as_u16()))
            });
        Self {
            error_kind: Some(classify_error(error)),
            http_status,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundJobLogEntry {
    pub timestamp_ms: u64,
    pub level: JobLogLevel,
    pub stage: String,
    pub message: String,
    pub metrics: JobLogMetrics,
}

impl BackgroundJobLogEntry {
    /// A stable, complete copy representation with an explicit UTC timestamp.
    pub fn format_line(&self) -> String {
        let mut line = format!(
            "{} [{}] [{}] {}",
            format_timestamp(self.timestamp_ms),
            self.level.as_str(),
            self.stage,
            self.message
        );
        for (label, value) in [
            ("run_id", self.metrics.run_id),
            ("ordinal", self.metrics.ordinal),
            ("total", self.metrics.total),
            ("attempt", self.metrics.attempt),
            ("request_attempt", self.metrics.request_attempt),
            ("duration_ms", self.metrics.duration_ms),
            ("response_bytes", self.metrics.response_bytes),
            ("http_status", self.metrics.http_status),
            ("json_line", self.metrics.json_line),
            ("json_column", self.metrics.json_column),
            ("expected_count", self.metrics.expected_count),
            ("actual_count", self.metrics.actual_count),
        ] {
            if let Some(value) = value {
                let _ = write!(line, " {label}={value}");
            }
        }
        if let Some(error_kind) = self.metrics.error_kind {
            // Serialization of a closed enum cannot expose an error body.
            if let Ok(label) = serde_json::to_string(&error_kind) {
                let _ = write!(line, " error_kind={}", label.trim_matches('"'));
            }
        }
        line
    }
}

fn format_timestamp(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let days = (seconds / 86_400) as i64;
    // Civil date conversion from Unix days, independent of local timezone and
    // platform date APIs. The input is nonnegative and well within i64.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03} UTC",
        time / 3600,
        time / 60 % 60,
        time % 60,
        timestamp_ms % 1000
    )
}

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
            target: "moye_ai",
            error_kind = crate::ai_diagnostics::error_kind(&error),
            "background job diagnostic could not be persisted"
        );
    }
}

#[derive(Clone, Debug)]
pub(crate) struct JobDiagnosticStore {
    path: PathBuf,
}

impl JobDiagnosticStore {
    pub(crate) fn for_database(database_path: &Path) -> Result<Self> {
        // Canonicalizing the owning DB also prevents relative or differently
        // spelled paths for the same library from splitting their log history.
        let canonical = fs::canonicalize(database_path).context("无法确认任务日志所属图书库")?;
        let parent = canonical.parent().context("任务日志缺少图书库目录")?;
        let key = blake3::hash(canonical.as_os_str().as_encoded_bytes());
        Ok(Self {
            path: parent.join("job-logs").join(format!("{key}.sqlite3")),
        })
    }

    fn connect(&self) -> Result<Connection> {
        fs::create_dir_all(self.path.parent().context("任务日志目录无效")?)?;
        let conn = Connection::open(&self.path).context("无法打开任务日志")?;
        conn.busy_timeout(Duration::from_millis(250))?;
        conn.pragma_update(None, "max_page_count", MAX_DATABASE_PAGES)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS logs (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                job_key TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                event_json TEXT NOT NULL,
                metrics_json TEXT NOT NULL,
                pruned INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS logs_job_sequence ON logs(job_key, sequence);",
        )?;
        Ok(conn)
    }

    pub(crate) fn record(
        &self,
        job_id: &str,
        event: JobLogEvent,
        metrics: JobLogMetrics,
    ) -> Result<()> {
        let mut conn = self.connect()?;
        self.record_on(
            &mut conn,
            job_id,
            event,
            metrics,
            BACKGROUND_JOB_LOG_LIMIT,
            BACKGROUND_JOB_LOG_TOTAL_LIMIT,
        )
    }

    fn record_on(
        &self,
        conn: &mut Connection,
        job_id: &str,
        event: JobLogEvent,
        metrics: JobLogMetrics,
        job_limit: usize,
        total_limit: usize,
    ) -> Result<()> {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("任务日志系统时间无效")?
            .as_millis()
            .min(i64::MAX as u128) as u64;
        let job_key = blake3::hash(job_id.as_bytes()).to_hex().to_string();
        let event_json = serde_json::to_string(&event)?;
        let metrics_json = serde_json::to_string(&metrics)?;
        ensure!(
            metrics_json.len() <= MAX_PAYLOAD_BYTES,
            "任务日志数值字段过大"
        );
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO logs(job_key, timestamp_ms, event_json, metrics_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![job_key, timestamp_ms, event_json, metrics_json],
        )?;
        let job_cutoff: Option<i64> = tx
            .query_row(
                "SELECT sequence FROM logs WHERE job_key = ?1
                 ORDER BY sequence DESC LIMIT 1 OFFSET ?2",
                params![job_key, job_limit - 1],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(cutoff) = job_cutoff {
            let removed = tx.execute(
                "DELETE FROM logs WHERE job_key = ?1 AND sequence < ?2",
                params![job_key, cutoff],
            )?;
            if removed != 0 {
                tx.execute(
                    "UPDATE logs SET pruned = 1 WHERE sequence = last_insert_rowid()",
                    [],
                )?;
            }
        }
        let global_cutoff: Option<i64> = tx
            .query_row(
                "SELECT sequence FROM logs ORDER BY sequence DESC LIMIT 1 OFFSET ?1",
                [total_limit - 1],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(cutoff) = global_cutoff {
            tx.execute(
                "UPDATE logs SET pruned = 1 WHERE sequence >= ?1 AND job_key IN (
                    SELECT job_key FROM logs WHERE sequence < ?1)",
                [cutoff],
            )?;
            tx.execute("DELETE FROM logs WHERE sequence < ?1", [cutoff])?;
        }
        tx.commit().context("无法保存任务日志")
    }

    pub(crate) fn read(&self, job_id: &str) -> Result<BackgroundJobLogSnapshot> {
        if !self.path.try_exists().context("无法检查任务日志")? {
            return Ok(BackgroundJobLogSnapshot::default());
        }
        let conn = self.connect()?;
        let job_key = blake3::hash(job_id.as_bytes()).to_hex().to_string();
        let mut statement = conn.prepare(
            "SELECT timestamp_ms, event_json, metrics_json, pruned FROM logs
             WHERE job_key = ?1 ORDER BY sequence DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![job_key, BACKGROUND_JOB_LOG_LIMIT], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?;
        let mut snapshot = BackgroundJobLogSnapshot::default();
        for row in rows {
            let (timestamp_ms, event_json, metrics_json, pruned) = row?;
            ensure!(
                event_json.len() <= MAX_PAYLOAD_BYTES && metrics_json.len() <= MAX_PAYLOAD_BYTES,
                "任务日志记录超出上限"
            );
            // Validate persisted input again before deriving UI text. A damaged
            // diagnostic file must not turn into a source of arbitrary messages.
            let event: JobLogEvent = serde_json::from_str(&event_json)
                .map_err(|_| anyhow::anyhow!("任务日志事件无效"))?;
            let metrics = serde_json::from_str(&metrics_json)
                .map_err(|_| anyhow::anyhow!("任务日志字段无效"))?;
            let (level, stage, message) = event.presentation();
            snapshot.entries.push(BackgroundJobLogEntry {
                timestamp_ms,
                level,
                stage: stage.into(),
                message: message.into(),
                metrics,
            });
            snapshot.truncated |= pruned;
        }
        snapshot.entries.reverse();
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, JobDiagnosticStore) {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("library.db");
        fs::write(&database, []).unwrap();
        let store = JobDiagnosticStore::for_database(&database).unwrap();
        (temp, database, store)
    }

    #[test]
    fn records_survive_reopen_and_do_not_expose_input_or_error_strings() {
        let (_temp, database, store) = fixture();
        let secret = "book body api-key=secret https://private.example/token";
        store
            .record(
                secret,
                JobLogEvent::RunFailed,
                JobLogMetrics {
                    run_id: Some(17),
                    ordinal: Some(8),
                    error_kind: Some(classify_error(&anyhow::anyhow!(secret.to_string()))),
                    ..Default::default()
                },
            )
            .unwrap();
        drop(store);
        let reopened = JobDiagnosticStore::for_database(&database).unwrap();
        let log = reopened.read(secret).unwrap();
        assert_eq!(log.entries.len(), 1);
        let line = log.entries[0].format_line();
        assert!(line.contains("run_id=17 ordinal=8"));
        assert!(line.contains("error_kind=unknown"));
        assert!(!line.contains(secret));
        assert!(!String::from_utf8_lossy(&fs::read(reopened.path).unwrap()).contains(secret));
    }

    #[test]
    fn task_and_library_scopes_are_independent() {
        let (_first, database, first) = fixture();
        let (_second, _, second) = fixture();
        first
            .record("job-a", JobLogEvent::RunStarted, JobLogMetrics::default())
            .unwrap();
        assert_eq!(first.read("job-a").unwrap().entries.len(), 1);
        assert!(first.read("job-b").unwrap().entries.is_empty());
        assert!(second.read("job-a").unwrap().entries.is_empty());
        let sibling = database.with_file_name("another-library.db");
        fs::write(&sibling, []).unwrap();
        assert!(
            JobDiagnosticStore::for_database(&sibling)
                .unwrap()
                .read("job-a")
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn retention_prunes_old_rows_atomically_and_bounds_the_whole_store() {
        let (_temp, _, store) = fixture();
        let mut conn = store.connect().unwrap();
        for ordinal in 0..8 {
            store
                .record_on(
                    &mut conn,
                    "job-a",
                    JobLogEvent::ItemSaved,
                    JobLogMetrics {
                        ordinal: Some(ordinal),
                        ..Default::default()
                    },
                    3,
                    5,
                )
                .unwrap();
        }
        let first = store.read("job-a").unwrap();
        assert!(first.truncated);
        assert_eq!(first.entries.len(), 3);
        assert_eq!(first.entries[0].metrics.ordinal, Some(5));
        for _ in 0..3 {
            store
                .record_on(
                    &mut conn,
                    "job-b",
                    JobLogEvent::RunStarted,
                    JobLogMetrics::default(),
                    3,
                    5,
                )
                .unwrap();
        }
        let count: usize = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 5);
        let first = store.read("job-a").unwrap();
        assert_eq!(first.entries.len(), 2);
        assert!(first.truncated);
        assert_eq!(first.entries[0].metrics.ordinal, Some(6));
        let max_pages: u64 = conn
            .pragma_query_value(None, "max_page_count", |row| row.get(0))
            .unwrap();
        assert_eq!(max_pages, MAX_DATABASE_PAGES);
    }

    #[test]
    fn production_retention_returns_exactly_the_most_recent_500_rows() {
        let (_temp, _, store) = fixture();
        let mut conn = store.connect().unwrap();
        for ordinal in 0..=BACKGROUND_JOB_LOG_LIMIT {
            store
                .record_on(
                    &mut conn,
                    "job",
                    JobLogEvent::ItemSaved,
                    JobLogMetrics {
                        ordinal: Some(ordinal as u64),
                        ..Default::default()
                    },
                    BACKGROUND_JOB_LOG_LIMIT,
                    BACKGROUND_JOB_LOG_TOTAL_LIMIT,
                )
                .unwrap();
        }
        let snapshot = store.read("job").unwrap();
        assert_eq!(snapshot.entries.len(), 500);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.entries[0].metrics.ordinal, Some(1));
        assert_eq!(snapshot.entries[499].metrics.ordinal, Some(500));
    }

    #[test]
    fn persisted_unknown_fields_are_rejected() {
        let (_temp, _, store) = fixture();
        store
            .record("job", JobLogEvent::RunStarted, JobLogMetrics::default())
            .unwrap();
        store
            .connect()
            .unwrap()
            .execute(
                "UPDATE logs SET metrics_json = ?1",
                [r#"{"response_text":"private response"}"#],
            )
            .unwrap();
        let error = store.read("job").unwrap_err();
        assert_eq!(error.to_string(), "任务日志字段无效");
        assert!(!format!("{error:#}").contains("response_text"));
        assert!(!format!("{error:#}").contains("private response"));
    }

    #[test]
    fn copied_timestamps_include_readable_utc_date_and_milliseconds() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00.000 UTC");
        assert_eq!(
            format_timestamp(1_700_000_000_123),
            "2023-11-14 22:13:20.123 UTC"
        );
    }
}
