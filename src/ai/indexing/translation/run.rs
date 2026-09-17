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
    /// The model never answered in the required shape. The block keeps its
    /// original text and the run goes on; the task window lists it under 失败.
    Untranslated(JobLogErrorKind),
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
    pub(crate) async fn persist(
        &self,
        inner: &Arc<IndexingInner>,
        job: &db::index_jobs::IndexJob,
    ) -> Result<()> {
        persist_running_cursor(inner, job, &self.cursor).await?;
        *self
            .committed
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.cursor.encode()?;
        Ok(())
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
        max_output_tokens = TRANSLATION_MAX_OUTPUT_TOKENS,
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
