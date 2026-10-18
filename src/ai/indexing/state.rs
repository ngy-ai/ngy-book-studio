//! Shared state the workers read: provider handles, scheduling knobs and
//! the shutdown flag.

use super::*;

use std::collections::{HashMap, VecDeque};

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
    /// 这个端点「每请求文本块数」的**解析结果**：`1` 表示一块一请求（本机端点的自动
    /// 档，或用户显式关掉），`> 1` 表示一次请求最多带走这么多块。它随每次装配重新
    /// 解析（`batch_block_limit`），所以改设置、换端点都即时生效，且**不进执行身份**：
    /// 打包方式不改变译文内容与落库形状，已翻好的块不会因此失效。
    ///
    /// 它必须始终是一个合法值：装配方没有可用端点配置时给 `1`（一块一请求 = 老行为）。
    pub(crate) batch_blocks: usize,
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
///
/// **每个被观察的任务各占一格**（`job_id -> 最近事件时间`）。曾经这里只有一个槽，
/// 任意任务的事件都会覆写它，于是 `progress_idle()` 只反映「最近写日志的那个任务」：
/// 并发大于 1 时，别的任务就算彻底静默也永远不会被报告——看门狗形同虚设
/// （现场 2026-10-18：并发 8 时只有最后写事件的那一个任务可被观察）。
#[derive(Debug, Default)]
pub(crate) struct ProgressWatch {
    entries: HashMap<String, Instant>,
}

impl ProgressWatch {
    /// 记一次进展；同一任务重复出现只保留最新时间戳。
    fn note(&mut self, job_id: &str, at: Instant) {
        self.entries.insert(job_id.to_string(), at);
    }

    /// 忘记一个任务：它的行已经不在运行态（收尾、被替换、被删）之后不再观察，
    /// 条目随之清理，观察表只随「还在跑的任务」增长。
    fn forget(&mut self, job_id: &str) {
        self.entries.remove(job_id);
    }

    /// 当前所有「已领取但没有新事件」且超过 `threshold` 的任务与无进展时长。
    ///
    /// 并发下每个静默的任务都要报，所以返回列表而不是单个。
    fn stalled(&self, now: Instant, threshold: Duration) -> Vec<(String, Duration)> {
        self.entries
            .iter()
            .filter_map(|(job_id, at)| {
                let elapsed = now.saturating_duration_since(*at);
                (elapsed >= threshold).then(|| (job_id.clone(), elapsed))
            })
            .collect()
    }
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
        watch.note(job_id, at);
    }

    /// 停止观察一个任务（它的行已经不在运行态）。看门狗每周期对不在运行态的格子
    /// 调用它，观察表因此不会随历史任务无限增长。
    pub(crate) fn forget_progress(&self, job_id: &str) {
        let mut watch = self
            .progress_watch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        watch.forget(job_id);
    }

    /// 当前所有超过 `threshold` 没有任何新事件（进展）的任务与时长；还没有任何
    /// 进展记录时为空。
    pub(crate) fn stalled_jobs(&self, threshold: Duration) -> Vec<(String, Duration)> {
        let watch = self
            .progress_watch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        watch.stalled(Instant::now(), threshold)
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
