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

    pub(super) fn colors(self, theme: Palette) -> (gpui_kit::Rgba, gpui_kit::Rgba) {
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
            // 两条跳过事件都要算：一条是「模型答得不对」，一条是「请求根本失败」，
            // 用户看到的都是「这一块没有译文，可以重试」。
            if !matches!(
                entry.event,
                JobLogEvent::ProtocolSkipped
                    | JobLogEvent::ProviderSkipped
                    | JobLogEvent::StepFailed
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
}

/// Live translation counters for one job. `cursor` mirrors the durable cursor:
/// the first block the task has not finished, and the first one a failed task
/// still has to translate. `processing` mirrors the worker's open window — whole
/// book translation keeps up to `任务并发` blocks in flight at once, so the cursor
/// alone can no longer say which block is being translated. `done_limit` carries
/// the boundary a narrowed run borrows its cursor from; 已处理 comes from it
/// instead of the cursor.
///
/// A block the newest execution recorded as failed leaves the state the cursor
/// would give it, so `done + processing + failed + pending` always equals
/// `total` and the header never disagrees with the list below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranslationProgress {
    pub(crate) total: usize,
    /// Raw durable cursor, kept for the anchor page.
    pub(crate) cursor: usize,
    /// 本次运行被收窄到的块（升序块号）。空表示整本：收窄的章节跳转与按块重试只
    /// 处理这些块，界面据此说明表头为什么不是整本书的进度。
    pub(crate) scope: Vec<usize>,
    /// 收窄范围之外的块的「已处理」边界：小于它的块在本次收窄开始前就已经处理过。
    ///
    /// 范围之内的块仍看游标（游标才代表这次运行走到哪），所以这个边界只回答收窄
    /// 之外那一部分。「立即翻译本章」借出推进点，它就是借出点；按块重试没有借出点，
    /// 它就是游标；整本运行范围为空，同样退回游标。少了这一份，借出期间游标停在
    /// 那一章的第一块，整本剩下的块会被显示成待处理（现场 2026-10-18：整本早已翻
    /// 完，点「立即翻译本章」后表头显示待处理 7763）。
    pub(crate) done_limit: usize,
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
        let scope = job.progress.scope.clone();
        // 收窄之外的块不归这次运行处理，但它们的真实状态由借出点决定：借出点之前的
        // 块是早先的运行处理过的，之后的块要等这次收窄结束、回到借出点才轮到它们。
        // 没有收窄（或按块重试没有借出点）时游标本身就是边界。
        let done_limit = if scope.is_empty() {
            cursor
        } else {
            job.progress
                .resume
                .map_or(cursor, |resume| resume.min(total))
        };
        // 窗口里的块不会再被算成失败：它对 `state()` 是 `Failed`（最新一次执行记过失败），
        // 对计数也是失败，否则表头加起来会超过块总数。
        let processing = in_flight_blocks(job, cursor, total)
            .into_iter()
            .filter(|ordinal| !failures.contains(*ordinal))
            .collect::<Vec<_>>();
        let mut progress = Self {
            total,
            cursor,
            scope,
            done_limit,
            done: 0,
            processing,
            failed: 0,
            pending: 0,
        };
        // 计数与 `state()` 共用同一份判定：四个数相加恒等于块总数，表头、筛选按钮与
        // 下面的列表不会各说各话。
        let mut done = 0;
        let mut failed = 0;
        let mut pending = 0;
        for index in 0..total {
            match progress.classify(index, failures) {
                BlockState::Done => done += 1,
                BlockState::Processing => {}
                BlockState::Failed => failed += 1,
                BlockState::Pending => pending += 1,
            }
        }
        progress.done = done;
        progress.failed = failed;
        progress.pending = pending;
        progress
    }

    /// State of one ordinal. 失败来自最新一次执行的逐块诊断，其余三种来自游标、
    /// 收窄边界与服务层投影的窗口；计数用的是同一份判定。
    fn classify(&self, index: usize, failures: &BlockFailures) -> BlockState {
        if failures.contains(index) {
            BlockState::Failed
        } else if self.is_processing(index) {
            BlockState::Processing
        } else if self.is_done(index) {
            BlockState::Done
        } else {
            BlockState::Pending
        }
    }

    /// Whether this run has already processed one ordinal.
    ///
    /// 收窄范围内的块只看游标：游标越过它才算这次运行翻过（范围里的块本来就该在
    /// 这一轮里重新过一遍）。范围之外的块不归这次运行，它们的答案来自借出点。
    fn is_done(&self, index: usize) -> bool {
        if self.scope.binary_search(&index).is_ok() {
            index < self.cursor
        } else {
            index < self.done_limit
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
        self.progress.classify(index, &self.failures)
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
    /// The serialized chat-completions body — the payload a single-block send
    /// puts on the wire（端点打包的批请求形状不同）.
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
