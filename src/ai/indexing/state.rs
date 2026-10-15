//! Shared state the workers read: provider handles, scheduling knobs and
//! the shutdown flag.

use super::*;

use std::collections::VecDeque;

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
    /// 分步对账的剩余单位。worker 一步取一个；取空即这一轮对账完成。
    ///
    /// 计划在第一次推进时按当时的配置构建，之后只在库/配置变化
    /// （`reconcile_translations` 再次置位）时丢弃重建 —— 已处理的单位因为行已经对齐
    /// 不会重新出现在新计划里。
    pub(crate) reconcile_progress: Mutex<Option<TranslationReconcileProgress>>,
    /// 「已领取但无进展」看门狗读的最近一次进展（任何任务日志事件都算）。
    pub(crate) progress_watch: Mutex<ProgressWatch>,
    pub(crate) shutdown: AtomicBool,
}

/// 一轮分步对账的剩余单位，以及「哪些排队任务是自己刚产出的」。
#[derive(Debug, Default)]
pub(crate) struct TranslationReconcileProgress {
    units: VecDeque<db::transactions::TranslationReconcileUnit>,
    /// 本计划会触碰的任务 id（插入的、要重置的、要取消的）。
    ///
    /// 它们出现在队列里不算「外部工作」，不给它们让路 —— 否则对账会被自己刚插入
    /// 的任务拖住：那个任务排在队首，worker 会去翻整本书，剩下的单位要等它跑完。
    own_ids: HashSet<String>,
}

impl TranslationReconcileProgress {
    fn new(units: Vec<db::transactions::TranslationReconcileUnit>) -> Self {
        let own_ids = units.iter().map(|unit| unit.job_id().to_string()).collect();
        Self {
            units: units.into(),
            own_ids,
        }
    }

    fn owns(&self, job_id: &str) -> bool {
        self.own_ids.contains(job_id)
    }
}

/// 后台任务最近一次进展。看门狗据此把「领取后长期没有动静」的任务写进任务日志。
#[derive(Debug, Default)]
pub(crate) struct ProgressWatch {
    job_id: Option<String>,
    last_event_at: Option<Instant>,
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

    /// 记一次任务进展：任何任务日志事件都算（见 `record_index_event`）。
    ///
    /// 看门狗把这条时间戳当作「任务还活着」的证据；它自己写的事件同样会经过这里，
    /// 因此「已领取但无进展」每个阈值周期最多报一次，而不是每次检查都刷一条。
    pub(crate) fn note_progress(&self, job_id: &str, at: Instant) {
        let mut watch = self
            .progress_watch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        watch.job_id = Some(job_id.to_string());
        watch.last_event_at = Some(at);
    }

    /// 当前「已领取但没有新事件」的任务与时长；还没有任何进展记录时为 `None`。
    pub(crate) fn progress_idle(&self) -> Option<(String, Duration)> {
        let watch = self
            .progress_watch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let job_id = watch.job_id.clone()?;
        let elapsed = Instant::now().saturating_duration_since(watch.last_event_at?);
        Some((job_id, elapsed))
    }

    pub(crate) fn clear_translation_reconcile(&self) {
        *self
            .reconcile_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    pub(crate) fn has_translation_reconcile(&self) -> bool {
        self.reconcile_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    pub(crate) fn set_translation_reconcile(
        &self,
        units: Vec<db::transactions::TranslationReconcileUnit>,
    ) {
        *self
            .reconcile_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some(TranslationReconcileProgress::new(units));
    }

    /// 待推进的计划里是否有这条任务（本计划自己的产出）。
    pub(crate) fn reconcile_plan_owns(&self, job_id: &str) -> bool {
        self.reconcile_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|progress| progress.owns(job_id))
    }

    /// 取出下一个对账单位；取尽即认为这一轮对账完成（计划随之清空，下次按最新状态
    /// 重新构建）。
    pub(crate) fn take_translation_reconcile_unit(
        &self,
    ) -> Option<db::transactions::TranslationReconcileUnit> {
        let mut progress = self
            .reconcile_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let unit = progress.as_mut()?.units.pop_front();
        if unit.is_none() {
            *progress = None;
        }
        unit
    }
}
