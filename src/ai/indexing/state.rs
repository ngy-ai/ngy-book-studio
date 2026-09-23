//! Shared state the workers read: provider handles, scheduling knobs and
//! the shutdown flag.

use super::*;

#[derive(Clone)]
pub(crate) struct ModelServices {
    pub(crate) embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) vision_provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) config: IndexingModelConfig,
    /// Whole-book translation is optional: until the user picks a default
    /// display language there is nothing to run and no model is held.
    pub(crate) translation: Option<TranslationServices>,
}

/// Live scheduling of the background model executor.
///
/// `concurrency` is how many jobs may run at the same time; `interval` is the
/// pause one worker takes after finishing a job before it claims the next one.
/// Both are published by [`IndexingCoordinator::configure_scheduling`] and read
/// by every worker on each iteration, so a saved settings change applies without
/// restarting the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JobScheduling {
    pub(crate) concurrency: usize,
    pub(crate) interval: Duration,
}

impl Default for JobScheduling {
    fn default() -> Self {
        Self {
            concurrency: crate::services::DEFAULT_BACKGROUND_JOB_CONCURRENCY,
            interval: Duration::from_millis(crate::services::DEFAULT_BACKGROUND_JOB_INTERVAL_MS),
        }
    }
}

#[derive(Clone)]
pub(crate) struct TranslationServices {
    pub(crate) provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) model: String,
    pub(crate) execution_identity: String,
    /// 翻译请求的生成参数（温度、采样、输出上限…）。它随每次请求发送，但**不进执行
    /// 身份**：调它不会让已翻好的译文失效，也不会让任务重跑。
    pub(crate) generation: TranslationGenerationSettings,
    pub(crate) target_language: Option<String>,
    pub(crate) auto_run: bool,
}

pub(crate) struct IndexingInner {
    // Public coordinator futures are also polled by GPUI's foreground
    // executor. Keep the owning Tokio handle instead of relying on an ambient
    // reactor when those futures dispatch SQLite work.
    pub(crate) runtime: Handle,
    pub(crate) db_path: PathBuf,
    pub(crate) blobs: Arc<dyn BlobStore>,
    pub(crate) models: RwLock<ModelServices>,
    /// User-configured concurrency and inter-job pause for the worker pool.
    pub(crate) scheduling: RwLock<JobScheduling>,
    pub(crate) transitions: AsyncMutex<()>,
    pub(crate) wake: Notify,
    /// Set on every successful library mutation so the worker reconciles
    /// missing whole-book translation jobs before going idle again.
    pub(crate) reconcile_translations: AtomicBool,
    pub(crate) shutdown: AtomicBool,
}

impl IndexingInner {
    /// Current scheduling snapshot. A poisoned lock still yields the last
    /// published value instead of stalling every worker.
    pub(crate) fn scheduling(&self) -> JobScheduling {
        *self
            .scheduling
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }
}
