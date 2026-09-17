//! The coordinator that claims visual jobs and drives them to completion.

use super::*;

pub struct VisualJobCoordinator {
    pub(crate) core: Arc<CoordinatorCore>,
    pub(crate) sender: async_channel::Sender<String>,
    pub(crate) runtime: Handle,
    pub(crate) worker: JoinHandle<()>,
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

pub(crate) async fn process_visual_job(core: Arc<CoordinatorCore>, job_id: &str) -> Result<()> {
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

pub(crate) async fn execute_visual_job(
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

pub(crate) fn validate_rendered_page(
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

pub(crate) async fn blocking_get(
    store: Arc<dyn VisualJobStore>,
    job_id: &str,
) -> Result<Option<VisualJobRecord>> {
    let job_id = job_id.to_string();
    tokio::task::spawn_blocking(move || store.get(&job_id))
        .await
        .context("视觉任务查询线程异常退出")?
}

pub(crate) async fn blocking_transition(
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

pub(crate) fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix epoch")
        .map(|duration| duration.as_secs())
}

pub(crate) fn truncate_error(error: &str) -> String {
    const MAX_ERROR_CHARS: usize = 4_096;
    if error.chars().count() <= MAX_ERROR_CHARS {
        error.to_string()
    } else {
        error.chars().take(MAX_ERROR_CHARS).collect()
    }
}
