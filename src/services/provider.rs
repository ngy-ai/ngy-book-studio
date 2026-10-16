//! Provider configuration, endpoint routing and model probing.

use super::*;
use crate::speech::SpeechRequest;

impl AppServices {
    pub fn provider(&self) -> Result<Arc<dyn OpenAiCompatibleProvider>> {
        Ok(Arc::clone(
            &self
                .ai
                .read()
                .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
                .provider,
        ))
    }

    pub(crate) fn ai_request_snapshot(&self) -> Result<AiRequestSnapshot> {
        let ai = self
            .ai
            .read()
            .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?;
        Ok(AiRequestSnapshot {
            settings: ai.settings.clone(),
            provider: Arc::clone(&ai.provider),
            search: Arc::clone(&ai.search),
        })
    }

    /// Builds the host web-search backend from the persisted configuration, or
    /// `None` when the fallback is disabled. The API key is read from the
    /// credential store, never from persisted JSON.
    pub fn web_search_backend(&self) -> Result<Option<Arc<dyn WebSearchBackend>>> {
        let settings = self.provider_settings()?;
        let api_key = if settings.web_search_enabled {
            self.credentials.api_key(WEB_SEARCH_CREDENTIAL_TARGET)?
        } else {
            None
        };
        match settings.web_search_config(api_key)? {
            Some(config) => Ok(Some(Arc::new(HttpWebSearch::new_with_proxy(
                config,
                settings.use_proxy,
            )?))),
            None => Ok(None),
        }
    }

    pub fn web_search_api_key(&self) -> Result<Option<String>> {
        self.credentials.api_key(WEB_SEARCH_CREDENTIAL_TARGET)
    }

    pub fn set_web_search_api_key(&self, key: &str) -> Result<()> {
        self.credentials
            .set_api_key(WEB_SEARCH_CREDENTIAL_TARGET, key)
    }

    pub fn delete_web_search_api_key(&self) -> Result<()> {
        self.credentials
            .delete_api_key(WEB_SEARCH_CREDENTIAL_TARGET)
    }

    pub fn search(&self) -> Result<Arc<SearchService>> {
        Ok(Arc::clone(
            &self
                .ai
                .read()
                .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
                .search,
        ))
    }

    pub fn office_enhancer(&self) -> Arc<dyn OfficeEnhancer> {
        Arc::clone(&self.office)
    }

    pub fn indexing(&self) -> Arc<IndexingCoordinator> {
        Arc::clone(&self.indexing)
    }

    /// 各 Endpoint 的请求额度表。它按 `base_url` 在进程里共享，设置保存与模型检测都
    /// 经过它；诊断与测试用它确认上限保存之后到底生效了没有。
    pub fn endpoint_limits(&self) -> Arc<EndpointRateLimits> {
        Arc::clone(&self.endpoint_limits)
    }

    pub fn visual_jobs(&self) -> Option<Arc<VisualJobCoordinator>> {
        self.visual_jobs
            .read()
            .ok()
            .and_then(|coordinator| coordinator.as_ref().map(Arc::clone))
    }

    /// Runs synchronous LibraryStore work away from GPUI and Tokio core
    /// workers. The returned handle belongs to the dedicated application
    /// runtime and may be awaited from a GPUI task.
    ///
    /// The mutation is logged stage by stage under the `ngy_library` target
    /// (queue ticket, admission, shared lock, commit) so a request that never
    /// finishes is still attributable to the stage it is stuck in: the last
    /// line records the wait that never returned.
    pub fn spawn_library<T, F>(&self, operation: F) -> tokio::task::JoinHandle<Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(&mut LibraryStore) -> Result<T> + Send + 'static,
    {
        let library = Arc::clone(&self.library);
        let library_generation = Arc::clone(&self.library_generation);
        let mutation_turn = self.library_mutations.reserve();
        let ticket = mutation_turn.ticket;
        let indexing = Arc::clone(&self.indexing);
        tracing::debug!(target: "ngy_library", ticket, "书库写入：已排队，等待写入许可");
        self.runtime.handle().spawn(async move {
            let queued = std::time::Instant::now();
            let turn = mutation_turn.enter().await?;
            tracing::debug!(
                target: "ngy_library",
                ticket,
                waited_ms = queued.elapsed().as_millis() as u64,
                "书库写入：已获准进入，等待共享书库锁"
            );
            tokio::task::spawn_blocking(move || {
                let _turn = turn;
                let waiting = std::time::Instant::now();
                let mut library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                tracing::debug!(
                    target: "ngy_library",
                    ticket,
                    waited_ms = waiting.elapsed().as_millis() as u64,
                    "书库写入：已取得共享书库锁，开始执行"
                );
                let started = std::time::Instant::now();
                let result = operation(&mut library);
                match &result {
                    Ok(_) => tracing::debug!(
                        target: "ngy_library",
                        ticket,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "书库写入：已提交"
                    ),
                    Err(error) => tracing::warn!(
                        target: "ngy_library",
                        ticket,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        error = %format!("{error:#}"),
                        "书库写入：已失败"
                    ),
                }
                if result.is_ok() {
                    library_generation.fetch_add(1, Ordering::AcqRel);
                    indexing.wake();
                }
                result
            })
            .await
            .context("library mutation worker stopped")?
        })
    }

    /// Runs a mutation and captures the resulting projection while the same
    /// mutex guard is still held. The generation reflects commit order, so a
    /// UI can merge only monotonically newer completions even when async tasks
    /// finish their GPUI callbacks out of order.
    ///
    /// Logging matches [`Self::spawn_library`]: each wait logs once before and
    /// once after it returns.
    pub fn spawn_library_projected<T, F>(
        &self,
        operation: F,
    ) -> tokio::task::JoinHandle<Result<LibraryMutation<T>>>
    where
        T: Send + 'static,
        F: FnOnce(&mut LibraryStore) -> Result<T> + Send + 'static,
    {
        let library = Arc::clone(&self.library);
        let library_generation = Arc::clone(&self.library_generation);
        let mutation_turn = self.library_mutations.reserve();
        let ticket = mutation_turn.ticket;
        let indexing = Arc::clone(&self.indexing);
        tracing::debug!(target: "ngy_library", ticket, "书库写入：已排队，等待写入许可");
        self.runtime.handle().spawn(async move {
            let queued = std::time::Instant::now();
            let turn = mutation_turn.enter().await?;
            tracing::debug!(
                target: "ngy_library",
                ticket,
                waited_ms = queued.elapsed().as_millis() as u64,
                "书库写入：已获准进入，等待共享书库锁"
            );
            tokio::task::spawn_blocking(move || {
                let _turn = turn;
                let waiting = std::time::Instant::now();
                let mut library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                tracing::debug!(
                    target: "ngy_library",
                    ticket,
                    waited_ms = waiting.elapsed().as_millis() as u64,
                    "书库写入：已取得共享书库锁，开始执行"
                );
                let started = std::time::Instant::now();
                let value = match operation(&mut library) {
                    Ok(value) => {
                        tracing::debug!(
                            target: "ngy_library",
                            ticket,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "书库写入：已提交"
                        );
                        value
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "ngy_library",
                            ticket,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            error = %format!("{error:#}"),
                            "书库写入：已失败"
                        );
                        return Err(error);
                    }
                };
                let generation = library_generation.fetch_add(1, Ordering::AcqRel) + 1;
                let snapshot = library.clone();
                drop(library);
                indexing.wake();
                Ok(LibraryMutation {
                    value,
                    snapshot,
                    generation,
                })
            })
            .await
            .context("projected library mutation worker stopped")?
        })
    }

    /// Runs a read-only LibraryStore operation away from GPUI and Tokio core
    /// workers. The in-memory projection is cloned before the operation so a
    /// long media read cannot hold the mutation lock. Unlike
    /// [`Self::spawn_library`], successful reads do not wake the indexing
    /// coordinator; WebView range requests therefore cannot turn playback
    /// into an indexing wake storm.
    pub fn spawn_library_read<T, F>(&self, operation: F) -> tokio::task::JoinHandle<Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(&LibraryStore) -> Result<T> + Send + 'static,
    {
        let library = Arc::clone(&self.library);
        self.runtime.handle().spawn_blocking(move || {
            let library = library
                .lock()
                .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?
                .clone();
            operation(&library)
        })
    }

    /// Updates the default Endpoint key while preserving every other key.
    pub async fn configure_provider(
        &self,
        settings: ProviderSettings,
        api_key: ApiKeyUpdate,
    ) -> Result<()> {
        self.configure_providers(
            settings,
            BTreeMap::from([(DEFAULT_ENDPOINT_ID.into(), api_key)]),
        )
        .await
    }

    /// Saves all Endpoint and role choices as one serialized transition. Once
    /// accepted, the runtime completes or rolls back even if the UI is closed.
    pub async fn configure_providers(
        &self,
        settings: ProviderSettings,
        api_keys: BTreeMap<String, ApiKeyUpdate>,
    ) -> Result<()> {
        settings.validate()?;
        for (id, update) in &api_keys {
            settings.endpoint_by_id(id)?;
            if let ApiKeyUpdate::Set(value) = update {
                ensure!(!value.is_empty(), "API key cannot be empty");
            }
        }
        let indexing_models = indexing_model_config(&settings)?;
        let translation_identity = translation_execution_identity(&settings)?;
        let db_path = self.db_path.clone();
        let credentials = Arc::clone(&self.credentials);
        let ai = Arc::clone(&self.ai);
        let endpoint_limits = Arc::clone(&self.endpoint_limits);
        let configuration = Arc::clone(&self.ai_configuration);
        let indexing = Arc::clone(&self.indexing);
        let auto_run = Arc::clone(&self.auto_run_background_jobs);
        self.runtime
            .spawn(async move {
                // 这一层超时只包住「等门闩」，不包住后面的写入：门闩里的重配置与
                // 回滚必须完整跑完（那是「要么提交、要么回滚」的保证），所以不能
                // 用超时把工作 future 整个丢掉。
                let _transition = match tokio::time::timeout(
                    AI_SETTINGS_GATE_TIMEOUT,
                    configuration.lock(),
                )
                .await
                {
                    Ok(transition) => transition,
                    Err(_) => {
                        anyhow::bail!(
                            "另一个设置保存仍在进行中（已等待 {} 秒）：它可能正被后台任务阻塞，稍后重试",
                            AI_SETTINGS_GATE_TIMEOUT.as_secs()
                        )
                    }
                };
                let previous_settings = ai
                    .read()
                    .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
                    .settings
                    .clone();
                let worker_db = db_path.clone();
                let worker_credentials = Arc::clone(&credentials);
                let worker_limits = Arc::clone(&endpoint_limits);
                let (next, previous_rows, previous_keys) = tokio::task::spawn_blocking(move || {
                    let previous_rows = snapshot_ai_settings(&worker_db)?;
                    let mut next_keys =
                        load_endpoint_api_keys(&settings, worker_credentials.as_ref())?;
                    let mut changes = BTreeMap::new();
                    for endpoint in settings.endpoints() {
                        let target = normalize_provider_base_url(&endpoint.base_url)?.to_string();
                        let update = api_keys.get(&endpoint.id).unwrap_or(&ApiKeyUpdate::Keep);
                        match update {
                            ApiKeyUpdate::Keep => {}
                            ApiKeyUpdate::Set(value) => {
                                next_keys.insert(endpoint.id, Some(value.clone()));
                                changes.insert(target, Some(value.clone()));
                            }
                            ApiKeyUpdate::Delete => {
                                next_keys.insert(endpoint.id, None);
                                changes.insert(target, None);
                            }
                        }
                    }
                    let next_urls = settings
                        .endpoints()
                        .into_iter()
                        .map(|endpoint| {
                            normalize_provider_base_url(&endpoint.base_url)
                                .map(|url| url.to_string())
                        })
                        .collect::<Result<BTreeSet<_>>>()?;
                    for endpoint in previous_settings.endpoints() {
                        let target = normalize_provider_base_url(&endpoint.base_url)?.to_string();
                        if !next_urls.contains(&target) {
                            changes.insert(target, None);
                        }
                    }
                    // Construct every role before mutating durable settings or keys.
                    let services = build_ai_services(
                        &worker_db,
                        settings.clone(),
                        &next_keys,
                        Arc::clone(&worker_limits),
                    )?;
                    let previous_keys = changes
                        .keys()
                        .map(|target| {
                            worker_credentials
                                .api_key(target)
                                .map(|key| (target.clone(), key))
                        })
                        .collect::<Result<BTreeMap<_, _>>>()?;
                    let mutation = (|| {
                        restore_endpoint_keys(worker_credentials.as_ref(), &changes)?;
                        save_provider_settings(&worker_db, &settings)
                    })();
                    if let Err(error) = mutation {
                        if let Err(rollback_error) =
                            restore_endpoint_keys(worker_credentials.as_ref(), &previous_keys)
                        {
                            return Err(error.context(format!(
                                "credential rollback also failed: {rollback_error:#}"
                            )));
                        }
                        return Err(error);
                    }
                    Ok((services, previous_rows, previous_keys))
                })
                .await
                .context("AI settings worker stopped")??;

                let reconfigure_result = match indexing
                    .reconfigure(
                        Arc::clone(&next.embedding_provider),
                        Arc::clone(&next.vision_provider),
                        indexing_models,
                    )
                    .await
                    .context("failed to reconfigure derived indexing")
                {
                    Ok(()) => indexing
                        .configure_translation(
                            Arc::clone(&next.translation_provider),
                            next.settings.translation_model.clone(),
                            translation_identity,
                            next.settings.translation_generation.clone(),
                            next.settings.resolved_translation_blocks_per_request()?,
                            next.settings.default_language.clone(),
                            next.settings.auto_run_background_jobs,
                        )
                        .await
                        .context("failed to reconfigure book translation"),
                    Err(error) => Err(error),
                };
                if let Err(error) = reconfigure_result {
                    let rollback = tokio::task::spawn_blocking(move || {
                        let settings_result = restore_ai_settings(&db_path, &previous_rows);
                        let credential_result =
                            restore_endpoint_keys(credentials.as_ref(), &previous_keys);
                        match (settings_result, credential_result) {
                            (Ok(()), Ok(())) => Ok(()),
                            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                            (Err(settings_error), Err(credential_error)) => Err(settings_error
                                .context(format!(
                                    "credential rollback also failed: {credential_error:#}"
                                ))),
                        }
                    })
                    .await
                    .context("AI settings rollback worker stopped")?;
                    if let Err(rollback_error) = rollback {
                        return Err(error.context(format!(
                            "persisted AI settings rollback also failed: {rollback_error:#}"
                        )));
                    }
                    return Err(error);
                }

                auto_run.store(next.settings.auto_run_background_jobs, Ordering::Release);
                indexing.configure_scheduling(
                    next.settings.background_job_concurrency,
                    Duration::from_millis(next.settings.background_job_interval_ms),
                );
                // 请求上限与调度、自动运行开关一样是内存里的即时发布，放在提交点之后：
                // 提交失败回滚之后，生效中的限速不能是新草稿的那一份。地址在这里不可能
                // 解析失败（`settings.validate()` 在进入这个任务之前就跑过），所以走不通
                // 只可能是内部状态坏了 —— 记一条告警，不让已经提交的设置变成一次失败。
                if let Err(error) = publish_endpoint_limits(&endpoint_limits, &next.settings) {
                    tracing::warn!(
                        target: "ngy_ai",
                        error = %format!("{error:#}"),
                        "Endpoint 限速设置未能应用到内存"
                    );
                }
                *ai.write().unwrap_or_else(|error| error.into_inner()) = next;
                ai.clear_poison();
                Ok(())
            })
            .await
            .context("AI configuration task stopped")?
    }

    /// 保存朗读语速这项应用级偏好，供阅读窗口工具栏的语速下拉使用。
    ///
    /// 这是一条**只写一行**的路径：改语速既不需要重建 provider，也不需要重配置索引，
    /// 走 [`Self::configure_providers`] 只会为了一个数字去等索引门闩。两条写路径共用
    /// 同一把设置门闩，串行化之后的语义是「最后保存者赢」——与各设置窗口之间已有的
    /// 约定一致。提交成功后更新内存快照：新的朗读会话读的就是它（正在播放的会话不受
    /// 影响）。
    pub async fn set_speech_rate(&self, rate: f32) -> Result<()> {
        let rate = rate.clamp(SpeechRequest::RATE_MIN, SpeechRequest::RATE_MAX);
        let _transition = self.ai_configuration.lock().await;
        let voice_id = self
            .ai
            .read()
            .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
            .settings
            .speech_voice_id
            .clone();
        let db_path = self.db_path.clone();
        // 用应用 runtime 的 handle 而不是 ambient Tokio：调用方可能像 GPUI 前台
        // 执行器一样不带 Tokio 上下文（服务层测试也用 `block_on_without_tokio` 盯住
        // 这条约束）。
        self.runtime
            .handle()
            .spawn_blocking(move || save_speech_preferences(&db_path, &voice_id, rate))
            .await
            .context("朗读设置写入 worker 停止")??;
        self.ai
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .settings
            .speech_rate = rate;
        self.ai.clear_poison();
        Ok(())
    }

    /// Probes only the default Endpoint, independently of model or web drafts.
    pub async fn probe_provider_models(
        &self,
        settings: ProviderSettings,
        api_key: ApiKeyUpdate,
    ) -> Result<Vec<ModelInfo>> {
        self.probe_endpoint_models(settings.endpoint_by_id(DEFAULT_ENDPOINT_ID)?, api_key)
            .await
    }

    /// Queries an existing or unsaved Endpoint without persisting it. Other
    /// Endpoint, model and web-search drafts do not affect this operation.
    pub async fn probe_endpoint_models(
        &self,
        endpoint: EndpointSettings,
        api_key: ApiKeyUpdate,
    ) -> Result<Vec<ModelInfo>> {
        let credentials = Arc::clone(&self.credentials);
        let limits = Arc::clone(&self.endpoint_limits);
        // Probing an endpoint is a network call like any other, so it respects
        // the system proxy preference of the saved settings.
        let saved = self.provider_settings()?;
        let use_proxy = saved.use_proxy;
        // 检测用的是草稿里的配置，但草稿不该顺手改掉正在生效的限速：地址已经在已保存的
        // 列表里就沿用它那份上限，只有还没保存过的新地址才按草稿建立额度条目。
        let requests_per_minute = normalize_provider_base_url(&endpoint.base_url)
            .ok()
            .and_then(|draft| {
                saved.endpoints().into_iter().find(|saved_endpoint| {
                    normalize_provider_base_url(&saved_endpoint.base_url)
                        .is_ok_and(|url| url == draft)
                })
            })
            .map_or_else(
                || endpoint.resolved_requests_per_minute(),
                |saved_endpoint| saved_endpoint.resolved_requests_per_minute(),
            );
        let provider = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                endpoint.validate()?;
                let target = normalize_provider_base_url(&endpoint.base_url)?;
                let api_key = match api_key {
                    ApiKeyUpdate::Keep => credentials.api_key(target.as_str())?,
                    ApiKeyUpdate::Set(value) => {
                        ensure!(!value.is_empty(), "API key cannot be empty");
                        Some(value)
                    }
                    ApiKeyUpdate::Delete => None,
                };
                let budget = EndpointBudget::new(
                    limits.slot(&endpoint.base_url, requests_per_minute)?,
                    // 检测是用户点一下、正等着看结果的请求，与问答同一类。
                    RequestClass::Interactive,
                );
                Ok(OpenAiHttpProvider::new_with_proxy(
                    endpoint.provider_config(api_key),
                    use_proxy,
                )?
                .with_endpoint_budget(budget))
            })
            .await
            .context("AI provider probe setup worker stopped")??;
        self.runtime
            .spawn(async move { provider.models().await })
            .await
            .context("AI model probe worker stopped")?
    }
}
