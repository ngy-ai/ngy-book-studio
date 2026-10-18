//! The visual job: its state machine, the records it is stored as, and the
//! memory and SQLite stores behind it.

use super::*;

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
    pub(crate) jobs: Mutex<HashMap<String, VisualJobRecord>>,
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
            } else if record.pause_requested {
                // 与 SQLite 存储同一条规则：没有执行者能观察这个 flag。排队中的行按
                // paused 修复，其它状态只把 flag 清掉，让真实状态重新可见。
                if record.state == VisualJobState::Queued {
                    record.state = VisualJobState::Paused;
                    record.error = None;
                }
                record.pause_requested = false;
                record.updated_at = now;
                changed += 1;
            }
        }
        Ok(changed)
    }

    fn request_pause(&self, job_id: &str, now: u64) -> Result<bool> {
        self.update(job_id, |record| {
            if record.cancel_requested {
                return false;
            }
            match record.state {
                // 排队中的任务没有执行者：暂停当场结清。flag 单独留着的话这行既不会被
                // 领取、也没有人消费它，界面会永远显示「正在暂停」。
                VisualJobState::Queued => {
                    record.state = VisualJobState::Paused;
                    record.pause_requested = false;
                    record.error = None;
                    record.updated_at = now;
                    true
                }
                VisualJobState::Running => {
                    record.pause_requested = true;
                    record.updated_at = now;
                    true
                }
                _ => false,
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
    pub(crate) fn update(
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
    pub(crate) database_path: PathBuf,
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

    pub(crate) fn connection(&self) -> Result<rusqlite::Connection> {
        db::open_conn(&self.database_path)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PersistedVisualCursor {
    pub(crate) schema_version: u32,
    pub(crate) spec: VisualJobSpec,
    pub(crate) completed_pages: usize,
}

impl PersistedVisualCursor {
    const SCHEMA_VERSION: u32 = 1;

    pub(crate) fn new(spec: VisualJobSpec, completed_pages: usize) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            spec,
            completed_pages,
        }
    }

    pub(crate) fn encode(&self) -> Result<String> {
        serde_json::to_string(self).context("无法序列化视觉任务游标")
    }

    pub(crate) fn decode(value: &str) -> Result<Self> {
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

pub(crate) fn record_from_db(job: db::index_jobs::IndexJob) -> Result<VisualJobRecord> {
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

pub(crate) fn to_db_state(state: VisualJobState) -> db::index_jobs::IndexJobStatus {
    match state {
        VisualJobState::Queued => db::index_jobs::IndexJobStatus::Queued,
        VisualJobState::Running => db::index_jobs::IndexJobStatus::Running,
        VisualJobState::Paused => db::index_jobs::IndexJobStatus::Paused,
        VisualJobState::Succeeded => db::index_jobs::IndexJobStatus::Succeeded,
        VisualJobState::Failed => db::index_jobs::IndexJobStatus::Failed,
        VisualJobState::Cancelled => db::index_jobs::IndexJobStatus::Cancelled,
    }
}

pub(crate) fn from_db_state(state: db::index_jobs::IndexJobStatus) -> VisualJobState {
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

pub(crate) struct CoordinatorCore {
    pub(crate) store: Arc<dyn VisualJobStore>,
    pub(crate) source: Arc<dyn VisualDocumentSource>,
    pub(crate) sink: Arc<dyn VisualPageSink>,
    pub(crate) renderers: HashMap<String, Arc<dyn VisualRenderer>>,
    pub(crate) controls: Mutex<HashMap<String, RenderControl>>,
}

pub(crate) fn record_visual_event(
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
