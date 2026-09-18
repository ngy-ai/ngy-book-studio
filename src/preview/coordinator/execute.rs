//! Executing a claimed visual job.

use super::*;

pub(crate) async fn execute_visual_job(
    core: &CoordinatorCore,
    spec: &VisualJobSpec,
    completed_pages: usize,
    control: RenderControl,
) -> Result<usize> {
    let renderer = core
        .renderers
        .get(&spec.renderer)
        .with_context(|| format!("找不到视觉渲染器 {}", spec.renderer))?;
    let descriptor = renderer.descriptor();
    ensure!(
        descriptor.version == spec.renderer_version && descriptor.fidelity == spec.fidelity,
        "renderer 已变化；请重建视觉任务"
    );
    control.checkpoint().map_err(anyhow::Error::new)?;
    let document = core
        .source
        .load_document(spec.book_id.clone(), spec.document_revision)
        .await?;
    ensure!(document.id == spec.book_id, "视觉任务加载了错误的图书 ID");
    ensure!(
        document.revision == spec.document_revision,
        "视觉任务 revision 已过期；请重新投递"
    );
    record_visual_event(
        core,
        &spec.id,
        JobLogEvent::SourceLoaded,
        JobLogMetrics {
            ordinal: Some(completed_pages as u64),
            expected_count: Some(spec.unit_ids.len() as u64),
            ..JobLogMetrics::default()
        },
    );
    let (emitter, receiver) = RenderPageEmitter::bounded(completed_pages, 1);
    let render = renderer.render_units_resumable(
        RenderUnitsRequest {
            document: Arc::new(document),
            source_id: spec.source_id.clone(),
            unit_ids: spec.unit_ids.clone(),
            profile: spec.profile.clone(),
            asset_source: Some(Arc::clone(&core.source)),
        },
        control.clone(),
        emitter,
    );
    tokio::pin!(render);
    let mut render_result = None;
    let mut next_page = completed_pages;
    let mut page_started = Instant::now();
    let mut control_poll = tokio::time::interval(Duration::from_millis(10));
    control_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let received = if render_result.is_some() {
            tokio::select! {
                _ = control_poll.tick() => {
                    control.checkpoint().map_err(anyhow::Error::new)?;
                    continue;
                }
                received = receiver.recv() => received,
            }
        } else {
            tokio::select! {
                _ = control_poll.tick() => {
                    control.checkpoint().map_err(anyhow::Error::new)?;
                    continue;
                }
                result = &mut render => {
                    render_result = Some(result);
                    continue;
                }
                received = receiver.recv() => received,
            }
        };
        let Ok(page) = received else {
            break;
        };
        record_visual_event(
            core,
            &spec.id,
            JobLogEvent::ItemStarted,
            JobLogMetrics {
                ordinal: Some(next_page as u64),
                ..JobLogMetrics::default()
            },
        );
        validate_rendered_page(spec, &page, next_page)?;
        let checkpoint = next_page.checked_add(1).context("视觉页面断点数量溢出")?;
        let response_bytes = page.bytes.len() as u64;
        core.sink
            .checkpoint_page(spec.clone(), page, checkpoint)
            .await?;
        record_visual_event(
            core,
            &spec.id,
            JobLogEvent::ItemSaved,
            JobLogMetrics {
                ordinal: Some(next_page as u64),
                response_bytes: Some(response_bytes),
                duration_ms: Some(page_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );
        next_page = checkpoint;
        page_started = Instant::now();
        control.checkpoint().map_err(anyhow::Error::new)?;
    }
    let page_count = match render_result {
        Some(result) => result?,
        None => render.await?,
    };
    ensure!(page_count == next_page, "renderer 页面总数与持久断点不一致");
    control.checkpoint().map_err(anyhow::Error::new)?;
    record_visual_event(
        core,
        &spec.id,
        JobLogEvent::PublicationStarted,
        JobLogMetrics {
            actual_count: Some(page_count as u64),
            ..JobLogMetrics::default()
        },
    );
    core.sink.commit_pages(spec.clone(), page_count).await?;
    Ok(page_count)
}
