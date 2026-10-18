//! The transition gate while the window keeps moving: absorbing one request's
//! per-block results, settling a stopped run, and the failure-streak cooldown.

use super::*;

/// 一发请求的逐块结果归位。
///
/// `Ok(None)`：这一发只是把块放回窗口，提交仍按块序号进行。`Ok(Some(stop))`：这一发要求
/// 停止整次运行，由调用方收尾 —— 停止要清空窗口并落盘，而落盘要拿门闩，所以不在归位里做。
pub(crate) fn absorb_translation_step(
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
        // A stop aborts the walk where it arrives, even while earlier blocks
        // are still waiting: nothing in flight is committed by this run.
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
pub(crate) async fn finish_stopped_run(
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
pub(crate) enum TransitionLock<'a> {
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
pub(crate) async fn lock_transitions<'a, 'b>(
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

/// 连续请求失败凑满阈值后的一次冷却：等一段时间再继续开新请求。
///
/// 端点临时不可用（本机模型重启、网关限流、地址暂时不通）时，停一下往往就能继续翻完
/// 后面的块；这段时间一个请求都不发，但暂停、取消与换版本照常生效 —— 按
/// `CONTROL_POLL_INTERVAL` 醒来复查控制请求，命中就立刻把控制权交回 walk（返回
/// `Some(outcome)`，由调用方按终态返回），不睡满整段冷却。
///
/// 冷却结束返回 `None`，调用方随即把连续失败计数清零：下一组连续失败重新累计，
/// 再凑满阈值就再冷却一次。
pub(crate) async fn cool_down_after_request_failure_streak(
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
