//! Stopping the jobs that must not survive a replacement.

use super::*;

#[cfg(target_os = "windows")]
pub(crate) async fn stop_visual_job_before_replacement(
    coordinator: &VisualJobCoordinator,
    job_id: &str,
) -> Result<()> {
    let Some(record) = coordinator.status(job_id).await? else {
        return Ok(());
    };
    if matches!(
        record.state,
        VisualJobState::Queued | VisualJobState::Running | VisualJobState::Paused
    ) {
        let _ = coordinator.cancel(job_id).await?;
    } else {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(125);
    loop {
        let Some(record) = coordinator.status(job_id).await? else {
            return Ok(());
        };
        if matches!(
            record.state,
            VisualJobState::Succeeded | VisualJobState::Failed | VisualJobState::Cancelled
        ) {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待现有视觉任务停止超时"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(target_os = "windows")]
pub(crate) async fn stop_indexing_jobs_before_visual_replacement(
    indexing: &IndexingCoordinator,
    book_id: &str,
    source_id: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let active = indexing
            .jobs_for_book(book_id)
            .await?
            .into_iter()
            .filter(|job| {
                job.source_id.as_deref() == Some(source_id)
                    && matches!(job.kind.as_str(), "embedding" | "vision")
                    && matches!(
                        job.status,
                        IndexingJobStatus::Queued
                            | IndexingJobStatus::Running
                            | IndexingJobStatus::Paused
                    )
            })
            .collect::<Vec<_>>();
        if active.is_empty() {
            return Ok(());
        }
        for job in active {
            if job.status == IndexingJobStatus::Paused {
                // Paused jobs are not scanned by the worker. Put them back in
                // the queue before setting cancellation so they can publish a
                // durable terminal state.
                let _ = indexing.resume(&job.id).await?;
            }
            let _ = indexing.cancel(&job.id).await?;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待旧视觉理解或向量任务停止超时"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
