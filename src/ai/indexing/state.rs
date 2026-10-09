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
    /// 连续请求失败后的冷却时长。生产恒为
    /// `TRANSLATION_REQUEST_FAILURE_COOLDOWN`（60 秒）；单元测试把它调短，免得故意让
    /// 一整本书失败的用例真的睡一分钟。
    pub(crate) failure_cooldown: Duration,
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

    /// 当前模型句柄。一次运行手里握的是**领取时**的快照，拿这一份和它比，就能看出用户
    /// 在这次运行期间是否保存过新的 Endpoint、密钥或生成参数（进程里只有这两个持有者）。
    /// 中毒的锁照旧交出最后发布的值，不因此让运行失败。
    pub(crate) fn models_snapshot(&self) -> ModelServices {
        self.models
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}
