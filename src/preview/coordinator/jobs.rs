//! Processing one queued visual job.

use super::*;

pub(crate) async fn process_visual_job(core: Arc<CoordinatorCore>, job_id: &str) -> Result<()> {
    let Some(record) = blocking_get(Arc::clone(&core.store), job_id).await? else {
        return Ok(());
    };
    if record.state != VisualJobState::Queued {
        return Ok(());
    }
    if record.cancel_requested {
        blocking_transition(
            Arc::clone(&core.store),
            job_id,
            VisualJobState::Cancelled,
            record.completed_pages,
            None,
        )
        .await?;
        record_visual_event(
            &core,
            job_id,
            JobLogEvent::Cancelled,
            JobLogMetrics::default(),
        );
        return Ok(());
    }
    if record.pause_requested {
        blocking_transition(
            Arc::clone(&core.store),
            job_id,
            VisualJobState::Paused,
            record.completed_pages,
            None,
        )
        .await?;
        record_visual_event(&core, job_id, JobLogEvent::Paused, JobLogMetrics::default());
        return Ok(());
    }

    blocking_transition(
        Arc::clone(&core.store),
        job_id,
        VisualJobState::Running,
        record.completed_pages,
        None,
    )
    .await?;
    let run_id = NEXT_VISUAL_LOG_RUN_ID.fetch_add(1, Ordering::Relaxed);
    VISUAL_LOG_RUN
        .scope(
            (run_id, Instant::now(), record.attempts.saturating_add(1)),
            async {
                record_visual_event(
                    &core,
                    job_id,
                    JobLogEvent::RunStarted,
                    JobLogMetrics {
                        ordinal: Some(record.completed_pages as u64),
                        ..JobLogMetrics::default()
                    },
                );
                let control = RenderControl::default();
                core.controls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("视觉任务控制锁已损坏"))?
                    .insert(job_id.to_string(), control.clone());

                // A pause/cancel request can land after the queued-state read but before
                // the in-memory control is published. Re-read persisted flags once the
                // control is visible so that narrow race cannot lose an accepted request.
                if let Some(latest) = blocking_get(Arc::clone(&core.store), job_id).await? {
                    if latest.cancel_requested {
                        control.request_cancel();
                    } else if latest.pause_requested {
                        control.request_pause();
                    }
                }

                let result =
                    execute_visual_job(&core, &record.spec, record.completed_pages, control).await;
                core.controls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("视觉任务控制锁已损坏"))?
                    .remove(job_id);

                match result {
                    // `commit_pages` is the single success commit point: production sinks
                    // publish final pages, clear staging, and mark the job succeeded in one
                    // SQLite transaction. A second state write here could turn a committed
                    // render into a reported failure.
                    Ok(page_count) => {
                        record_visual_event(
                            &core,
                            job_id,
                            JobLogEvent::RunSucceeded,
                            JobLogMetrics {
                                actual_count: Some(page_count as u64),
                                duration_ms: VISUAL_LOG_RUN
                                    .try_with(|(_, started, _)| {
                                        started.elapsed().as_millis() as u64
                                    })
                                    .ok(),
                                ..JobLogMetrics::default()
                            },
                        );
                    }
                    Err(error) => {
                        let state = match error.downcast_ref::<RenderInterrupted>() {
                            Some(RenderInterrupted::Paused) => VisualJobState::Paused,
                            Some(RenderInterrupted::Cancelled) => VisualJobState::Cancelled,
                            None => VisualJobState::Failed,
                        };
                        let detail = truncate_error(&format!("{error:#}"));
                        let completed_pages = blocking_get(Arc::clone(&core.store), job_id)
                            .await?
                            .map(|latest| latest.completed_pages)
                            .unwrap_or(record.completed_pages);
                        blocking_transition(
                            Arc::clone(&core.store),
                            job_id,
                            state,
                            completed_pages,
                            (state == VisualJobState::Failed).then_some(detail.as_str()),
                        )
                        .await?;
                        record_visual_event(
                            &core,
                            job_id,
                            match state {
                                VisualJobState::Paused => JobLogEvent::Paused,
                                VisualJobState::Cancelled => JobLogEvent::Cancelled,
                                _ => JobLogEvent::RunFailed,
                            },
                            JobLogMetrics {
                                ordinal: Some(completed_pages as u64),
                                duration_ms: VISUAL_LOG_RUN
                                    .try_with(|(_, started, _)| {
                                        started.elapsed().as_millis() as u64
                                    })
                                    .ok(),
                                error_kind: (state == VisualJobState::Failed)
                                    .then(|| classify_error(&error)),
                                ..JobLogMetrics::for_error(&error)
                            },
                        );
                    }
                }
                Ok(())
            },
        )
        .await
}
