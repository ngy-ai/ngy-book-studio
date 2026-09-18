//! The Office/DjVu preview gate: waiting for enhanced pages, preparing
//! DjVu pages and waiting for a render job.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct OfficePreviewRun {
    pub(crate) request_id: u64,
    pub(crate) book_id: String,
    pub(crate) title: String,
    pub(crate) kind: VisualPageSourceKind,
    /// Page to open first; `1` for a fresh Office preview.
    pub(crate) initial_page: u32,
    pub(crate) cancellation: OfficeCancellation,
    pub(crate) enabling: bool,
}

pub(crate) struct OfficePreviewLaunch {
    pub(crate) book_id: String,
    pub(crate) title: String,
    pub(crate) kind: VisualPageSourceKind,
    pub(crate) initial_page: u32,
    pub(crate) pages: Vec<PublishedVisualPage>,
}

pub(crate) fn is_office_format_name(format: &str) -> bool {
    matches!(format, "doc" | "docx" | "pptx" | "xlsx")
}

pub(crate) fn is_current_office_preview(
    active: Option<&OfficePreviewRun>,
    request_id: u64,
    book_id: &str,
) -> bool {
    active.is_some_and(|run| run.request_id == request_id && run.book_id == book_id)
}

pub(crate) async fn wait_for_office_enhanced_pages(
    services: Arc<AppServices>,
    book_id: String,
    cancellation: OfficeCancellation,
) -> Result<Vec<PublishedVisualPage>> {
    let job_id = services
        .set_office_enhancement_enabled(book_id.clone(), true)
        .await?;
    let coordinator = services.visual_jobs().context("视觉任务协调器不可用")?;
    let started = std::time::Instant::now();
    loop {
        ensure!(!cancellation.is_cancelled(), "Office 增强预览已取消");
        let record = coordinator
            .status(&job_id)
            .await?
            .with_context(|| format!("Office 增强视觉任务不存在：{job_id}"))?;
        match record.state {
            VisualJobState::Succeeded => break,
            VisualJobState::Failed => {
                anyhow::bail!(
                    "Office 增强视觉任务失败：{}",
                    record.error.as_deref().unwrap_or("未知错误")
                )
            }
            VisualJobState::Cancelled => anyhow::bail!("Office 增强预览已取消"),
            VisualJobState::Queued | VisualJobState::Running | VisualJobState::Paused => {}
        }
        ensure!(
            started.elapsed() < OFFICE_PREVIEW_WAIT_TIMEOUT,
            "等待 Office 增强视觉任务完成超时"
        );
        tokio::time::sleep(OFFICE_PREVIEW_POLL_INTERVAL).await;
    }
    ensure!(!cancellation.is_cancelled(), "Office 增强预览已取消");
    services
        .load_published_visual_pages(book_id, VisualPageSourceKind::OfficeEnhanced)
        .await
}

/// Prepares the DjVu page images for reading: schedules the local render job,
/// waits for it to publish, then loads the verified page set.
///
/// DjVu rasterization is pure local Rust, so no per-book opt-in is required
/// and a long book is bounded by the user's cancellation rather than by the
/// Office preview timeout.
pub(crate) async fn prepare_djvu_pages(
    services: Arc<AppServices>,
    book_id: String,
    cancellation: OfficeCancellation,
) -> Result<Vec<PublishedVisualPage>> {
    let job_id = services.ensure_visual_render_job(book_id.clone()).await?;
    wait_for_page_render_job(&services, &job_id, &cancellation, "DjVu 页面准备").await?;
    ensure!(!cancellation.is_cancelled(), "DjVu 页面准备已取消");
    services
        .load_published_visual_pages(book_id, VisualPageSourceKind::Djvu)
        .await
}

/// Waits until one visual render job publishes its page set.
pub(crate) async fn wait_for_page_render_job(
    services: &AppServices,
    job_id: &str,
    cancellation: &OfficeCancellation,
    label: &str,
) -> Result<()> {
    let coordinator = services.visual_jobs().context("视觉任务协调器不可用")?;
    loop {
        ensure!(!cancellation.is_cancelled(), "{label}已取消");
        let record = coordinator
            .status(job_id)
            .await?
            .with_context(|| format!("{label}的视觉任务不存在：{job_id}"))?;
        match record.state {
            VisualJobState::Succeeded => return Ok(()),
            VisualJobState::Failed => anyhow::bail!(
                "{label}失败：{}",
                record.error.as_deref().unwrap_or("未知错误")
            ),
            VisualJobState::Cancelled => anyhow::bail!("{label}已取消"),
            VisualJobState::Paused => {
                // A pause requested from the background-task window must not
                // hang the reader: the job is local and deterministic, so
                // resume it from its durable cursor instead of restarting it.
                coordinator.resume(job_id).await?;
            }
            VisualJobState::Queued | VisualJobState::Running => {}
        }
        tokio::time::sleep(OFFICE_PREVIEW_POLL_INTERVAL).await;
    }
}
