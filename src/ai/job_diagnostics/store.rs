//! The in-memory store behind the diagnostics window.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct JobDiagnosticStore {
    pub(crate) path: PathBuf,
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

    pub(crate) fn connect(&self) -> Result<Connection> {
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

    pub(crate) fn record_on(
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
                event,
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
