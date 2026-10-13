//! The durable state of one translation run: cursor commits, block slots and
//! the run state machine.

use super::*;

/// One block's coordinates inside a parallel walk.
///
/// The walk keeps mutating the cursor while a request waits, so a request cannot
/// borrow it. Everything it needs is copied out instead: `ordinal` names the
/// block in diagnostics and publication (never the live cursor, which may already
/// point at a later block), `revision` pins the source, and `committed` holds the
/// newest cursor JSON this run persisted — compared against the durable row on
/// every control poll, so a request notices a pause/cancel or that the row no
/// longer belongs to it.
#[derive(Clone)]
pub(crate) struct TranslationBlockScope {
    pub(crate) ordinal: usize,
    pub(crate) revision: u64,
    pub(crate) committed: Arc<Mutex<String>>,
}

impl TranslationBlockScope {
    pub(crate) fn committed_json(&self) -> String {
        self.committed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

/// Counters one block contributes to the run summary. A parallel walk cannot share
/// the run's stats across requests, so a block counts its own and the run adds
/// them up when it commits the block.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BlockStats {
    pub(crate) fragment_fallback_blocks: usize,
    pub(crate) fragment_fallback_requests: usize,
    pub(crate) duration_ms: u64,
}

/// What one slot of the translation window resolved to. Slots are applied in
/// ordinal order, so the durable cursor never steps over a block that is still
/// waiting or that finished out of order.
pub(crate) enum BlockSlot {
    /// Nothing to publish: the ordinal is outside the retried set.
    Pass,
    /// Republish the translation already stored for this block.
    Cached(StoredTranslation),
    /// A fresh translation, plus the counters its own walk contributed.
    Translated(StoredTranslation, BlockStats),
    /// 这一块没有译文。原文保留、运行继续，任务窗口把它列进「失败」。
    Untranslated(SkipCause, JobLogErrorKind),
}

/// 一个文本块被跳过的原因。它决定记哪条事件（`ProtocolSkipped` / `ProviderSkipped`），
/// 也决定 walk 的连续失败诊断读数；两者都不中止运行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkipCause {
    /// 模型答了、但形状不合格（`ResponseError`）。
    Protocol,
    /// 请求本身失败：传输、超时、流协议、上下文超限、响应过大。
    Request,
}

/// 一个槽位提交之后的结果，供 walk 统计「连续多少个块的请求失败」这类诊断读数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommittedBlock {
    /// 译出来了，或复用了已有译文。
    Translated,
    /// 这一块被跳过，附上原因与固定分类。
    Skipped(SkipCause, JobLogErrorKind),
    /// 本次不处理（重试范围之外），或这一块没有可翻译的片段。
    Untouched,
}

/// One block's provider walk: either a translation with the counters that walk
/// contributed, or the reason the walk stopped early. The walk owns these counters
/// instead of the run: a parallel window cannot share one mutable summary across
/// requests, and the run adds them up when it commits the block.
pub(crate) enum BlockWalk {
    Stored(StoredTranslation, BlockStats),
    Stopped(StopRequest),
}

/// One awaited provider step inside a block. It mirrors [`BlockWalk`] because the
/// steps that only produce a value (a stream, a collected response) must be able
/// to stop the block too — a parallel window learns about a pause/cancel from
/// whichever of its requests polls first.
pub(crate) enum BlockStep<T> {
    Value(T),
    Stopped(StopRequest),
}

/// What one ordinal of the window needs before it can be committed.
pub(crate) enum BlockPlan<'a> {
    /// Nothing to send and nothing to publish: outside the retried set.
    Pass,
    /// 出了本次重试范围、又没有译文：本次运行既不许替它发请求，也不许把它记成
    /// 已处理（那正是「任务显示完成、书却还是原文」的来源）。运行停在这里。
    Untouched,
    /// The block has no text leaves to translate.
    NoSegments,
    /// Publish the translation already stored for this block text.
    Cached(Box<StoredTranslation>),
    /// Send this block to the provider.
    Request {
        source: &'a TranslationSource,
        /// 书名与章节标题，随请求一起发给模型。
        context: TranslationContext<'a>,
    },
}

/// The durable progress of one translation run: the cursor this execution owns
/// plus the JSON of the last value it persisted.
///
/// In-flight requests read the JSON instead of holding the cursor: the run keeps
/// advancing the cursor while they wait, and only the value it last wrote proves
/// the durable row is still theirs.
pub(crate) struct TranslationRun {
    pub(crate) cursor: JobCursor,
    pub(crate) committed: Arc<Mutex<String>>,
}

impl TranslationRun {
    pub(crate) fn new(cursor: JobCursor) -> Result<Self> {
        let committed = cursor.encode()?;
        Ok(Self {
            cursor,
            committed: Arc::new(Mutex::new(committed)),
        })
    }

    /// Hands one block request the coordinates it is allowed to read.
    pub(crate) fn scope(&self, ordinal: usize) -> TranslationBlockScope {
        TranslationBlockScope {
            ordinal,
            revision: self.cursor.revision,
            committed: Arc::clone(&self.committed),
        }
    }

    /// Writes the current cursor to the durable row and then publishes it to the
    /// in-flight requests. The order matters: a request must never accept a value
    /// the database has not seen.
    ///
    /// A rejected write also rolls the cursor back to the value the row really
    /// holds. This execution must keep reporting the last committed position:
    /// a cursor the database never accepted is not just a lost update, it also
    /// makes the failure publication unmatchable, so the task would stay「运行中」
    /// with a phantom in-flight block forever（现场：夹具拒绝游标推进后，任务卡在
    /// running、处理中恒有一块，终态永远发布不出去）。
    pub(crate) async fn persist(
        &mut self,
        inner: &Arc<IndexingInner>,
        job: &db::index_jobs::IndexJob,
    ) -> Result<()> {
        if let Err(error) = persist_running_cursor(inner, job, &self.cursor).await {
            self.restore_committed_cursor();
            return Err(error);
        }
        *self
            .committed
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.cursor.encode()?;
        Ok(())
    }

    /// Rolls the cursor back to the JSON this run last persisted successfully.
    ///
    /// That value is by construction the row's current cursor, so the failure
    /// publication after a rejected write still matches the row and the task still
    /// reaches a terminal state. An unparseable snapshot leaves the cursor alone:
    /// the publication then reports「已被替换」instead of guessing a position.
    fn restore_committed_cursor(&mut self) {
        let committed = self
            .committed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Ok(cursor) = serde_json::from_str::<JobCursor>(&committed) {
            self.cursor = cursor;
        }
    }

    /// Moves the cursor past one committed block. Only called in ordinal order.
    pub(crate) async fn commit(
        &mut self,
        inner: &Arc<IndexingInner>,
        job: &db::index_jobs::IndexJob,
        ordinal: usize,
    ) -> Result<()> {
        self.cursor.next_ordinal = ordinal + 1;
        self.cursor.close_inflight(ordinal);
        // 最后一个重试目标已经提交，这份收窄就到此用完了，必须立刻从持久游标里去掉：
        // 留着它以后每一次恢复、重试或重跑都还会被收窄成「只跑这几块」，而 walk 会
        // 把其余的块当成不必处理一路推过去 —— 现场 2026-09-20：18168 块的书只翻了 71
        // 块，任务却显示「已完成 / 已处理 18168」，阅读端整本没有译文。
        if self
            .cursor
            .retry_ordinals
            .last()
            .is_some_and(|last| ordinal >= *last)
        {
            self.cursor.retry_ordinals.clear();
            // 「立即翻译本章」只是把推进点借给一章用：最后一个目标块提交后要把位置还给
            // 原来的推进点，否则这一本从本章之后继续，中间没翻的章节再也没人碰
            // （同一个坑的另一半见上面的 2026-09-20 记录）。按块重试不带这个位置，
            // 它跑完就停在集合之后，行为与以前逐字相同。
            if let Some(resume) = self.cursor.resume_ordinal.take() {
                self.cursor.next_ordinal = resume;
            }
        }
        self.persist(inner, job).await
    }

    /// Ends the run with an empty window. A paused or finished task that still
    /// claimed 「处理中」 blocks would be lying to the task window, and publication
    /// only matches a cursor equal to its own, so the settle is persisted before
    /// the outcome is published. Callers hold `transitions`, so the cleared cursor
    /// cannot race a settings reset.
    pub(crate) async fn settle(
        &mut self,
        inner: &Arc<IndexingInner>,
        job: &db::index_jobs::IndexJob,
    ) -> Result<()> {
        if self.cursor.inflight.is_empty() {
            return Ok(());
        }
        self.cursor.inflight.clear();
        self.persist(inner, job).await
    }

    /// Publishes the reason a walk stopped early. An abandoned run writes nothing
    /// at all: the durable row belongs to whoever replaced it.
    ///
    /// The outcome carries the cursor this run has *after* the window was cleared.
    /// The stop outcome a request turned into still names the pre-settle cursor —
    /// which is also the JSON `publish_outcome` matches the durable row against —
    /// so publishing it unchanged would both fail the match and let a paused task
    /// keep claiming requests that no longer exist.
    pub(crate) async fn interrupt(
        &mut self,
        inner: &Arc<IndexingInner>,
        job: &db::index_jobs::IndexJob,
        outcome: RunOutcome,
    ) -> Result<RunOutcome> {
        if matches!(outcome, RunOutcome::Abandoned) {
            return Ok(outcome);
        }
        self.settle(inner, job).await?;
        Ok(self.settle_outcome(outcome))
    }

    /// The same reason, published at the position this run actually reached.
    pub(crate) fn settle_outcome(&self, outcome: RunOutcome) -> RunOutcome {
        let cursor = self.cursor.clone();
        match outcome {
            RunOutcome::Succeeded(_) => RunOutcome::Succeeded(cursor),
            RunOutcome::Paused(_) => RunOutcome::Paused(cursor),
            RunOutcome::Cancelled(_, reason) => RunOutcome::Cancelled(cursor, reason),
            RunOutcome::Failed(_, message, kind) => RunOutcome::Failed(cursor, message, kind),
            RunOutcome::WaitingForPages(_) => RunOutcome::WaitingForPages(cursor),
            RunOutcome::Abandoned => RunOutcome::Abandoned,
        }
    }

    /// The outcome of a controlled stop, at the newest committed position.
    pub(crate) fn stopped(&self, stop: StopRequest) -> RunOutcome {
        match stop {
            StopRequest::Pause => RunOutcome::Paused(self.cursor.clone()),
            StopRequest::Cancel => RunOutcome::Cancelled(self.cursor.clone(), None),
            StopRequest::SourceChanged => RunOutcome::Cancelled(
                self.cursor.clone(),
                Some("图书已发布更新版本，翻译结果已丢弃".to_string()),
            ),
            StopRequest::Abandoned => RunOutcome::Abandoned,
        }
    }
}

pub(crate) async fn run_translation(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    models: &ModelServices,
) -> Result<RunOutcome> {
    let Some(translation) = models.translation.clone() else {
        return Ok(RunOutcome::Failed(
            cursor_without_model(job)?,
            "翻译模型尚未配置".to_string(),
            None,
        ));
    };
    let target_language = translation_target_language(job)?;
    let mut cursor = JobCursor::from_job(
        job,
        &translation.model,
        Some(&translation.execution_identity),
    )?;
    tracing::info!(
        target: "ngy_ai",
        stage = "translation_run_start",
        target_language = %crate::ai::ai_diagnostics::safe_label(&target_language),
        model = %crate::ai::ai_diagnostics::safe_label(&translation.model),
        execution_identity = %crate::ai::ai_diagnostics::safe_label(&translation.execution_identity),
        attempts = job.attempts,
        next_ordinal = cursor.next_ordinal,
        max_output_tokens = translation.generation.max_output_tokens,
        temperature = ?translation.generation.temperature,
        response_attempts = TRANSLATION_RESPONSE_ATTEMPTS,
        "Translation run started"
    );
    match run_translation_blocks(inner, job, &translation, &target_language, &mut cursor).await {
        Ok(outcome) => {
            let (result, next_ordinal) = match &outcome {
                RunOutcome::Succeeded(cursor) => ("succeeded", cursor.next_ordinal),
                RunOutcome::Paused(cursor) => ("paused", cursor.next_ordinal),
                RunOutcome::Cancelled(cursor, _) => ("cancelled", cursor.next_ordinal),
                RunOutcome::Failed(cursor, _, _) => ("failed", cursor.next_ordinal),
                RunOutcome::WaitingForPages(cursor) => ("waiting_for_pages", cursor.next_ordinal),
                RunOutcome::Abandoned => ("abandoned", cursor.next_ordinal),
            };
            tracing::info!(
                target: "ngy_ai",
                stage = "translation_run_finish",
                result,
                next_ordinal,
                "Translation run finished"
            );
            Ok(outcome)
        }
        // Keep the cursor owned by this execution, including on provider or
        // protocol failure. Reading the latest database cursor here could
        // mark a newly configured translation task as failed.
        Err(error) => {
            record_index_event(
                inner,
                job,
                JobLogEvent::StepFailed,
                JobLogMetrics {
                    ordinal: Some(cursor.next_ordinal as u64),
                    error_kind: Some(classify_error(&error)),
                    ..JobLogMetrics::for_error(&error)
                },
            );
            tracing::warn!(
                target: "ngy_ai",
                stage = "translation_run_failed",
                error_kind = crate::ai::ai_diagnostics::error_kind(&error),
                next_ordinal = cursor.next_ordinal,
                "Translation run failed"
            );
            Ok(RunOutcome::Failed(
                cursor,
                format!("{error:#}"),
                Some(classify_error(&error)),
            ))
        }
    }
}

pub(crate) async fn run_translation_blocks(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    cursor: &mut JobCursor,
) -> Result<RunOutcome> {
    let started = Instant::now();
    let mut stats = TranslationRunStats::default();
    let outcome =
        run_translation_blocks_inner(inner, job, translation, target_language, cursor, &mut stats)
            .await;
    // 收尾汇总仍然打印失败运行：一次「什么都没保存」的运行必须能和「保存了但最后
    // 失败」区分开，否则只能靠逐行日志数数。
    tracing::debug!(
        target: "ngy_ai",
        stage = "translation_run_stats",
        saved = stats.saved,
        cached = stats.cached,
        no_segments = stats.no_segments,
        untranslated = stats.untranslated,
        fragment_fallback_blocks = stats.fragment_fallback_blocks,
        fragment_fallback_requests = stats.fragment_fallback_requests,
        cooldowns = stats.cooldowns,
        next_ordinal = cursor.next_ordinal,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Translation run statistics"
    );
    outcome
}

pub(crate) async fn run_translation_blocks_inner(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    cursor: &mut JobCursor,
    stats: &mut TranslationRunStats,
) -> Result<RunOutcome> {
    let mut run = TranslationRun::new(cursor.clone())?;
    let outcome =
        walk_translation_blocks(inner, job, translation, target_language, &mut run, stats).await;
    // The walk's progress belongs to the caller even when it failed: the failure
    // publication has to name the last committed position.
    *cursor = run.cursor.clone();
    outcome
}
