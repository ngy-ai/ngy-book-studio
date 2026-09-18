//! The visual job coordinator and its shutdown hook.

use super::*;

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

    pub(crate) async fn store_control(&self, job_id: &str, pause: bool) -> Result<bool> {
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

    pub(crate) async fn requeue(&self, job_id: &str, retry: bool) -> Result<bool> {
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
