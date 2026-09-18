//! Blocking helpers that hop onto the database runtime.

use super::*;

pub(crate) async fn blocking_get(
    store: Arc<dyn VisualJobStore>,
    job_id: &str,
) -> Result<Option<VisualJobRecord>> {
    let job_id = job_id.to_string();
    tokio::task::spawn_blocking(move || store.get(&job_id))
        .await
        .context("视觉任务查询线程异常退出")?
}

pub(crate) async fn blocking_transition(
    store: Arc<dyn VisualJobStore>,
    job_id: &str,
    state: VisualJobState,
    completed_pages: usize,
    error: Option<&str>,
) -> Result<()> {
    let job_id = job_id.to_string();
    let error = error.map(str::to_string);
    let now = unix_timestamp()?;
    let changed = tokio::task::spawn_blocking(move || {
        store.transition(&job_id, state, completed_pages, error.as_deref(), now)
    })
    .await
    .context("视觉任务状态更新线程异常退出")??;
    ensure!(changed, "视觉任务状态更新失败：任务不存在");
    Ok(())
}
