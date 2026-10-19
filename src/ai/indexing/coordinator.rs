//! The coordinator handle: construction, scheduling publication and the
//! per-job control entry points.

use super::*;

/// One bounded process-level executor. It spawns a fixed number of workers and
/// lets at most the configured concurrency claim jobs, which caps
/// memory/network pressure on low-end machines while allowing a deliberate
/// increase on capable ones. Jobs claim their durable row one at a time, so a
/// vision-derived chunk never races the source's embedding pass.
pub struct IndexingCoordinator {
    pub(crate) inner: Arc<IndexingInner>,
    pub(crate) workers: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for IndexingCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndexingCoordinator")
            .field("db_path", &self.inner.db_path)
            .field("shutdown", &self.inner.shutdown.load(Ordering::Acquire))
            .field("max_concurrent_jobs", &self.inner.scheduling().concurrency)
            .finish_non_exhaustive()
    }
}

impl IndexingCoordinator {
    pub fn start(
        runtime: Handle,
        db_path: impl Into<PathBuf>,
        blobs: Arc<dyn BlobStore>,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        vision_provider: Arc<dyn OpenAiCompatibleProvider>,
        config: IndexingModelConfig,
    ) -> Result<Arc<Self>> {
        let db_path = db_path.into();
        let now = unix_timestamp()?;
        let mut startup_conn = db::open_conn(&db_path)?;
        db::index_jobs::recover_interrupted(&startup_conn, now)?;
        // A provider setting may have been durably saved immediately before a
        // crash. Collapse legacy base/model-specific vision rows before the
        // worker starts so only the stable source identity can ever run.
        for source in db::book_sources::list_current(&startup_conn)? {
            db::transactions::reconcile_current_vision_job(
                &mut startup_conn,
                &source,
                &config.vision_model,
                &config.vision_execution_identity,
                now,
            )?;
            db::transactions::reconcile_current_embedding_job(
                &mut startup_conn,
                &source,
                &config.embedding_model,
                &config.embedding_execution_identity,
                &config.vision_execution_identity,
                now,
            )?;
        }

        let coordinator = Arc::new(Self {
            inner: Arc::new(IndexingInner {
                runtime: runtime.clone(),
                db_path,
                blobs,
                models: RwLock::new(ModelServices {
                    embedding_provider,
                    vision_provider,
                    config,
                    translation: None,
                }),
                scheduling: RwLock::new(JobScheduling::default()),
                failure_cooldown: TRANSLATION_REQUEST_FAILURE_COOLDOWN,
                transitions: AsyncMutex::new(()),
                wake: Notify::new(),
                reconcile_translations: AtomicBool::new(false),
                reconcile_progress: Mutex::new(None),
                progress_watch: Mutex::new(ProgressWatch::default()),
                shutdown: AtomicBool::new(false),
            }),
            workers: Mutex::new(Vec::new()),
        });
        {
            let mut workers = coordinator
                .workers
                .lock()
                .map_err(|_| anyhow::anyhow!("indexing worker lock is poisoned"))?;
            // Spawn every slot up front; workers above the configured
            // concurrency stay parked so the setting can change at runtime.
            for index in 0..crate::services::MAX_BACKGROUND_JOB_CONCURRENCY {
                let inner = Arc::clone(&coordinator.inner);
                workers.push(runtime.spawn(async move { worker_loop(inner, index).await }));
            }
            // 静默看门狗独立于 job worker：所有槽位都在跑任务（或 concurrency 设成 1
            // 并且那一个 worker 卡住）时，它仍按自己的节奏把「已领取但无进展」写进任务
            // 日志。它与 worker 一样随协调器一起中止。
            let inner = Arc::clone(&coordinator.inner);
            workers.push(runtime.spawn(async move { watchdog_loop(inner).await }));
        }
        Ok(coordinator)
    }

    /// Applies the configured background-job concurrency and inter-job pause.
    /// Both values are clamped to the supported range so a damaged settings row
    /// can never stall the worker or start unbounded model calls.
    pub fn configure_scheduling(&self, concurrency: usize, interval: Duration) {
        let next = JobScheduling {
            concurrency: concurrency.clamp(
                crate::services::MIN_BACKGROUND_JOB_CONCURRENCY,
                crate::services::MAX_BACKGROUND_JOB_CONCURRENCY,
            ),
            interval: interval.min(Duration::from_millis(
                crate::services::MAX_BACKGROUND_JOB_INTERVAL_MS,
            )),
        };
        *self
            .inner
            .scheduling
            .write()
            .unwrap_or_else(|error| error.into_inner()) = next;
        self.inner.scheduling.clear_poison();
        // Parked workers poll once per idle interval, so this only shortens the
        // wait for an already running worker picking up new work.
        self.inner.wake.notify_one();
    }

    pub fn wake(&self) {
        self.inner
            .reconcile_translations
            .store(true, Ordering::Release);
        self.inner.wake.notify_one();
    }

    pub async fn job(&self, job_id: &str) -> Result<Option<IndexingJobSnapshot>> {
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            db::index_jobs::get(&conn, &job_id)
        })
        .await
        .map(|job| job.map(snapshot_from_row))
    }

    pub async fn jobs_for_book(&self, book_id: &str) -> Result<Vec<IndexingJobSnapshot>> {
        let book_id = book_id.to_string();
        let db_path = self.inner.db_path.clone();
        run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            db::index_jobs::list_for_book(&conn, &book_id)
        })
        .await
        .map(|jobs| jobs.into_iter().map(snapshot_from_row).collect())
    }

    pub async fn pause(&self, job_id: &str) -> Result<bool> {
        self.request_control(job_id, ControlRequest::Pause).await
    }

    pub async fn cancel(&self, job_id: &str) -> Result<bool> {
        self.request_control(job_id, ControlRequest::Cancel).await
    }

    pub async fn resume(&self, job_id: &str) -> Result<bool> {
        let _transition = self.inner.transitions.lock().await;
        let diagnostic_id = job_id.to_string();
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let now = unix_timestamp()?;
        let changed = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            db::index_jobs::resume(&conn, &job_id, now)
        })
        .await?
            == 1;
        if changed {
            record_for_database(
                &self.inner.db_path,
                &diagnostic_id,
                JobLogEvent::ResumeRequested,
                JobLogMetrics::default(),
            );
            self.wake();
        }
        Ok(changed)
    }

    pub async fn retry(&self, job_id: &str) -> Result<bool> {
        let _transition = self.inner.transitions.lock().await;
        let diagnostic_id = job_id.to_string();
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let (config, translation) = {
            let models = self
                .inner
                .models
                .read()
                .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?;
            (
                models.config.clone(),
                models.translation.as_ref().map(|translation| {
                    (
                        translation.model.clone(),
                        translation.execution_identity.clone(),
                    )
                }),
            )
        };
        let now = unix_timestamp()?;
        let changed = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let Some(job) = db::index_jobs::get(&conn, &job_id)? else {
                return Ok(0);
            };
            if job.kind == "vision"
                && !vision_job_matches_current_execution(
                    &conn,
                    &job,
                    &config.vision_model,
                    &config.vision_execution_identity,
                )?
            {
                return Ok(0);
            }
            if job.kind == "embedding"
                && !embedding_job_matches_current_execution(
                    &conn,
                    &job,
                    &config.embedding_model,
                    &config.embedding_execution_identity,
                    &config.vision_execution_identity,
                )?
            {
                return Ok(0);
            }
            if job.kind == TRANSLATION_JOB_KIND {
                // 取消/失败可能把一次没跑完的收窄留在游标里：重试必须从当前推进点继续
                // 整本，不能再沿用那份范围与「命中缓存也重发」。清掉之后游标之前（已处理）
                // 的块根本不进入 walk，范围内已译的块按缓存命中推进，都不会再花请求。
                let mut cursor: JobCursor =
                    serde_json::from_str(&job.cursor_json).context("索引任务游标无效")?;
                let stored = cursor.encode()?;
                cursor.retry_ordinals.clear();
                cursor.force_retry = false;
                cursor.resume_ordinal = None;
                cursor.stops_after_targets = false;
                if let Some((model, execution_identity)) = &translation {
                    cursor.model = Some(model.clone());
                    cursor.execution_identity = Some(execution_identity.clone());
                }
                let cursor_json = cursor.encode()?;
                if cursor_json != stored {
                    return db::index_jobs::retry_translation(&conn, &job_id, &cursor_json, now);
                }
            }
            db::index_jobs::retry(&conn, &job_id, now)
        })
        .await?
            == 1;
        if changed {
            record_for_database(
                &self.inner.db_path,
                &diagnostic_id,
                JobLogEvent::RetryRequested,
                JobLogMetrics::default(),
            );
            self.wake();
        }
        Ok(changed)
    }

    /// Called after an atomic visual-page publication. Jobs that previously
    /// failed only because screenshots did not yet exist become runnable.
    pub async fn visual_pages_ready(&self, source_id: &str) -> Result<usize> {
        let _transition = self.inner.transitions.lock().await;
        let source_id = source_id.to_string();
        let db_path = self.inner.db_path.clone();
        let now = unix_timestamp()?;
        let changed = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            db::index_jobs::retry_failed_for_source_kind(&conn, &source_id, "vision", now)
        })
        .await?;
        if changed != 0 {
            self.wake();
        }
        Ok(changed)
    }

    /// Replaces provider/model handles without touching canonical documents.
    /// An unchanged embedding endpoint/model identity leaves every existing
    /// embedding row untouched. Vision work retains one stable per-source row:
    /// an unchanged canonical endpoint/model identity does not restart it,
    /// while a changed identity atomically supersedes its former execution and
    /// clears model-derived page descriptions.
    pub async fn reconfigure(
        &self,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        vision_provider: Arc<dyn OpenAiCompatibleProvider>,
        config: IndexingModelConfig,
    ) -> Result<()> {
        let _transition = self.inner.transitions.lock().await;
        let embedding_execution_changed = {
            let models = self
                .inner
                .models
                .read()
                .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?;
            models.config.embedding_execution_identity != config.embedding_execution_identity
        };
        let db_path = self.inner.db_path.clone();
        let db_config = config.clone();
        run_db_on(self.inner.runtime.clone(), db_path, move |mut conn| {
            let now = unix_timestamp()?;
            db::transactions::reconfigure_current_index_jobs(
                &mut conn,
                &db_config.embedding_model,
                &db_config.embedding_execution_identity,
                embedding_execution_changed,
                &db_config.vision_model,
                &db_config.vision_execution_identity,
                now,
            )
        })
        .await?;

        // The database transaction is now the durable commit point. Publishing
        // the matching process-local handles must therefore be infallible;
        // recover the complete value from an unlikely poisoned lock instead
        // of reporting failure after the database has already committed.
        let mut models = self
            .inner
            .models
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let translation = models.translation.clone();
        *models = ModelServices {
            embedding_provider,
            vision_provider,
            config,
            translation,
        };
        drop(models);
        self.inner.models.clear_poison();
        self.wake();
        Ok(())
    }
}
impl Drop for IndexingCoordinator {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Release);
        self.inner.wake.notify_waiters();
        if let Ok(workers) = self.workers.get_mut() {
            for worker in workers.drain(..) {
                worker.abort();
            }
        }
    }
}
