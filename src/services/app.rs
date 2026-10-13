//! Construction of the service graph and the accessors the UI reads.

use super::*;

impl AppServices {
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_credentials(data_dir, Arc::new(SystemCredentialStore))
    }

    pub fn open_with_credentials(
        data_dir: impl Into<PathBuf>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self> {
        let requested_data_dir = data_dir.into();
        let runtime = IoRuntime::default();
        let library =
            LibraryStore::load_from_with_runtime(requested_data_dir.clone(), runtime.clone())?;
        let data_dir = fs::canonicalize(&requested_data_dir).with_context(|| {
            format!(
                "failed to resolve application data directory {}",
                requested_data_dir.display()
            )
        })?;
        let db_path = data_dir.join(db::DATABASE_FILE);
        let learning = Arc::new(crate::learning::LearningService::new(
            data_dir.join("learning"),
            runtime.clone(),
        ));
        let blobs = Arc::new(LocalBlobStore::new(data_dir.join(OBJECT_DIRECTORY))?);
        let formats = Arc::new(FormatRegistry::with_builtin_importers());
        let blob_publication = library.blob_publication_lock();
        let auto_run_background_jobs = library.background_job_auto_run_flag();
        let library = Arc::new(Mutex::new(library));
        let library_generation = Arc::new(AtomicU64::new(0));
        let library_mutations = Arc::new(LibraryMutationQueue::new(&runtime.handle()));
        let chat = ChatRepository::new(db_path.clone(), runtime.clone());
        let settings = load_provider_settings(&db_path)?;
        auto_run_background_jobs.store(settings.auto_run_background_jobs, Ordering::Release);
        let api_keys = load_endpoint_api_keys(&settings, credentials.as_ref())?;
        let endpoint_limits = Arc::new(EndpointRateLimits::new());
        let ai = build_ai_services(&db_path, settings, &api_keys, Arc::clone(&endpoint_limits))?;
        // 保存设置时发布的是列表里**每一条** Endpoint 的上限，启动时补齐同一份口径：
        // 没被任何模型绑定的地址也不该等到下一次保存才有额度条目。
        publish_endpoint_limits(&endpoint_limits, &ai.settings)?;
        let office: Arc<dyn OfficeEnhancer> = Arc::new(OfficeComWorker::start()?);
        let mut visual_renderers: Vec<Arc<dyn VisualRenderer>> =
            vec![Arc::new(StructuralPngRenderer)];
        // DjVu rasterization is portable safe Rust, so it is registered on every
        // target; the Windows-only PDF/Office renderers stay behind `cfg`.
        visual_renderers.push(Arc::new(DjvuPngRenderer));
        #[cfg(target_os = "windows")]
        visual_renderers.push(Arc::new(WindowsPdfRenderer));
        #[cfg(target_os = "windows")]
        visual_renderers.push(Arc::new(OfficeEnhancedRenderer::new(Arc::clone(&office))));
        let visual_job_store = Arc::new(SqliteVisualJobStore::new(&db_path));
        let renderer_descriptors = visual_renderers
            .iter()
            .map(|renderer| renderer.descriptor())
            .collect::<Vec<_>>();
        // Reconcile before starting the model worker so stale SVG pages can
        // never race a requeued vision job during startup. The same gate used
        // by every object publisher spans the reference-removal transaction;
        // stale page bytes are then rechecked and reclaimed before any worker
        // can publish or consume a replacement.
        let reconciliation = {
            let publication_guard = runtime.block_on(blob_publication.acquire());
            let reconciliation = visual_job_store
                .reconcile_registered_renderers(&renderer_descriptors, unix_timestamp()?)?;
            drop(publication_guard);
            reconciliation
        };
        if reconciliation.changed_sources != 0 {
            tracing::info!(
                changed_sources = reconciliation.changed_sources,
                "已按当前 renderer/profile 重建视觉派生任务"
            );
        }
        runtime.block_on(reclaim_unreferenced_blobs(
            &db_path,
            &blobs,
            &blob_publication,
            reconciliation.unreferenced_blobs,
            "过期视觉页面对象",
        ));
        let indexing_blobs: Arc<dyn BlobStore> = blobs.clone();
        let indexing_models = indexing_model_config(&ai.settings)?;
        let indexing = IndexingCoordinator::start(
            runtime.handle(),
            &db_path,
            indexing_blobs,
            Arc::clone(&ai.embedding_provider),
            Arc::clone(&ai.vision_provider),
            indexing_models,
        )?;
        indexing.configure_scheduling(
            ai.settings.background_job_concurrency,
            Duration::from_millis(ai.settings.background_job_interval_ms),
        );
        runtime.block_on(indexing.configure_translation(
            Arc::clone(&ai.translation_provider),
            ai.settings.translation_model.clone(),
            translation_execution_identity(&ai.settings)?,
            ai.settings.translation_generation.clone(),
            ai.settings.default_language.clone(),
            ai.settings.auto_run_background_jobs,
        ))?;

        let source: Arc<dyn VisualDocumentSource> = Arc::new(LibraryVisualDocumentSource {
            library: Arc::clone(&library),
        });
        let sink: Arc<dyn VisualPageSink> = Arc::new(LocalVisualPageSink {
            db_path: db_path.clone(),
            blobs: Arc::clone(&blobs),
            blob_publication: blob_publication.clone(),
            indexing: Arc::clone(&indexing),
        });
        let visual_jobs = VisualJobCoordinator::new(
            runtime.handle(),
            visual_job_store,
            source,
            sink,
            visual_renderers,
        )?;

        Ok(Self {
            data_dir,
            db_path,
            library,
            learning,
            library_generation,
            library_mutations,
            blobs,
            blob_publication,
            formats,
            chat,
            credentials,
            ai: Arc::new(RwLock::new(ai)),
            endpoint_limits,
            ai_configuration: Arc::new(tokio::sync::Mutex::new(())),
            office,
            indexing,
            auto_run_background_jobs,
            visual_jobs: RwLock::new(Some(Arc::new(visual_jobs))),
            visual_renderer_descriptors: RwLock::new(renderer_descriptors),
            runtime,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn learning(&self) -> Arc<crate::learning::LearningService> {
        Arc::clone(&self.learning)
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn runtime(&self) -> IoRuntime {
        self.runtime.clone()
    }

    pub fn blob_store(&self) -> Arc<LocalBlobStore> {
        Arc::clone(&self.blobs)
    }

    pub fn format_registry(&self) -> Arc<FormatRegistry> {
        Arc::clone(&self.formats)
    }

    pub fn chat(&self) -> ChatRepository {
        self.chat.clone()
    }

    /// Returns a current library snapshot for constructing a window. Window
    /// code should still use the service methods for long-running mutations;
    /// this clone is only the existing UI's initial, local view model.
    pub fn library_snapshot(&self) -> Result<LibraryStore> {
        self.library
            .lock()
            .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))
            .map(|library| library.clone())
    }

    /// Monotonic sequence assigned to successful mutations of the shared
    /// in-memory library projection. It lets windows discard a delayed older
    /// completion without ever taking the library mutex on the GPUI thread.
    pub fn library_generation(&self) -> u64 {
        self.library_generation.load(Ordering::Acquire)
    }

    pub fn provider_settings(&self) -> Result<ProviderSettings> {
        Ok(self
            .ai
            .read()
            .map_err(|_| anyhow::anyhow!("AI service lock is poisoned"))?
            .settings
            .clone())
    }
}
