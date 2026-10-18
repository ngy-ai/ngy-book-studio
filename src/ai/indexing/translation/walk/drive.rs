//! The main loop: commit in ordinal order, fill the window, wait for the next
//! request, and publish the run's final outcome.

use super::*;

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
            if retry_stopped {
                // 收窄的重试已经在范围之外撞线：不再开新批，但**已经攒下的这一批照发** ——
                // 它的块全在重试范围内，请求本来就该发。以前这里把整批作废，于是「每请求
                // 文本块数」大于重试块数时整次重试一个请求都发不出去（现场 2026-10-17：
                // 立即翻译本章的重试范围有 22 块要发，端点上限 32，批永远攒不满就撞线，
                // 任务窗口里「处理中 22」却没有任何请求）。
                // 收尾等窗口清空之后再进行：在飞的块不会被提交，下一次运行重做。
                break 'window;
            }
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
            if retry_stopped {
                // 撞线时一批都没发出去（`pending` 为空）：没有在飞请求可等，必须直接走
                // 下面的收尾发布。在这里 `continue` 会跳过那段发布 —— 此后每一轮既不能
                // 提交（游标不许越过窗口里还在等的块）也不能再开窗口（窗口已满），main
                // loop 于是变成一个没有任何 await 点的 100% 空转：任务永远停在「运行中」
                // 并带着一批幽灵「处理中」块，重启也只会在同一处复现（现场 2026-10-17）。
                break;
            }
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
