//! Planning and walking the parallel block window.

use super::*;

use crate::ai::translation::{
    TranslationBatchBudget, TranslationBatchPacker, estimate_batch_block,
};

/// Walks the blocks of one whole-book translation with up to `任务并发` requests
/// open at the same time.
///
/// The window is the only thing block-level concurrency changed: every block still
/// commits in ordinal order, so the durable cursor keeps the meaning it always had
/// (the first block the task has not finished) and a pause or cancel simply drops
/// whatever is in flight — those blocks are not committed and the next run redoes
/// them. Requests never borrow the cursor, which the walk keeps advancing while
/// they wait; they read the run's committed JSON instead, through
/// [`TranslationBlockScope`].
pub(crate) async fn walk_translation_blocks(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    run: &mut TranslationRun,
    stats: &mut TranslationRunStats,
) -> Result<RunOutcome> {
    {
        let _transition = inner.transitions.lock().await;
        if let Some(outcome) = requested_outcome(inner, job, &run.cursor).await? {
            return run.interrupt(inner, job, outcome).await;
        }
        if !source_is_current(inner, job, run.cursor.revision).await? {
            let outcome = run.stopped(StopRequest::SourceChanged);
            return run.interrupt(inner, job, outcome).await;
        }
        // A previous execution may have been killed with requests still open.
        // Nothing is in flight for this one, and the durable row must not claim
        // otherwise.
        run.cursor.inflight.clear();
        run.persist(inner, job).await?;
    }

    let book_id = run.cursor.book_id.clone();
    let source_id = run.cursor.source_id.clone();
    let revision = run.cursor.revision;
    let blocks = load_translation_blocks(inner, &book_id, &source_id, revision).await?;
    ensure!(
        run.cursor.next_ordinal <= blocks.len(),
        "翻译任务游标超出文本块数量"
    );

    let source_language = {
        let db_path = inner.db_path.clone();
        let book_id = book_id.clone();
        run_db(db_path, move |conn| {
            Ok(db::books::get(&conn, &book_id)?.and_then(|book| book.language))
        })
        .await?
    };

    tracing::info!(
        target: "ngy_ai",
        stage = "translation_blocks_loaded",
        total_blocks = blocks.len(),
        next_ordinal = run.cursor.next_ordinal,
        document_revision = revision,
        max_output_tokens = translation.generation.max_output_tokens,
        source_language = %source_language
            .as_deref()
            .map(crate::ai::ai_diagnostics::safe_label)
            .unwrap_or_else(|| "<none>".to_string()),
        "Translation blocks loaded"
    );

    record_index_event(
        inner,
        job,
        JobLogEvent::SourceLoaded,
        JobLogMetrics {
            ordinal: Some(run.cursor.next_ordinal as u64),
            total: Some(blocks.len() as u64),
            ..JobLogMetrics::default()
        },
    );
    let total = blocks.len();
    // Stored translations are read per content unit and reused for the whole run.
    // A block whose text was translated before — even under another block id — is
    // republished instead of being sent again.
    let mut unit_cache: HashMap<String, HashMap<String, StoredTranslation>> = HashMap::new();
    // A per-block retry narrows the walk to the requested ordinals; a normal run
    // leaves this empty and visits everything. The span ends at the last target:
    // beyond it the run is no longer allowed to decide anything about a block that
    // has no译文 (see `BlockPlan::Untouched`).
    let retry_targets: HashSet<usize> = run.cursor.retry_ordinals.iter().copied().collect();
    let retry_last = run.cursor.retry_ordinals.last().copied();
    // Ordinals in `[next_ordinal, frontier)` are exactly the window: each of them
    // is either finished (`resolved`) or waiting on a request (`in_flight`).
    let mut frontier = run.cursor.next_ordinal;
    let mut resolved: BTreeMap<usize, BlockSlot> = BTreeMap::new();
    let mut in_flight: FuturesUnordered<TranslationRequestFuture<'_>> = FuturesUnordered::new();
    // A block the model never answered in the required shape — or whose request
    // failed outright — is left untranslated and the run continues: one
    // pathological block must not block a whole book, and neither must any number
    // of them in a row. Every skip commits like a cache hit, so the run reaches
    // the end of the book and reports `Succeeded`; the blocks it could not
    // translate are listed by the block inspector, which is where the user retries
    // them after switching to a better model.
    let mut saved_blocks = 0usize;
    let mut untranslated_blocks = 0usize;
    // 连续多少个块的**请求级**失败就告警并冷却一次（见
    // `TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT`）。两者都**不中止运行**：连续失败多少个
    // 块，这个 walk 都要一直翻到书末。
    let mut consecutive_request_failures = 0usize;
    // 凑满阈值：提交完手上这批槽位之后先冷却，再继续开新请求。
    let mut streak_cooldown = false;
    // 本次运行接下来该用的翻译服务。默认是领取时的那份快照；用户在这次运行期间保存了
    // Endpoint、密钥、超时或生成参数时，换成库里最新的一份
    // （见 `refreshed_translation_services`）。
    let mut request_services = translation.clone();
    // 收窄的重试在范围之外撞上一个还没翻译的块：运行停在这里，任务留在可继续的状态。
    let mut retry_stopped = false;
    loop {
        // 1) Commit everything that is next in ordinal order. The durable cursor
        //    may never step over a block that is still waiting, and a settings
        //    reset or a pause/cancel must be noticed before the next publication.
        while let Some(slot) = resolved.remove(&run.cursor.next_ordinal) {
            let ordinal = run.cursor.next_ordinal;
            let _transition = match lock_transitions(inner, &mut in_flight, &mut resolved).await? {
                TransitionLock::Held(guard) => guard,
                TransitionLock::Stopped(stop) => {
                    return finish_stopped_run(inner, job, run, &mut in_flight, stop).await;
                }
            };
            if let Some(outcome) = requested_outcome(inner, job, &run.cursor).await? {
                return run.interrupt(inner, job, outcome).await;
            }
            if !source_is_current(inner, job, revision).await? {
                let outcome = run.stopped(StopRequest::SourceChanged);
                return run.interrupt(inner, job, outcome).await;
            }
            let committed = commit_translation_slot(
                inner,
                job,
                translation,
                target_language,
                &source_language,
                run,
                &blocks,
                ordinal,
                slot,
                &mut unit_cache,
                stats,
                &mut saved_blocks,
                &mut untranslated_blocks,
            )
            .await?;
            match committed {
                // 请求级失败只跳过这一块，运行一直翻到书末 —— 连续失败多少个都一样
                // （2026-09-24 用户裁定：连续失败就中止整次翻译是错的，现场一次端点
                // 抖动让任务在翻完 3045 块后按 Failed 收尾，剩下 4921 块再也没人翻）。
                // 凑满阈值只告警并标记一次冷却，冷却本身在提交循环之外进行。
                CommittedBlock::Skipped(SkipCause::Request, kind) => {
                    consecutive_request_failures += 1;
                    if !streak_cooldown
                        && consecutive_request_failures >= TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT
                    {
                        streak_cooldown = true;
                        tracing::warn!(
                            target: "ngy_ai",
                            stage = "translation_request_failure_streak",
                            ordinal,
                            consecutive = consecutive_request_failures,
                            error_kind = kind.label(),
                            cooldown_secs = inner.failure_cooldown.as_secs(),
                            "Consecutive model request failures; cooling down before continuing"
                        );
                    }
                }
                CommittedBlock::Skipped(SkipCause::Protocol, _) => {}
                CommittedBlock::Translated => consecutive_request_failures = 0,
                CommittedBlock::Untouched => {}
            }
        }
        // 连续请求失败凑满阈值：先冷却再继续开新请求，冷却结束把连续计数清零（下一组连续
        // 失败重新累计、再冷却一次）。冷却绝不能在提交循环里进行 —— 那条路径持有
        // `transitions` 门闩，睡一分钟等于把保存设置一起卡住。
        if streak_cooldown {
            streak_cooldown = false;
            // 书末已经没有新请求要发时不必再等：那只会让用户在任务最后一分钟里盯着一个
            // 不动的进度，等一个根本不会发出的请求。
            if frontier < total {
                let cooldown = inner.failure_cooldown;
                if let Some(outcome) = cool_down_after_request_failure_streak(
                    inner,
                    job,
                    run,
                    &mut in_flight,
                    &mut resolved,
                    cooldown,
                )
                .await?
                {
                    return Ok(outcome);
                }
                stats.cooldowns += 1;
                consecutive_request_failures = 0;
            }
        }
        // 用户在这次运行期间保存过 Endpoint、密钥、超时或生成参数时，`models.translation`
        // 已经是新的，而这次运行手里还是领取时的快照（现场 2026-09-24：改完 Endpoint 与密钥
        // 后，正在跑的任务继续用旧值翻完剩下的块）。下一块起换用新的 provider 与生成参数：
        // 模型、执行身份与目标语言保持一致，已存译文的缓存键与缓存行都不受影响。
        if let Some(fresh) = refreshed_translation_services(inner, &request_services) {
            if !Arc::ptr_eq(&fresh.provider, &request_services.provider) {
                tracing::info!(
                    target: "ngy_ai",
                    stage = "translation_services_refreshed",
                    next_ordinal = run.cursor.next_ordinal,
                    model = %crate::ai::ai_diagnostics::safe_label(&fresh.model),
                    "Translation requests will use the reconfigured endpoint and key"
                );
            }
            request_services = fresh;
        }
        // 2) Fill the window. The configured concurrency is re-read every round, so
        //    a saved change applies to a run that is already going — raising it does
        //    not interrupt anything, it only lets the walk run further ahead.
        //
        //    窗口按**请求**计：一轮里最多同时打开「任务并发」个请求。端点的「每请求
        //    文本块数」（`request_services.batch_blocks`）大于 1 时，同一个请求可以带走
        //    一批块（`TranslationBatchPacker` 的三个上限：输出预算、块数、请求体字节），
        //    所以一轮里同时打开的文本块最多是「并发 × 每请求块数」—— 任务窗口的
        //    「处理中」列的是块，请求数是另一件事。
        let window = inner.scheduling().concurrency.max(1);
        let batch_blocks = request_services.batch_blocks.max(1);
        let batch_budget = TranslationBatchBudget::new(
            request_services.generation.max_output_tokens,
            batch_blocks,
        );
        // 预算装不下、留给下一批的那一块。它的计划（含 `ItemStarted` 读数与
        // `translation_block_start` 日志）已经做过了，重做会重复记账，所以连它的冻结
        // 输入一起带过去，下一批直接用。
        let mut carried: Option<&TranslationBlock> = None;
        'window: while in_flight.len() + resolved.len() < window && frontier < total {
            // 攒一批：一个请求最多装「每请求文本块数」块，只装同一个内容单元的块，
            // 装到预算上限为止。缓存命中与「不必处理」的块不进批，也不打断这一批。
            let mut pending: Vec<TranslationBatchBlock<'_>> = Vec::new();
            let mut packer = TranslationBatchPacker::new(batch_budget);
            loop {
                // 这一块：通常现规划（缓存命中、跳过、重试范围之外都在这里定），
                // 预算或单元拒收过的那一块直接复用上一次的计划。
                let block = match carried.take() {
                    Some(block) => block,
                    None => {
                        if frontier >= total {
                            break;
                        }
                        let ordinal = frontier;
                        // The control check, the decision and the cursor write share one
                        // transition: releasing the lock in between would let a settings
                        // change (for example a per-block retry that resets the task)
                        // install a new cursor, only for this round to overwrite it with
                        // the old one.
                        let _transition =
                            match lock_transitions(inner, &mut in_flight, &mut resolved).await? {
                                TransitionLock::Held(guard) => guard,
                                TransitionLock::Stopped(stop) => {
                                    return finish_stopped_run(
                                        inner,
                                        job,
                                        run,
                                        &mut in_flight,
                                        stop,
                                    )
                                    .await;
                                }
                            };
                        if let Some(outcome) = requested_outcome(inner, job, &run.cursor).await? {
                            return run.interrupt(inner, job, outcome).await;
                        }
                        if !source_is_current(inner, job, revision).await? {
                            let outcome = run.stopped(StopRequest::SourceChanged);
                            return run.interrupt(inner, job, outcome).await;
                        }
                        match plan_translation_block(
                            inner,
                            job,
                            &book_id,
                            translation,
                            target_language,
                            &blocks,
                            ordinal,
                            revision,
                            &retry_targets,
                            retry_last,
                            run.cursor.force_retry,
                            &mut unit_cache,
                            stats,
                        )
                        .await?
                        {
                            // Nothing to send and nothing to publish: outside the retried set.
                            BlockPlan::Pass | BlockPlan::NoSegments => {
                                resolved.insert(ordinal, BlockSlot::Pass);
                                frontier += 1;
                                continue;
                            }
                            BlockPlan::Cached(stored) => {
                                resolved.insert(ordinal, BlockSlot::Cached(*stored));
                                frontier += 1;
                                continue;
                            }
                            // 重试范围之外还没有译文的块：不发请求，也绝不提交。停止打开
                            // 窗口，把已经解析出来的槽位提交完之后收尾。
                            BlockPlan::Untouched => {
                                retry_stopped = true;
                                break;
                            }
                            BlockPlan::Request => &blocks[ordinal],
                        }
                    }
                };
                let ordinal = block.ordinal;
                // 同一个内容单元（同一章）的块才能进同一批：`book`/`chapter` 语境字段
                // 是每批一份，混进另一章的块会让这两个字段撒谎。
                if let Some(first) = pending.first()
                    && first.unit_id != block.unit_id
                {
                    carried = Some(block);
                    break;
                }
                let context = block.translation_context();
                let estimate = estimate_batch_block(&block.source, &context);
                if !packer.accepts(&estimate) {
                    // 这一批装不下：留给下一批。这一块还没有打开窗口，游标里没有它 ——
                    // 停下来绝不会留下「处理中但没有请求」的块。
                    carried = Some(block);
                    break;
                }
                // The window is part of the durable cursor: the task window
                // reads it as 「处理中」, so it is published before the request
                // goes out.
                let _transition =
                    match lock_transitions(inner, &mut in_flight, &mut resolved).await? {
                        TransitionLock::Held(guard) => guard,
                        TransitionLock::Stopped(stop) => {
                            return finish_stopped_run(inner, job, run, &mut in_flight, stop).await;
                        }
                    };
                run.cursor.open_inflight(ordinal);
                run.persist(inner, job).await?;
                packer.push(&estimate);
                pending.push(TranslationBatchBlock {
                    ordinal,
                    unit_id: &block.unit_id,
                    source: &block.source,
                    context,
                    scope: run.scope(ordinal),
                });
                frontier += 1;
                if pending.len() >= batch_blocks {
                    break;
                }
            }
            if retry_stopped {
                // 收窄的重试在范围之外撞上一个还没译文的块：不再打开新请求。已经攒下但
                // 还没发出去的这一批随之作废（那些块的游标窗口由收尾统一清空），它们没有
                // 发过请求，也没有花掉任何输出预算。
                break 'window;
            }
            if pending.is_empty() {
                break 'window;
            }
            if pending.len() > 1 {
                stats.batched_requests += 1;
                stats.batched_blocks += pending.len();
            }
            spawn_translation_request(
                &mut in_flight,
                inner,
                job,
                &request_services,
                target_language,
                source_language.as_deref(),
                pending,
            );
        }
        // 3) Nothing is open and nothing is left to commit: every ordinal of the
        //    window was applied in order. The check has to cover `resolved` too —
        //    a round whose blocks all came from the cache (or had nothing to
        //    translate) opens no request while still holding blocks to commit, so
        //    breaking on the empty window alone ends the run 「succeeded」 without
        //    committing anything and leaves the durable cursor where it was. That
        //    is exactly the「点恢复后一会儿就完成、文本块一个都没处理」report:
        //    the task window reads 已处理 from the cursor, so it stays at 0.
        //    （现场 2026-09-18：模型或端点变化把游标从书首重扫时，前几块全是
        //    缓存命中，任务立刻按 Succeeded 收尾，只有清空后的游标没有前进。）
        if in_flight.is_empty() && resolved.is_empty() {
            break;
        }
        // 4) Wait for the next block to come back. A round that only resolved
        //    blocks has nothing to wait for; the next iteration commits those
        //    slots in ordinal order and fills the window again.
        if in_flight.is_empty() {
            continue;
        }
        let Some((ordinals, walk)) = in_flight.next().await else {
            continue;
        };
        // A stop aborts the walk where it arrives, even while earlier blocks
        // are still waiting: nothing in flight is committed by this run.
        if let Some(stop) = absorb_translation_step(&mut resolved, ordinals, walk)? {
            return finish_stopped_run(inner, job, run, &mut in_flight, stop).await;
        }
    }
    if retry_stopped {
        // 收窄的重试在范围之外撞上一个还没翻译的块：这一块既不在本次重试里（不许替它
        // 发请求），也没有译文（不许被记成已处理）。运行停在这里，窗口清空之后按「暂停」
        // 发布 —— 任务留在可继续的状态，而重试集合在上面最后一次提交时已经清掉，用户点
        // 「恢复」时那次运行会按整本书继续翻。绝不能按 Succeeded 收尾：那是「整本都处理
        // 过了」的谎话，也正是这次现场里「任务已完成、阅读端没有译文」的来源。
        tracing::info!(
            target: "ngy_ai",
            stage = "translation_retry_incomplete",
            next_ordinal = run.cursor.next_ordinal,
            total_blocks = total,
            "Narrowed translation retry stopped before the end of the book"
        );
        let _transition = inner.transitions.lock().await;
        return run
            .interrupt(inner, job, RunOutcome::Paused(run.cursor.clone()))
            .await;
    }
    if untranslated_blocks > 0 {
        // The per-block warnings above are the durable record; this summary keeps
        // the console honest about a run that finished with partial text.
        tracing::warn!(
            target: "ngy_ai",
            stage = "translation_run_untranslated",
            untranslated = untranslated_blocks,
            total_blocks = total,
            translated = saved_blocks,
            "Translation run finished with untranslated text blocks"
        );
    }
    // Every block the window opened was committed: nothing may still be in flight.
    debug_assert!(run.cursor.inflight.is_empty(), "翻译运行结束时窗口必须为空");
    Ok(RunOutcome::Succeeded(run.cursor.clone()))
}

/// 一发请求的逐块结果归位。
///
/// `Ok(None)`：这一发只是把块放回窗口，提交仍按块序号进行。`Ok(Some(stop))`：这一发要求
/// 停止整次运行，由调用方收尾 —— 停止要清空窗口并落盘，而落盘要拿门闩，所以不在归位里做。
fn absorb_translation_step(
    resolved: &mut BTreeMap<usize, BlockSlot>,
    ordinals: Vec<usize>,
    walk: TranslationStepResult,
) -> Result<Option<StopRequest>> {
    match walk {
        // 一发请求的逐块结果：按块号归位，提交仍是按块序号的。
        Ok(BlockStep::Value(walks)) => {
            for (ordinal, walk) in walks {
                let slot = match walk {
                    Ok(BlockWalk::Stored(stored, block)) => BlockSlot::Translated(stored, block),
                    // 单块回退里的停止已经作为这一发的停止返回，这里只做兜底。
                    Ok(BlockWalk::Stopped(stop)) => return Ok(Some(stop)),
                    // 个别的失败（协议不合，或这一块的请求本身失败）只跳过这一块，
                    // 同批其它块照常落库；数据库、IO、取消类错误不是某一块的毛病，
                    // 照旧让整次运行失败。
                    Err(error) => match block_skip_cause(&error) {
                        Some((cause, kind)) => BlockSlot::Untranslated(cause, kind),
                        None => return Err(error),
                    },
                };
                resolved.insert(ordinal, slot);
            }
            Ok(None)
        }
        // A stop aborts the walk where it arrives, even while earlier blocks are
        // still waiting: nothing in flight is committed by this run.
        Ok(BlockStep::Stopped(stop)) => Ok(Some(stop)),
        // 这一发请求本身失败（传输、超时、流协议、超出上下文或响应上限）：它带走的
        // 每一块都没有译文 —— 与单块路径同一条规则，逐块跳过（或让整次运行失败）。
        // 连续失败照旧由 walk 的冷却兜住：批的失败一次记一批的块，冷却来得更快，
        // 这也正是端点真出问题时应有的反应。
        Err(error) => match block_skip_cause(&error) {
            Some((cause, kind)) => {
                for ordinal in ordinals {
                    resolved.insert(ordinal, BlockSlot::Untranslated(cause, kind));
                }
                Ok(None)
            }
            None => Err(error),
        },
    }
}

/// 一发请求要求停止整次运行：先丢掉窗口，再清空持久窗口并按终态发布。
///
/// 次序不能反。等门闩的是这个 walk，而窗口里的请求也可能排在同一把门闩上（收流前后的
/// 控制检查），它们只有这个 walk 会轮询 —— 先等锁就会和它们互相等待（见
/// [`lock_transitions`]）。运行本来就要停，在飞的块不会被提交、下一次运行重做，所以现在
/// 丢掉与收尾时丢掉没有区别。
async fn finish_stopped_run(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    run: &mut TranslationRun,
    in_flight: &mut FuturesUnordered<TranslationRequestFuture<'_>>,
    stop: StopRequest,
) -> Result<RunOutcome> {
    in_flight.clear();
    if stop != StopRequest::Abandoned {
        let _transition = inner.transitions.lock().await;
        run.settle(inner, job).await?;
    }
    Ok(run.stopped(stop))
}

/// 等 `transitions` 门闩时窗口的处置结果。
enum TransitionLock<'a> {
    /// 拿到门闩：调用方立刻做它那段「控制检查 + 写游标」的工作。
    Held(tokio::sync::MutexGuard<'a, ()>),
    /// 等锁期间窗口里的一发请求要求停止：调用方按正常路径收尾
    /// （[`finish_stopped_run`]）。
    Stopped(StopRequest),
}

/// 取 `transitions` 门闩，等锁的同时继续驱动窗口。
///
/// 窗口里的请求 future 只有这个 walk 会轮询（它们就在 `in_flight` 里），而请求自己也会排
/// 在同一把门闩上（收流前后的控制检查拿这把锁读「还在被需要吗」）。于是「walk 干等门闩 +
/// 请求排在它前面」就是死锁：请求等着被轮询，walk 等着请求放锁 —— 2026-10-16 两个并发
/// 槽位的整本翻译就是这样永久停住的：第二发批请求在收流后的控制检查上排队，walk 的提交
/// 紧接着排在同一把锁上，此后没有任何东西再被轮询，任务窗口里「处理中」的四块再也不动。
///
/// 所以等锁与收窗口放在同一个 future 里：门闩一空就立刻进去，被占着就顺手把已经就绪的
/// 请求归位（提交仍按块序号）。窗口为空时这里退化成原来的 `lock()` —— 没有请求会再排队，
/// 等锁不再有第二方依赖这个 walk。
async fn lock_transitions<'a, 'b>(
    inner: &'a Arc<IndexingInner>,
    in_flight: &mut FuturesUnordered<TranslationRequestFuture<'b>>,
    resolved: &mut BTreeMap<usize, BlockSlot>,
) -> Result<TransitionLock<'a>> {
    let acquire = inner.transitions.lock();
    tokio::pin!(acquire);
    // 窗口空了之后 `in_flight.next()` 会立刻返回 `None`，再轮询它就只是空转。
    let mut window_open = !in_flight.is_empty();
    loop {
        tokio::select! {
            biased;
            // 先收窗口：排在同一把门闩上的请求只有这里会轮询它。
            step = in_flight.next(), if window_open => {
                let Some((ordinals, walk)) = step else {
                    window_open = false;
                    continue;
                };
                match absorb_translation_step(resolved, ordinals, walk) {
                    Ok(Some(stop)) => return Ok(TransitionLock::Stopped(stop)),
                    Ok(None) => {}
                    Err(error) => return Err(error),
                }
            }
            guard = &mut acquire => return Ok(TransitionLock::Held(guard)),
        }
    }
}

/// 一发请求的逐块结论：块号与那一块自己的结果一一对应。
///
/// 「每请求文本块数」大于 1 时一发请求带走多块，所以结果不再是一块对一发；块级失败按
/// `Err` 原样带出来，由 [`absorb_translation_step`] 归位或跳过。
type TranslationStepResult = Result<BlockStep<Vec<(usize, Result<BlockWalk>)>>>;

/// 一轮里打开的一个请求：它覆盖的块号，以及每一块的逐块结果（与块号一一对应）。
///
/// 一个请求现在可能带走多个块（端点级的「每请求文本块数」），所以结果不再是一块对一发：
/// 块号单独带出来，walk 据此把每一块放回 `resolved`，提交仍是按块序号的。
type TranslationRequestFuture<'a> = BoxFuture<'a, (Vec<usize>, TranslationStepResult)>;

/// 把攒好的一批发出去。
///
/// 「每请求文本块数」为 1 时这里就是老的「一块一请求」路径：`translate_block_batch`
/// 把手上的那**一**块交给单块路径，严格尝试、一次协议纠正、
/// `TRANSLATION_BLOCK_RETRIES` 轮重发与逐片段回退都在那边，行为与加批之前一致。
fn spawn_translation_request<'a>(
    in_flight: &mut FuturesUnordered<TranslationRequestFuture<'a>>,
    inner: &Arc<IndexingInner>,
    job: &'a db::index_jobs::IndexJob,
    services: &TranslationServices,
    target_language: &'a str,
    source_language: Option<&'a str>,
    blocks: Vec<TranslationBatchBlock<'a>>,
) {
    let ordinals = blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>();
    let inner = Arc::clone(inner);
    let services = services.clone();
    in_flight.push(
        async move {
            let walk = translate_block_batch(
                &inner,
                job,
                &services,
                target_language,
                source_language,
                &blocks,
            )
            .await;
            // 这一发收尾后才轮到本槽位休息，所以休眠永远发生在请求之后，不会挤占
            // 模型的响应时间。批里的块共用这一次休息：间隔按请求计，不按块计。
            pause_after_translation_request(&inner, &walk).await;
            (ordinals, walk)
        }
        .boxed(),
    );
}

/// 一发模型请求收尾后的块级节流，也就是「任务间隔」真正生效的地方。
///
/// 间隔原本只作用于两个任务之间（`worker_loop`），而一整本书就是一个任务：书内几十上百
/// 次请求之间毫无停顿，本机模型（Ollama 等）会连续把 GPU 吃到满载 —— 现场 2026-09-20，
/// 用户把间隔调成 5000 毫秒也不起作用，持续满载时显示器黑屏。现在按请求计：每个并发
/// 槽位发出一发就休眠一次，窗口仍按「任务并发」填，整体请求频率因此被限制在
/// 「每个槽位一发 / 间隔」。缓存命中与「不必处理」的块不发请求，也就不会休眠。
///
/// 停止请求（暂停、取消、换版本）不休眠：这类结果要求马上把控制权交回 `walk`，
/// 让它清空窗口并按终态发布，多睡一个间隔只会拖慢用户的暂停操作。
async fn pause_after_translation_request<T>(
    inner: &Arc<IndexingInner>,
    walk: &Result<BlockStep<T>>,
) {
    if matches!(walk, Ok(BlockStep::Stopped(_))) {
        return;
    }
    let interval = inner.scheduling().interval;
    if !interval.is_zero() {
        tokio::time::sleep(interval).await;
    }
}

/// 连续请求失败凑满阈值后的一次冷却：等一段时间再继续开新请求。
///
/// 端点临时不可用（本机模型重启、网关限流、地址暂时不通）时，停一下往往就能继续翻完
/// 后面的块；这段时间一个请求都不发，但暂停、取消与换版本照常生效 —— 按
/// `CONTROL_POLL_INTERVAL` 醒来复查控制请求，命中就立刻把控制权交回 walk（返回
/// `Some(outcome)`，由调用方按终态返回），不睡满整段冷却。
///
/// 冷却结束返回 `None`，调用方随即把连续失败计数清零：下一组连续失败重新累计，
/// 再凑满阈值就再冷却一次。
async fn cool_down_after_request_failure_streak(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    run: &mut TranslationRun,
    in_flight: &mut FuturesUnordered<TranslationRequestFuture<'_>>,
    resolved: &mut BTreeMap<usize, BlockSlot>,
    cooldown: Duration,
) -> Result<Option<RunOutcome>> {
    if cooldown.is_zero() {
        return Ok(None);
    }
    // 进任务日志：用户需要在任务窗口里看到「任务不是死了，是在等端点缓过来」，
    // 而不是一个只有运行日志才知道原因的静止进度。
    record_index_event(
        inner,
        job,
        JobLogEvent::Cooldown,
        JobLogMetrics {
            ordinal: Some(run.cursor.next_ordinal as u64),
            duration_ms: Some(cooldown.as_millis() as u64),
            ..JobLogMetrics::default()
        },
    );
    tracing::info!(
        target: "ngy_ai",
        stage = "translation_failure_cooldown_start",
        cooldown_ms = cooldown.as_millis() as u64,
        next_ordinal = run.cursor.next_ordinal,
        "Cooling down after consecutive model request failures"
    );
    let mut remaining = cooldown;
    while !remaining.is_zero() {
        let step = remaining.min(CONTROL_POLL_INTERVAL);
        tokio::time::sleep(step).await;
        remaining = remaining.saturating_sub(step);
        // 冷却期间窗口里可能还有请求在飞：这里的复查同样不能干等门闩
        // （见 `lock_transitions`）。
        let _transition = match lock_transitions(inner, in_flight, resolved).await? {
            TransitionLock::Held(guard) => guard,
            TransitionLock::Stopped(stop) => {
                return finish_stopped_run(inner, job, run, in_flight, stop)
                    .await
                    .map(Some);
            }
        };
        if let Some(outcome) = requested_outcome(inner, job, &run.cursor).await? {
            return Ok(Some(run.interrupt(inner, job, outcome).await?));
        }
        if !source_is_current(inner, job, run.cursor.revision).await? {
            let outcome = run.stopped(StopRequest::SourceChanged);
            return Ok(Some(run.interrupt(inner, job, outcome).await?));
        }
    }
    tracing::info!(
        target: "ngy_ai",
        stage = "translation_failure_cooldown_end",
        next_ordinal = run.cursor.next_ordinal,
        "Translation failure cooldown finished; continuing"
    );
    Ok(None)
}

/// 这次运行接下来该用的翻译服务：库里最新的一份，前提是它属于同一套引擎。
///
/// `None` 表示保持现在这一份：没有配置翻译，或者模型、执行身份、目标语言里有一项与本次
/// 运行不同。后者（改地址、换模型或换目标语言）不是「换一份凭据」，而是一次会重置任务行的
/// 重配置：本次运行会在下一个控制检查里被判成已被替换（`Abandoned`），重新领取时才会按新
/// 引擎继续；绝不能在这里悄悄换身份，把两种来源的译文混进同一次运行。
///
/// 同一套引擎下换 provider（例如只换了 API key 或超时）或换生成参数必须立刻生效：这正是
/// 用户保存 Endpoint 设置之后期望看到的行为（现场 2026-09-24：改完 Endpoint 与 API key，
/// 正在跑的任务仍用旧值翻完剩下的块）。
fn refreshed_translation_services(
    inner: &Arc<IndexingInner>,
    current: &TranslationServices,
) -> Option<TranslationServices> {
    let fresh = inner.models_snapshot().translation?;
    let same_engine = fresh.model == current.model
        && fresh.execution_identity == current.execution_identity
        && fresh.target_language == current.target_language;
    same_engine.then_some(fresh)
}

/// Remembers one committed translation so a later block with the same text hits it
/// instead of the provider, exactly like the cache rows read from the database.
pub(crate) fn keep_in_unit_cache(
    unit_cache: &mut HashMap<String, HashMap<String, StoredTranslation>>,
    block: &TranslationBlock,
    stored: StoredTranslation,
) {
    unit_cache
        .entry(block.unit_id.clone())
        .or_default()
        .insert(block_cache_key(&block.source), stored);
}

pub(crate) fn block_cache_key(source: &TranslationSource) -> String {
    translation_cache_key(&source.text, source.segments.iter().map(String::as_str))
}

/// 一个块的失败该跳过还是该让整次运行失败。
///
/// `Some(..)` 表示这一块按跳过处理，原文保留、游标前进、后面的块照常翻：
///
/// - `SkipCause::Protocol`：模型答了、但形状不合格（`ResponseError`）；
/// - `SkipCause::Request`：这一发请求/这一份响应不行 —— 传输、超时、流协议、上下文
///   超限，以及「响应超过大小上限」这类没有更细分类的响应级错误。
///
/// `None` 表示不是某一块的毛病，照旧让整次运行失败：数据库、IO 与取消类错误跳过只会
/// 把损坏的结果铺满整本书，重试或换模型也解决不了它们。
pub(crate) fn block_skip_cause(error: &anyhow::Error) -> Option<(SkipCause, JobLogErrorKind)> {
    if error
        .downcast_ref::<crate::ai::translation::ResponseError>()
        .is_some()
    {
        return Some((SkipCause::Protocol, classify_error(error)));
    }
    let kind = classify_error(error);
    if matches!(
        kind,
        JobLogErrorKind::Database | JobLogErrorKind::Io | JobLogErrorKind::Cancelled
    ) {
        return None;
    }
    Some((SkipCause::Request, kind))
}

/// Decides what one ordinal of the window needs.
///
/// A block whose text was translated before is republished instead of being sent
/// again: the lookup is by content inside the block's own unit, so a paragraph an
/// edit moved (new block id, same text) still finds its row. The cache is read as
/// it is when the block enters the window, so two identical blocks inside one
/// window are both sent — the second one just writes the same text over its own
/// row.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_translation_block(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    book_id: &str,
    translation: &TranslationServices,
    target_language: &str,
    blocks: &[TranslationBlock],
    ordinal: usize,
    revision: u64,
    retry_targets: &HashSet<usize>,
    retry_last: Option<usize>,
    force_retry: bool,
    unit_cache: &mut HashMap<String, HashMap<String, StoredTranslation>>,
    stats: &mut TranslationRunStats,
) -> Result<BlockPlan> {
    // 出了本次重试范围（最后一个目标之后）的块不能再被「跳过」：跳过它等于把它记成
    // 已处理，而它可能从来没有译文。那里只允许走缓存命中，没有译文就由调用方停下运行。
    let beyond_retry = retry_last.is_some_and(|last| ordinal > last);
    // Not part of a per-block retry, and still inside the retried span: walk past it
    // without a provider request. Its stored translation is untouched, so nothing has
    // to be republished.
    if !retry_targets.is_empty() && !retry_targets.contains(&ordinal) && !beyond_retry {
        return Ok(BlockPlan::Pass);
    }
    let block = &blocks[ordinal];
    // 范围之外的块不是这次运行「开始处理」的块：不许留下 item_started 这种读数。
    if !beyond_retry {
        record_index_event(
            inner,
            job,
            JobLogEvent::ItemStarted,
            JobLogMetrics {
                ordinal: Some(block.ordinal as u64),
                total: Some(blocks.len() as u64),
                expected_count: Some(block.source.segments.len() as u64),
                ..JobLogMetrics::default()
            },
        );
        tracing::debug!(
            target: "ngy_ai",
            stage = "translation_block_start",
            block_ordinal = block.ordinal,
            block_id = %block.block_id,
            total_blocks = blocks.len(),
            unit_ordinal = block.unit_ordinal,
            segments = block.source.segments.len(),
            multi_segment = block.source.segments.len() > 1,
            segment_chars = %translation_segment_chars(&block.source.segments),
            source_chars = block.source.text.chars().count(),
            request_bytes = block.source.request_input(&block.translation_context()).len(),
            "Translation block started"
        );
    }
    if block.source.segments.is_empty() {
        tracing::debug!(
            target: "ngy_ai",
            stage = "translation_block_skipped",
            block_ordinal = block.ordinal,
            reason = "no_translatable_segments",
            "Translation block skipped"
        );
        stats.no_segments += 1;
        record_index_event(
            inner,
            job,
            JobLogEvent::NoTranslatableSegments,
            JobLogMetrics {
                ordinal: Some(block.ordinal as u64),
                actual_count: Some(0),
                ..JobLogMetrics::default()
            },
        );
        return Ok(BlockPlan::NoSegments);
    }
    if !unit_cache.contains_key(&block.unit_id) {
        let unit_id = block.unit_id.clone();
        let unit_revision = block.unit_revision;
        let language = target_language.to_string();
        let model = translation.model.clone();
        let execution_identity = translation.execution_identity.clone();
        let book_id = book_id.to_string();
        let db_path = inner.db_path.clone();
        let stored = run_db(db_path, move |conn| {
            current_translation_blocks(
                &conn,
                &book_id,
                &unit_id,
                &language,
                revision,
                unit_revision,
                &model,
                &execution_identity,
            )
        })
        .await?;
        unit_cache.insert(block.unit_id.clone(), stored);
    }
    let cache_key = block_cache_key(&block.source);
    let cached = unit_cache
        .get(&block.unit_id)
        .and_then(|unit| unit.get(&cache_key))
        .cloned();
    // 阅读端逐块「重新翻译」把这一块标成强制重发：命中缓存也要重新请求一次，新译文
    // 覆盖旧行（用户对译文不满意、或换了更好的模型重做这一块）。
    let forced = force_retry && retry_targets.contains(&ordinal);
    match &cached {
        Some(stored) if !forced && !stored.is_passthrough() => {
            return Ok(BlockPlan::Cached(Box::new(stored.clone())));
        }
        Some(_) if !forced => {
            // 模型把整块原样抄了回来：这一块其实还没有译文（现场 2026-10-15，阅读端
            // 显示的仍是英文）。它不再算缓存命中，继续往下走请求路径重新翻一次。
            // 只在本次运行真的要处理它时留读数：重试范围之外的块既不请求也不记录。
            if !beyond_retry {
                tracing::info!(
                    target: "ngy_ai",
                    stage = "translation_source_echoed",
                    block_ordinal = block.ordinal,
                    "Model echoed the source text back; treating the block as untranslated"
                );
                record_index_event(
                    inner,
                    job,
                    JobLogEvent::SourceEchoed,
                    JobLogMetrics {
                        ordinal: Some(block.ordinal as u64),
                        ..JobLogMetrics::default()
                    },
                );
            }
        }
        _ => {}
    }
    if beyond_retry {
        // 不在本次重试里、又没有译文：既不发请求，也不许提交。调用方会停在
        // 这一块上，任务因此留在「还没翻完、可以继续」的状态。
        tracing::info!(
            target: "ngy_ai",
            stage = "translation_retry_stop_block",
            block_ordinal = block.ordinal,
            total_blocks = blocks.len(),
            "Narrowed retry reached an untranslated block outside its targets"
        );
        return Ok(BlockPlan::Untouched);
    }
    Ok(BlockPlan::Request)
}
