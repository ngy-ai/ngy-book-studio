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
            if inner.reconcile_translations.swap(false, Ordering::AcqRel)
                && let Err(error) = reconcile_translation_jobs(&inner).await
            {
                tracing::warn!(%error, "翻译任务对账失败");
            }
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

/// Rebuilds the durable whole-book translation jobs from the active target
/// language and model. Runs only when the queue is empty so a burst of library
/// mutations cannot starve already queued model work.
pub(crate) async fn reconcile_translation_jobs(inner: &Arc<IndexingInner>) -> Result<()> {
    let translation = inner
        .models
        .read()
        .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?
        .translation
        .clone();
    let Some(translation) = translation else {
        return Ok(());
    };
    let db_path = inner.db_path.clone();
    let changed = run_db_mut(db_path, move |mut conn| {
        let now = unix_timestamp()?;
        db::transactions::reconfigure_translation_jobs(
            &mut conn,
            translation.target_language.as_deref(),
            &translation.model,
            &translation.execution_identity,
            translation.auto_run,
            now,
        )
    })
    .await?;
    if changed != 0 {
        inner.wake.notify_one();
    }
    Ok(())
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
                        inflight: Vec::new(),
                        blocks_version: TRANSLATION_BLOCKS_VERSION,
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
