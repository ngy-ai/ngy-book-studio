//! The window state: selection, filters, block progress and detail tabs.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackgroundJobBook {
    pub id: String,
    pub title: String,
}

/// Which detail panel is open for the selected task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DetailTab {
    Summary,
    Logs,
    /// Per-block view of a whole-book translation task.
    Blocks,
}

/// State of one translatable block in the inspector. `Done`/`Processing`/
/// `Pending` come from the live durable cursor; `Failed` comes from the bounded
/// per-block diagnostics of the newest execution, because a skipped block
/// advances the cursor just like a cache hit and so cannot be named by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockState {
    Done,
    Processing,
    Failed,
    Pending,
}

impl BlockState {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Done => "已处理",
            Self::Processing => "处理中",
            Self::Failed => "失败",
            Self::Pending => "待处理",
        }
    }

    pub(super) fn colors(self, theme: Palette) -> (gpui::Rgba, gpui::Rgba) {
        match self {
            Self::Done => (rgb(theme.success), rgb(theme.success_soft)),
            Self::Processing => (rgb(theme.accent_strong), rgb(theme.accent_soft)),
            Self::Failed => (rgb(theme.danger), rgb(theme.danger_soft)),
            Self::Pending => (rgb(theme.neutral), rgb(theme.neutral_soft)),
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Processing => "processing",
            Self::Failed => "failed",
            Self::Pending => "pending",
        }
    }
}

/// Blocks the newest execution of one translation task could not translate,
/// with the fixed category recorded for each of them. Diagnostics are the only
/// place that names *which* block failed: a skipped block advances the durable
/// cursor like a cache hit, so the counters alone can only say how far the task
/// got.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockFailures {
    /// Block ordinal -> fixed failure category, when one was recorded.
    pub(crate) reasons: BTreeMap<usize, Option<JobLogErrorKind>>,
    /// Per-block diagnostics of this task were pruned, so blocks that failed
    /// earlier in the same execution may be missing from `reasons`.
    pub(crate) truncated: bool,
}

impl BlockFailures {
    /// Reads the newest execution out of the task's persistent log. Lines older
    /// than the last `RunStarted` belong to an earlier attempt: a block that
    /// failed once and was translated by a retry is not a failure now.
    pub(super) fn from_logs(logs: Option<&BackgroundJobLogSnapshot>) -> Self {
        let Some(logs) = logs else {
            return Self::default();
        };
        let mut reasons = BTreeMap::new();
        for entry in logs.entries.iter().rev() {
            if entry.event == JobLogEvent::RunStarted {
                break;
            }
            if !matches!(
                entry.event,
                JobLogEvent::ProtocolSkipped | JobLogEvent::StepFailed
            ) {
                continue;
            }
            let Some(ordinal) = entry.metrics.ordinal else {
                continue;
            };
            // A controlled stop is not a block failure: pausing or cancelling
            // aborts the in-flight request and would otherwise mark its block.
            if entry.metrics.error_kind == Some(JobLogErrorKind::Cancelled) {
                continue;
            }
            reasons
                .entry(ordinal as usize)
                .or_insert(entry.metrics.error_kind);
        }
        Self {
            reasons,
            truncated: logs.truncated,
        }
    }

    pub(super) fn contains(&self, ordinal: usize) -> bool {
        self.reasons.contains_key(&ordinal)
    }

    pub(super) fn reason(&self, ordinal: usize) -> Option<&str> {
        self.reasons
            .get(&ordinal)
            .and_then(|kind| *kind)
            .map(JobLogErrorKind::label)
    }

    pub(super) fn failed(&self, total: usize) -> usize {
        self.reasons
            .keys()
            .filter(|ordinal| **ordinal < total)
            .count()
    }

    /// Blocks already accounted for by the cursor lose that state to `Failed`,
    /// so the counters keep adding up to the block count.
    pub(super) fn before_cursor(&self, cursor: usize) -> usize {
        self.reasons
            .keys()
            .filter(|ordinal| **ordinal < cursor)
            .count()
    }
}

/// Live translation counters for one job. `cursor` mirrors the durable cursor:
/// the first block the task has not finished, and the first one a failed task
/// still has to translate. `processing` mirrors the worker's open window — whole
/// book translation keeps up to `任务并发` blocks in flight at once, so the cursor
/// alone can no longer say which block is being translated.
///
/// A block the newest execution recorded as failed leaves the state the cursor
/// would give it, so `done + processing + failed + pending` always equals
/// `total` and the header never disagrees with the list below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranslationProgress {
    pub(crate) total: usize,
    /// Raw durable cursor, kept for the anchor page.
    pub(crate) cursor: usize,
    pub(crate) done: usize,
    /// Ordinals the worker has sent and not committed yet, ascending. Empty for
    /// a task that is not walking a whole book, and for every terminal state.
    pub(crate) processing: Vec<usize>,
    pub(crate) failed: usize,
    pub(crate) pending: usize,
}

impl TranslationProgress {
    pub(super) fn new(job: &BackgroundJobSnapshot, total: usize, failures: &BlockFailures) -> Self {
        let cursor = job.progress.completed.min(total);
        let failed = failures.failed(total);
        let failed_before_cursor = failures.before_cursor(cursor);
        // 窗口里的块不会再被算成失败：它对 `state()` 是 `Failed`（最新一次执行记过失败），
        // 对计数也是失败，否则表头加起来会超过块总数。
        let processing = in_flight_blocks(job, cursor, total)
            .into_iter()
            .filter(|ordinal| !failures.contains(*ordinal))
            .collect::<Vec<_>>();
        let failed_after_cursor = failed - failed_before_cursor;
        let pending = total - cursor - processing.len() - failed_after_cursor;
        Self {
            total,
            cursor,
            done: cursor - failed_before_cursor,
            processing,
            failed,
            pending,
        }
    }

    /// Whether one ordinal is inside the worker's open window.
    pub(super) fn is_processing(&self, index: usize) -> bool {
        self.processing.binary_search(&index).is_ok()
    }

    pub(super) fn count(&self, state: BlockState) -> usize {
        match state {
            BlockState::Done => self.done,
            BlockState::Processing => self.processing.len(),
            BlockState::Failed => self.failed,
            BlockState::Pending => self.pending,
        }
    }

    pub(super) fn filtered(&self, filter: Option<BlockState>) -> usize {
        match filter {
            Some(state) => self.count(state),
            None => self.total,
        }
    }
}

/// Ordinals the running worker has in flight, as the service layer projected them.
///
/// 只有整本翻译会填这个集合（其他任务逐项走、窗口恒空），所以它同时也是「这本图书现在
/// 有几块在跑」的唯一权威读数。缺口一律按「没有在飞的块」处理：投影缺失（旧格式游标、
/// 写坏的 JSON）、已经不在运行的任务都不该凭空显示成处理中。已提交的块（序号小于游标）
/// 永远不算在飞 —— 提交意味着这一块已经写完，游标也不会越过还在等的块。
///
/// 读数来自 `BackgroundJobProgress::processing`，**不是**快照里的原始游标：运行中的
/// 翻译任务不下发内部游标 JSON（见 `BackgroundJobSnapshot::cursor_json`），窗口由服务层
/// 解析后交出来。现场 2026-09-20：并发 16 的翻译任务「已处理」一直在涨、「处理中」恒为 0，
/// 因为界面当时只能去读那一份永远缺失的游标。
pub(crate) fn in_flight_blocks(
    job: &BackgroundJobSnapshot,
    cursor: usize,
    total: usize,
) -> Vec<usize> {
    if job.status != BackgroundJobStatus::Running {
        return Vec::new();
    }
    let mut blocks = job
        .progress
        .processing
        .iter()
        .copied()
        .filter(|ordinal| *ordinal >= cursor && *ordinal < total)
        .collect::<Vec<_>>();
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}

/// One inspected translation task: the live counters derived from the durable
/// cursor plus the blocks the newest execution could not translate, which only
/// the persistent diagnostics know about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockInspector {
    pub(crate) progress: TranslationProgress,
    pub(crate) failures: BlockFailures,
}

impl BlockInspector {
    pub(super) fn new(
        job: &BackgroundJobSnapshot,
        total: usize,
        logs: Option<&BackgroundJobLogSnapshot>,
    ) -> Self {
        let failures = BlockFailures::from_logs(logs);
        Self {
            progress: TranslationProgress::new(job, total, &failures),
            failures,
        }
    }

    pub(super) fn state(&self, index: usize) -> BlockState {
        if self.failures.contains(index) {
            BlockState::Failed
        } else if index < self.progress.cursor {
            BlockState::Done
        } else if self.progress.is_processing(index) {
            BlockState::Processing
        } else {
            BlockState::Pending
        }
    }

    pub(super) fn reason(&self, index: usize) -> Option<&str> {
        self.failures.reason(index)
    }
}

/// Which part of one block's debug view is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockDetailTab {
    /// Identifiers, sizes and request shape, with nothing that can be truncated.
    Overview,
    Source,
    /// The serialized chat-completions body — the payload actually sent.
    Body,
    Prompt,
    Message,
    /// The raw answer of one on-demand replay. Only offered for a failed block:
    /// a block that translated fine has no answer worth re-asking for.
    Response,
}

impl BlockDetailTab {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Overview => "概览",
            Self::Source => "原文",
            Self::Body => "请求 Body",
            Self::Prompt => "System 提示词",
            Self::Message => "User 内容",
            Self::Response => "模型响应",
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Source => "source",
            Self::Body => "body",
            Self::Prompt => "prompt",
            Self::Message => "message",
            Self::Response => "response",
        }
    }
}

/// Tabs of one block's debug view, in display order. The response tab exists
/// only for a failed block, so a healthy block keeps exactly the five request-side
/// tabs it always had.
pub(crate) fn block_detail_tabs(failed: bool) -> Vec<BlockDetailTab> {
    let tabs = [
        BlockDetailTab::Overview,
        BlockDetailTab::Body,
        BlockDetailTab::Source,
        BlockDetailTab::Prompt,
        BlockDetailTab::Message,
    ];
    if failed {
        return tabs.into_iter().chain([BlockDetailTab::Response]).collect();
    }
    tabs.to_vec()
}

/// One on-demand replay of the open block, cached while its viewer stays open.
/// Kept separate from `loading`/`detail`: a replay is a fresh model request the
/// user triggers on purpose, it can be repeated, and it must never be confused
/// with the pinned request payload that the other tabs describe.
#[derive(Clone, Debug, Default)]
pub(crate) struct BlockProbeState {
    /// A replay is in flight. The button is disabled while it is.
    pub(crate) loading: bool,
    pub(crate) result: Option<TranslationBlockProbe>,
    pub(crate) error: Option<String>,
}

/// The block whose debug view is open, and the payload once it is loaded. The
/// ordinal is kept separately so the panel can name the block while it is still
/// loading or after a failure.
#[derive(Clone, Debug)]
pub(crate) struct BlockDetailState {
    pub(crate) job_id: String,
    pub(crate) ordinal: usize,
    pub(crate) generation: u64,
    pub(crate) tab: BlockDetailTab,
    /// The block is one the newest execution could not translate, which is what
    /// unlocks the response tab.
    pub(crate) failed: bool,
    pub(crate) loading: bool,
    pub(crate) detail: Option<TranslationBlockDetail>,
    pub(crate) error: Option<String>,
    pub(crate) probe: BlockProbeState,
    pub(crate) scroll: ScrollHandle,
}

#[derive(Clone, Debug)]
pub(crate) struct JobsNotice {
    pub(crate) text: String,
    pub(crate) error: bool,
}
