//! The job loop: how workers claim durable rows, wait for work, and dispatch
//! one claimed job to the pass that owns it.

use super::*;

use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use anyhow::Result;

use crate::{
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics, classify_error},
    db,
};

pub(crate) async fn worker_loop(inner: Arc<IndexingInner>, index: usize) {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            break;
        }
        // Workers above the configured concurrency stay parked. They poll once
        // per idle interval instead of scanning the queue or consuming a wake
        // permit that an active worker needs.
        if index >= inner.scheduling().concurrency {
            tokio::time::sleep(IDLE_POLL_INTERVAL).await;
            continue;
        }
        // 先推进翻译任务对账一步（一步一个单位）。队列里有**本计划之外**的排队
        // 工作时它返回 `Yielded`，这一轮就去跑那个任务；本计划自己刚插入的任务不算
        // 外部工作，它们排队不会阻塞后面的单位（见 `advance_translation_reconcile`）。
        match advance_translation_reconcile(&inner).await {
            Ok(TranslationReconcileStep::Advanced) => continue,
            Ok(TranslationReconcileStep::Yielded | TranslationReconcileStep::Idle) => {}
            Err(error) => tracing::warn!(%error, "翻译任务对账失败"),
        }

        let db_path = inner.db_path.clone();
        let jobs = match run_db(db_path, move |conn| {
            db::index_jobs::list_queued(&conn, QUEUE_SCAN_LIMIT)
        })
        .await
        {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(%error, "读取后台索引队列失败");
                wait_for_work(&inner).await;
                continue;
            }
        };

        let Some(job) = jobs.into_iter().next() else {
            wait_for_work(&inner).await;
            continue;
        };
        if let Err(error) = run_queued_job(&inner, job).await {
            tracing::error!(%error, "后台索引任务状态提交失败");
        }
        // Configurable pause between two jobs on the same worker; vision and
        // embedding work is paced by this alone. Whole-book translation has a
        // second, request-level pause of the same value (one per in-flight
        // block, see `pause_after_translation_request`), because one book is a
        // single job and a job-level pause cannot pace anything inside it.
        let interval = inner.scheduling().interval;
        if !interval.is_zero() {
            tokio::time::sleep(interval).await;
        }
    }
}

/// 分步对账的一次推进结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TranslationReconcileStep {
    /// 这一步处理了一个对账单位；计划里可能还有下一步。
    Advanced,
    /// 队列里已经有排队任务：这一步先不推进，把执行权让给它。
    Yielded,
    /// 没有待推进的计划：没有配置翻译，或者任务已经与当前配置一致。
    Idle,
}

/// 推进翻译任务对账的一步：一次只处理一个单位，动手之前先看队列。
///
/// 「一步一个单位」是 2026-10-15 的现场要求：库变更后的对账要遍历整个书库，原先
/// 一次事务做完，期间新入队的任务只能干等。现在每个单位一个小事务（
/// [`db::transactions::apply_translation_reconcile_unit`]），每步之前重新扫描队列，
/// 排队中的**外部**工作因此可以先跑。
///
/// 让路只针对本计划之外的任务：本计划自己插入/重置的任务同样排进队列，它们是这一步的
/// 产物，给它们让路等于让一次对账等整本书翻完，其余单位就再也建不出来。计划之外的
/// 任务（例如同时排队的 embedding、视觉，或用户手动排的翻译）会让这一步返回
/// [`TranslationReconcileStep::Yielded`]。
///
/// 计划在第一次推进时按当时的配置构建；`reconcile_translations` 再次置位（库或配置
/// 又变了）时丢弃重建 —— 已处理的单位在新计划里因为行已经对齐而不会出现。
pub(crate) async fn advance_translation_reconcile(
    inner: &Arc<IndexingInner>,
) -> Result<TranslationReconcileStep> {
    if !inner.reconcile_translations.load(Ordering::Acquire) && !inner.has_translation_reconcile() {
        // 没有待办也没有计划：这一步不碰数据库（worker 每轮都会经过这里）。
        return Ok(TranslationReconcileStep::Idle);
    }

    // 让路：队列里有本计划之外的任务时，本步不推进（调用方下一件事就是跑它）。
    // 注意这里**不消费**对账请求：让路之后请求仍然挂着，外部任务跑完再推进。
    let db_path = inner.db_path.clone();
    let queued = run_db(db_path, move |conn| db::index_jobs::list_queued(&conn, 1)).await?;
    if let Some(job) = queued.first()
        && !inner.reconcile_plan_owns(&job.id)
    {
        return Ok(TranslationReconcileStep::Yielded);
    }

    if inner.reconcile_translations.swap(false, Ordering::AcqRel) {
        // 库或配置刚变过：丢弃旧计划，下一次按当前状态重建。已处理的单位在新计划里
        // 因为行已经对齐而不会出现，所以丢弃不会重复劳动。
        inner.clear_translation_reconcile();
    }

    if !inner.has_translation_reconcile() {
        let Some(translation) = inner.models_snapshot().translation else {
            return Ok(TranslationReconcileStep::Idle);
        };
        let db_path = inner.db_path.clone();
        let units = run_db(db_path, move |conn| {
            db::transactions::plan_translation_reconcile(
                &conn,
                translation.target_language.as_deref(),
                &translation.model,
                &translation.execution_identity,
                translation.auto_run,
            )
        })
        .await?;
        if units.is_empty() {
            return Ok(TranslationReconcileStep::Idle);
        }
        inner.set_translation_reconcile(units);
    }

    let Some(unit) = inner.take_translation_reconcile_unit() else {
        return Ok(TranslationReconcileStep::Idle);
    };
    let db_path = inner.db_path.clone();
    let now = unix_timestamp()?;
    let changed = run_db_mut(db_path, move |mut conn| {
        db::transactions::apply_translation_reconcile_unit(&mut conn, &unit, now)
    })
    .await?;
    if changed != 0 {
        // 新建/重置的任务可能就是下一个要跑的工作：叫醒还在等信号的 worker。
        inner.wake.notify_one();
    }
    Ok(TranslationReconcileStep::Advanced)
}

/// 静默看门狗的一个周期：任务被领取后超过 [`NO_PROGRESS_WATCHDOG_THRESHOLD`]
/// 没有任何新事件（进展）时，往任务日志写一条「已领取但无进展」。
///
/// 看门狗只报告、不干预：它不取消、不重排任务，只是让「任务显示运行中但处理中恒为 0」
/// 这类现场（2026-10-15 用户反馈）在任务窗口里有据可查，而不是一片沉默。写入的事件
/// 走 [`record_index_event`]，因此它同时刷新观察时间戳：同一段无进展每个阈值周期最多
/// 报一条，不会刷屏。
pub(crate) async fn watchdog_tick(inner: &Arc<IndexingInner>) -> Result<()> {
    let Some((job_id, idle)) = inner.progress_idle() else {
        return Ok(());
    };
    if !no_progress_exceeded(idle) {
        return Ok(());
    }
    let db_path = inner.db_path.clone();
    let lookup = job_id.clone();
    let Some(job) = run_db(db_path, move |conn| db::index_jobs::get(&conn, &lookup)).await? else {
        return Ok(());
    };
    if job.status != db::index_jobs::IndexJobStatus::Running {
        return Ok(());
    }
    let ordinal = serde_json::from_str::<JobCursor>(&job.cursor_json)
        .ok()
        .map(|cursor| cursor.next_ordinal as u64);
    record_index_event(
        inner,
        &job,
        JobLogEvent::ClaimedNoProgress,
        JobLogMetrics {
            ordinal,
            duration_ms: Some(idle.as_millis() as u64),
            ..JobLogMetrics::default()
        },
    );
    Ok(())
}

pub(crate) async fn watchdog_loop(inner: Arc<IndexingInner>) {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = watchdog_tick(&inner).await {
            tracing::warn!(%error, "后台任务看门狗检查失败");
        }
        tokio::time::sleep(NO_PROGRESS_WATCHDOG_INTERVAL).await;
    }
}

/// 静默看门狗的判定线。单独成函数是为了把边界钉在测试里 —— 差一毫秒就该继续等，
/// 到达就该报告。
pub(crate) fn no_progress_exceeded(idle: Duration) -> bool {
    idle >= NO_PROGRESS_WATCHDOG_THRESHOLD
}

pub(crate) async fn wait_for_work(inner: &IndexingInner) {
    tokio::select! {
        _ = inner.wake.notified() => {},
        _ = tokio::time::sleep(IDLE_POLL_INTERVAL) => {},
    }
}

pub(crate) async fn run_queued_job(
    inner: &Arc<IndexingInner>,
    scheduled: db::index_jobs::IndexJob,
) -> Result<()> {
    let transition = inner.transitions.lock().await;
    let scheduled_id = scheduled.id;
    let db_path = inner.db_path.clone();
    let Some(mut job) = run_db(db_path, move |conn| {
        db::index_jobs::get(&conn, &scheduled_id)
    })
    .await?
    else {
        return Ok(());
    };
    if job.status != db::index_jobs::IndexJobStatus::Queued {
        return Ok(());
    }
    let models = inner
        .models
        .read()
        .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?
        .clone();
    let canonical_kind = job.source_id.as_deref().and_then(|source_id| {
        (job.id == format!("{}:{source_id}", job.kind)
            && matches!(job.kind.as_str(), "embedding" | "vision"))
        .then_some(job.kind.as_str())
    });
    if let Some(kind) = canonical_kind {
        let (model, execution_identity) = if kind == "embedding" {
            (
                models.config.embedding_model.as_str(),
                models.config.embedding_execution_identity.as_str(),
            )
        } else {
            (
                models.config.vision_model.as_str(),
                models.config.vision_execution_identity.as_str(),
            )
        };
        let mut bound = JobCursor::from_job(&job, model, Some(execution_identity))?;
        if bound.input_execution_identity.take().is_some() {
            bound.next_ordinal = 0;
        }
        let cursor_json = bound.encode()?;
        if cursor_json != job.cursor_json {
            let job_id = job.id.clone();
            let expected_cursor_json = job.cursor_json.clone();
            let next_cursor_json = cursor_json.clone();
            let db_path = inner.db_path.clone();
            let now = unix_timestamp()?;
            if !run_db(db_path, move |conn| {
                db::index_jobs::bind_queued_canonical_execution(
                    &conn,
                    &job_id,
                    &expected_cursor_json,
                    &next_cursor_json,
                    now,
                )
            })
            .await?
            {
                return Ok(());
            }
            job.cursor_json = cursor_json;
        }
    }
    // 翻译任务的持久游标由两个写入者产出：执行器写完整的块级游标，重配置路径写
    // 「较小的持久化形状」（没有 `retry_ordinals`/`inflight`；`blocks_version` 由它
    // 从原游标搬运，见 `translation_cursor_json`）。而这里的一切比较都是 JSON 字符串
    // 相等，所以必须在领取前重写成规范编码 —— 否则重配置刚建好的任务第一次运行就会
    // 判成 `Abandoned`：行留在 `running`、`attempts` 已经加一、没有任何执行者，任务
    // 窗口于是显示「运行中」而「处理中」恒为 0，点「恢复」也不会有任何进展
    // （现场 2026-09-17）。规范化本身不得改动 `next_ordinal`：换模型保位继续，靠的
    // 就是这次重写把位置和它的 `blocks_version` 一起原样编码出来。
    //
    // 未配置翻译模型时不绑定：那次运行会以「翻译模型尚未配置」失败并正常发布终态，
    // 而不是在这里悄悄不动。
    if job.kind == TRANSLATION_JOB_KIND
        && let Some(translation) = models.translation.as_ref()
    {
        let bound = JobCursor::from_job(
            &job,
            &translation.model,
            Some(&translation.execution_identity),
        )?;
        let cursor_json = bound.encode()?;
        if cursor_json != job.cursor_json {
            let job_id = job.id.clone();
            let expected_cursor_json = job.cursor_json.clone();
            let next_cursor_json = cursor_json.clone();
            let db_path = inner.db_path.clone();
            let now = unix_timestamp()?;
            if !run_db(db_path, move |conn| {
                db::index_jobs::bind_queued_translation_cursor(
                    &conn,
                    &job_id,
                    &expected_cursor_json,
                    &next_cursor_json,
                    now,
                )
            })
            .await?
            {
                return Ok(());
            }
            job.cursor_json = cursor_json;
        }
    }

    if job.cancel_requested {
        return publish_outcome(
            inner,
            &job,
            RunOutcome::Cancelled(cursor_without_model(&job)?, None),
            db::index_jobs::IndexJobStatus::Queued,
        )
        .await;
    }
    // 安全网：`request_pause` 现在会把排队中的行当场结清成 `paused`，正常不会再有
    // 「排队中 + 暂停请求」的行到这里；留着这一条是兜住旧构建写下的行（它们在
    // `recover_interrupted` 的启动修复之前也可能被这一轮撞见）。
    if job.pause_requested {
        return publish_outcome(
            inner,
            &job,
            RunOutcome::Paused(cursor_without_model(&job)?),
            db::index_jobs::IndexJobStatus::Queued,
        )
        .await;
    }

    if job.kind == "embedding" {
        let job_id = job.id.clone();
        let expected_cursor_json = job.cursor_json.clone();
        let embedding_model = models.config.embedding_model.clone();
        let embedding_execution_identity = models.config.embedding_execution_identity.clone();
        let vision_execution_identity = models.config.vision_execution_identity.clone();
        let db_path = inner.db_path.clone();
        let now = unix_timestamp()?;
        let current = run_db(db_path, move |conn| {
            if embedding_job_matches_current_execution(
                &conn,
                &db::index_jobs::get(&conn, &job_id)?.context("embedding 任务不存在")?,
                &embedding_model,
                &embedding_execution_identity,
                &vision_execution_identity,
            )? {
                return Ok(true);
            }
            db::index_jobs::cancel_queued_from_cursor(
                &conn,
                &job_id,
                &expected_cursor_json,
                "embedding 执行配置已经变更",
                now,
            )?;
            Ok(false)
        })
        .await?;
        if !current {
            return Ok(());
        }
    }

    let job_id = job.id.clone();
    let expected_cursor_json = job.cursor_json.clone();
    let db_path = inner.db_path.clone();
    let now = unix_timestamp()?;
    if !run_db(db_path, move |conn| {
        db::index_jobs::claim(&conn, &job_id, &expected_cursor_json, now)
    })
    .await?
    {
        return Ok(());
    }

    // `list_queued` is only a scheduling hint. Reconfiguration can replace a
    // queued cursor between that scan and the claim, so execute only the row
    // read back after ownership was acquired.
    let claimed_id = job.id.clone();
    let db_path = inner.db_path.clone();
    let Some(job) = run_db(db_path, move |conn| db::index_jobs::get(&conn, &claimed_id)).await?
    else {
        return Ok(());
    };
    if job.status != db::index_jobs::IndexJobStatus::Running {
        return Ok(());
    }

    drop(transition);
    let run_id = NEXT_TRANSLATION_RUN_ID.fetch_add(1, Ordering::Relaxed);
    INDEXING_LOG_RUN
        .scope((run_id, Instant::now()), async {
            record_index_event(
                inner,
                &job,
                JobLogEvent::RunStarted,
                JobLogMetrics::default(),
            );
            let outcome = match job.kind.as_str() {
                "embedding" => run_embedding(inner, &job, &models).await,
                "vision" => run_vision(inner, &job, &models).await,
                "translation" => {
                    run_translation(inner, &job, &models)
                        .instrument(tracing::info_span!(
                            target: "ngy_ai",
                            "translation_run",
                            translation_run_id = run_id,
                        ))
                        .await
                }
                other => Err(anyhow::anyhow!("不支持的索引任务类型：{other}")),
            };
            match outcome {
                Ok(outcome) => {
                    publish_outcome(
                        inner,
                        &job,
                        outcome,
                        db::index_jobs::IndexJobStatus::Running,
                    )
                    .await
                }
                Err(error) => {
                    record_index_event(
                        inner,
                        &job,
                        JobLogEvent::StepFailed,
                        JobLogMetrics {
                            error_kind: Some(classify_error(&error)),
                            ..JobLogMetrics::for_error(&error)
                        },
                    );
                    let current = current_cursor(inner, &job)
                        .await
                        .unwrap_or(job.cursor_json.clone());
                    let cursor = serde_json::from_str(&current).unwrap_or_else(|_| JobCursor {
                        schema_version: 1,
                        book_id: job.book_id.clone(),
                        source_id: job.source_id.clone().unwrap_or_default(),
                        revision: 0,
                        kind: job.kind.clone(),
                        model: None,
                        execution_identity: None,
                        input_execution_identity: None,
                        next_ordinal: 0,
                        retry_ordinals: Vec::new(),
                        resume_ordinal: None,
                        inflight: Vec::new(),
                        blocks_version: TRANSLATION_BLOCKS_VERSION,
                        priority: None,
                        force_retry: false,
                    });
                    publish_outcome(
                        inner,
                        &job,
                        RunOutcome::Failed(
                            cursor,
                            format!("{error:#}"),
                            Some(classify_error(&error)),
                        ),
                        db::index_jobs::IndexJobStatus::Running,
                    )
                    .await
                }
            }
        })
        .await
}
