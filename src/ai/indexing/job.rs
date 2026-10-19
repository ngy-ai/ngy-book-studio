//! Durable job vocabulary: status, projection, cursor and run outcome.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexingJobStatus {
    Queued,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexingJobSnapshot {
    pub id: String,
    pub book_id: String,
    pub source_id: Option<String>,
    pub kind: String,
    pub status: IndexingJobStatus,
    pub attempts: u32,
    pub cursor_json: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexingModelConfig {
    pub(crate) embedding_model: String,
    pub(crate) embedding_dimensions: usize,
    pub(crate) embedding_execution_identity: String,
    pub(crate) vision_model: String,
    pub(crate) vision_execution_identity: String,
}

impl IndexingModelConfig {
    pub fn new(
        embedding_model: impl Into<String>,
        embedding_dimensions: usize,
        embedding_execution_identity: impl Into<String>,
        vision_model: impl Into<String>,
        vision_execution_identity: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            embedding_model: validated_model(embedding_model.into(), "embedding")?,
            embedding_dimensions,
            embedding_execution_identity: validated_execution_identity(
                embedding_execution_identity.into(),
                "embedding",
            )?,
            vision_model: validated_model(vision_model.into(), "vision")?,
            vision_execution_identity: validated_execution_identity(
                vision_execution_identity.into(),
                "vision",
            )?,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ControlRequest {
    Pause,
    Cancel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct JobCursor {
    pub(crate) schema_version: u32,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) revision: u64,
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) execution_identity: Option<String>,
    #[serde(default)]
    pub(crate) input_execution_identity: Option<String>,
    #[serde(default)]
    pub(crate) next_ordinal: usize,
    /// Blocks a per-block retry asked for, as a sorted ordinal list. Empty means
    /// a normal run: walk the whole book from `next_ordinal`. Non-empty means
    /// `next_ordinal` sits at the first entry and the run stops after the last
    /// one, so blocks outside the set are never re-sent to the provider — an
    /// already translated block in between still hits its cache row and is not
    /// requested again.
    #[serde(default)]
    pub(crate) retry_ordinals: Vec<usize>,
    /// 收窄结束后要回到的位置。「立即翻译本章」把推进点借给一章时写下它：最后一个目标
    /// 块提交的那一刻，`next_ordinal` 回到这里继续整本（见 `TranslationRun::commit`）。
    /// 为空表示这次收窄不回来 —— 现有的按块重试就是如此，它跑完停在集合之后。
    ///
    /// 连续点两章时保留更早的那个位置，否则第二章会把「原地到第一章之间」那一段丢掉。
    #[serde(default)]
    pub(crate) resume_ordinal: Option<usize>,
    /// 逐块「重新翻译」只跑 `retry_ordinals` 这一组就收尾：walk 的窗口不越过最后一个
    /// 目标块，后面的文本块既不重挂、也不进入这一轮。`false`（默认）让收窄按旧的
    /// 跨度规则一路重挂到书末；「立即翻译本章」用它保证整本仍然能继续推进。
    #[serde(default)]
    pub(crate) stops_after_targets: bool,
    /// Ordinals this execution has already sent to the provider and not committed
    /// yet, sorted. `任务并发` is how many of them one task keeps open at a time,
    /// and the task window reads the set as 「处理中」.
    ///
    /// Only whole-book translation fills this in; every other kind walks one item
    /// at a time and leaves it empty. `next_ordinal` never moves past a block that
    /// is still waiting, so an interrupted run only ever redoes work — nothing in
    /// flight is treated as done, and the set is empty again in every terminal
    /// state (a paused task must not keep claiming requests that are already
    /// gone).
    #[serde(default)]
    pub(crate) inflight: Vec<usize>,
    /// Version of the text-block extraction rules this cursor was built with.
    /// Every cursor is written with the current version; only whole-book
    /// translation compares it, because only its ordinals are block ordinals.
    ///
    /// Changing the rules (a stricter "this block is source code" test, for
    /// example) changes how many blocks the book has, so every ordinal after the
    /// first affected block now points at a different piece of text. A cursor
    /// from another version is therefore reset to the start of the book in
    /// `from_job`: stored translations still match by cache key, so the rescan
    /// costs no requests.
    #[serde(default)]
    pub(crate) blocks_version: u32,
    /// 用户「立即翻译本章」写下的排队优先级（请求时间戳，越大越靠前）。它只是调度
    /// 提示：运行本身不看它，只有 `db::index_jobs::list_queued` 用它把这一本书的翻译
    /// 排到其它待执行任务前面。为空表示没有插队请求，按 `created_at` 的既有顺序。
    ///
    /// 它随游标一起持久化，所以重启后依然生效；下一次点（任意一本书）写下更大的
    /// 时间戳即可顶掉它，不需要清理。
    #[serde(default)]
    pub(crate) priority: Option<u64>,
    /// 用户显式要求「重新翻译」这一批目标块：命中缓存也要重发一次。
    ///
    /// 只有阅读端的逐块「重新翻译」写下它（2026-10-15：这一块的译文不满意，或者换了
    /// 更好的模型想把这一块重做一遍）。任务窗口的「重试失败块」保持缓存规则 —— 那些块
    /// 本来就没有译文，命中缓存是「别为同一块重复付费」的保护。
    ///
    /// 与 `retry_ordinals` 同生共死：最后一个目标块提交时一起清掉（见
    /// `TranslationRun::commit`），块列表版本变化导致收窄作废时同样清掉。
    #[serde(default)]
    pub(crate) force_retry: bool,
}

impl JobCursor {
    pub(crate) fn from_job(
        job: &db::index_jobs::IndexJob,
        model: &str,
        execution_identity: Option<&str>,
    ) -> Result<Self> {
        let mut cursor =
            serde_json::from_str::<Self>(&job.cursor_json).context("索引任务游标无效")?;
        let source_id = job.source_id.as_deref().context("索引任务缺少来源")?;
        ensure!(
            cursor.schema_version == 1
                && cursor.book_id == job.book_id
                && cursor.source_id == source_id
                && cursor.kind == job.kind,
            "索引任务游标与任务身份不匹配"
        );
        if cursor.model.as_deref() != Some(model)
            || execution_identity
                .is_some_and(|identity| cursor.execution_identity.as_deref() != Some(identity))
        {
            cursor.model = Some(model.to_string());
            cursor.execution_identity = execution_identity.map(str::to_string);
            cursor.input_execution_identity = None;
            // 翻译任务的序号只由正文的块列表决定，与模型/执行身份无关：已存译文在重配置
            // 时已改签到当前身份（`adopt_execution_identity`），缓存仍按内容命中。换模型
            // 时把推进点退回书首，只会让任务窗口的「已处理」归零、把整本重扫一遍，并把
            // 还没改签到的行当成「没译文」重新发请求。其它种类保持原有的重置语义。
            if cursor.kind != TRANSLATION_JOB_KIND {
                cursor.next_ordinal = 0;
            }
        }
        // 提取文本块的规则变了（例如「这一块是源代码」的判定变严），块数随之改变，
        // 于是受影响之后的每个序号都指向了另一段文字。旧版本的游标因此退回书首重扫：
        // 已存译文仍按缓存键命中，重扫不花请求。只有整本翻译读这个字段，其它任务
        // 恒为当前版本，不会因为这条检查被重置。
        if cursor.kind == TRANSLATION_JOB_KIND
            && cursor.blocks_version != TRANSLATION_BLOCKS_VERSION
        {
            cursor.blocks_version = TRANSLATION_BLOCKS_VERSION;
            cursor.next_ordinal = 0;
            cursor.retry_ordinals.clear();
            // 这份收窄连同它的「强制重发」一起作废：块列表变了，序号也不再指向同一块。
            cursor.force_retry = false;
            // 回到原位置说的是「旧编号里的哪一块」，块列表变了这个编号就不再指向同一处。
            cursor.resume_ordinal = None;
            cursor.inflight.clear();
        }
        Ok(cursor)
    }

    pub(crate) fn encode(&self) -> Result<String> {
        serde_json::to_string(self).context("无法序列化索引任务游标")
    }

    /// Opens one more block in the parallel window. The walk dispatches blocks in
    /// ordinal order, so appending keeps the list sorted and deduplicated.
    pub(crate) fn open_inflight(&mut self, ordinal: usize) {
        debug_assert!(
            self.inflight.iter().all(|open| *open < ordinal),
            "翻译窗口必须按序号打开"
        );
        self.inflight.push(ordinal);
    }

    /// Drops every block at or below `committed`. The walk commits in ordinal
    /// order, so everything at or below it is finished and must not be reported
    /// as in flight any more.
    pub(crate) fn close_inflight(&mut self, committed: usize) {
        self.inflight.retain(|open| *open > committed);
    }
}

pub(crate) enum RunOutcome {
    Succeeded(JobCursor),
    Paused(JobCursor),
    Cancelled(JobCursor, Option<String>),
    /// `None` when the failure has no single fixed category (for example a task
    /// that never had a model to call). The window shows the message instead.
    Failed(JobCursor, String, Option<JobLogErrorKind>),
    WaitingForPages(JobCursor),
    Abandoned,
}

/// Why one in-flight block must stop early.
///
/// A block request cannot name the position to publish — the run keeps advancing
/// the cursor while the request waits — so a request only reports the reason and
/// the run decides which cursor the outcome carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopRequest {
    Pause,
    Cancel,
    /// The book published a newer revision, so every translation of this run is
    /// stale and is dropped.
    SourceChanged,
    /// The durable row is no longer this execution's: it was replaced by a
    /// settings reset, reclaimed, or is not running any more.
    Abandoned,
}
