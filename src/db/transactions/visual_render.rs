//! Reconciling visual render jobs, sources and their renderers.

use super::*;

pub(crate) fn reconcile_visual_render_jobs(
    conn: &mut Connection,
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
    now: u64,
) -> Result<VisualRenderReconciliation> {
    validate_visual_renderers(renderers, profile)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉 renderer 恢复事务")?;
    let current_sources = book_sources::list_current(&tx)?;
    let mut changed = 0;
    let mut replaced_object_keys = HashSet::new();

    for source in current_sources {
        if reconcile_visual_source(
            &tx,
            source,
            renderers,
            profile,
            now,
            false,
            &mut replaced_object_keys,
        )? {
            changed += 1;
        }
    }

    let unreferenced_blobs = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉 renderer 恢复事务")?;
    Ok(VisualRenderReconciliation {
        changed_sources: changed,
        unreferenced_blobs,
    })
}

/// Persists the explicit per-book Office opt-in and atomically replaces the
/// current source's visual/vision pipeline. The caller must first stop any
/// in-memory renderer for the returned source ID, then wake the coordinator
/// after this transaction commits.
#[cfg(target_os = "windows")]
pub(crate) fn set_office_enhancement(
    conn: &mut Connection,
    book_id: &str,
    enabled: bool,
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
    now: u64,
) -> Result<(String, VisualRenderReconciliation)> {
    validate_visual_renderers(renderers, profile)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动 Office 增强预览设置事务")?;
    let book = books::get(&tx, book_id)?.context("图书不存在")?;
    let source =
        book_sources::get_revision(&tx, book_id, book.revision)?.context("当前图书来源不存在")?;
    if enabled {
        ensure!(
            source.source_kind == "original"
                && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx"),
            "只有尚未规范化编辑的 Office 原件可以启用增强预览"
        );
        ensure!(
            renderers
                .iter()
                .any(|renderer| renderer.renderer == "ngy-office-com-enhanced"),
            "Office 增强 renderer 未注册"
        );
    }
    office_enhancements::upsert(&tx, book_id, enabled, now)?;
    let source_id = source.id.clone();
    let mut replaced_object_keys = HashSet::new();
    let changed = reconcile_visual_source(
        &tx,
        source,
        renderers,
        profile,
        now,
        true,
        &mut replaced_object_keys,
    )?;
    debug_assert!(changed, "forced Office renderer reconciliation did not run");
    let unreferenced_blobs = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交 Office 增强预览设置事务")?;
    Ok((
        source_id,
        VisualRenderReconciliation {
            changed_sources: usize::from(changed),
            unreferenced_blobs,
        },
    ))
}

pub(crate) fn validate_visual_renderers(
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
) -> Result<()> {
    ensure!(!renderers.is_empty(), "视觉任务恢复缺少已注册 renderer");
    profile.validate()?;
    let mut renderer_names = HashSet::new();
    for renderer in renderers {
        renderer.validate()?;
        ensure!(
            renderer_names.insert(renderer.renderer.as_str()),
            "视觉任务恢复包含重复 renderer {}",
            renderer.renderer
        );
    }
    Ok(())
}

pub(crate) fn reconcile_visual_source(
    conn: &Connection,
    source: BookSource,
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
    now: u64,
    force: bool,
    replaced_object_keys: &mut HashSet<String>,
) -> Result<bool> {
    let units = content_units::list_for_source(conn, &source.id)?;
    ensure!(!units.is_empty(), "当前图书来源没有内容单元");
    let descriptor = renderer_for_source(conn, &source, renderers)?;
    let job_id = format!("visual-render:{}", source.id);
    let expected = VisualJobSpec {
        id: job_id.clone(),
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        document_revision: Revision::new(source.revision),
        renderer: descriptor.renderer.clone(),
        renderer_version: descriptor.version.clone(),
        fidelity: descriptor.fidelity,
        unit_ids: units.iter().map(|unit| unit.id.clone()).collect(),
        profile: profile.clone(),
    };
    expected.validate()?;
    let existing = index_jobs::get(conn, &job_id)?;
    let is_current = match existing.as_ref() {
        Some(job) => visual_job_matches_current(conn, job, &expected)?,
        None => false,
    };
    if is_current && !force {
        return Ok(false);
    }

    replaced_object_keys.extend(
        visual_pages::list_for_source(conn, &source.id)?
            .into_iter()
            .map(|page| page.object_key),
    );
    replaced_object_keys.extend(
        visual_page_staging::list_for_job(conn, &job_id)?
            .into_iter()
            .map(|staged| staged.page.object_key),
    );
    visual_pages::delete_for_source(conn, &source.id)?;
    visual_page_staging::delete_for_job(conn, &job_id)?;
    search_chunks::delete_stale_visual_derived(conn, &source.id, 0)?;
    if existing.is_some() {
        index_jobs::delete(conn, &job_id)?;
    }
    index_jobs::insert(
        conn,
        &IndexJob {
            id: job_id,
            book_id: source.book_id.clone(),
            source_id: Some(source.id.clone()),
            kind: "visual_render".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: encode_persisted_visual_job(expected, 0)?,
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        },
    )?;

    let base_vision_id = format!("vision:{}", source.id);
    let base_embedding_id = format!("embedding:{}", source.id);
    for job in index_jobs::list_for_book(conn, &source.book_id)? {
        if job.source_id.as_deref() != Some(source.id.as_str()) {
            continue;
        }
        if job.kind == "vision" || (job.kind == "embedding" && job.id != base_embedding_id) {
            index_jobs::delete(conn, &job.id)?;
        }
    }
    let vision_cursor = serde_json::json!({
        "schema_version": 1,
        "book_id": source.book_id,
        "source_id": source.id,
        "revision": source.revision,
        "kind": "vision",
        "model": null,
        "next_ordinal": 0,
    });
    index_jobs::insert(
        conn,
        &IndexJob {
            id: base_vision_id,
            book_id: source.book_id,
            source_id: Some(source.id),
            kind: "vision".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: serde_json::to_string(&vision_cursor)
                .context("无法序列化视觉索引恢复游标")?,
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        },
    )?;
    Ok(true)
}

pub(crate) fn renderer_for_source<'a>(
    conn: &Connection,
    source: &BookSource,
    renderers: &'a [RendererDescriptor],
) -> Result<&'a RendererDescriptor> {
    #[cfg(target_os = "windows")]
    if source.source_kind == "original"
        && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx")
        && office_enhancements::is_enabled(conn, &source.book_id)?
        && let Some(renderer) = renderers
            .iter()
            .find(|renderer| renderer.renderer == "ngy-office-com-enhanced")
    {
        return Ok(renderer);
    }
    #[cfg(target_os = "windows")]
    if source.source_kind == "original"
        && source.format == "pdf"
        && let Some(renderer) = renderers
            .iter()
            .find(|renderer| renderer.renderer == "ngy-windows-pdf-png")
    {
        return Ok(renderer);
    }
    // DjVu pages are rasterized from the retained original by the portable
    // DjVu renderer; the canonical model never stores the page bitmaps.
    if source.source_kind == "original"
        && source.format == "djvu"
        && let Some(renderer) = renderers
            .iter()
            .find(|renderer| renderer.renderer == DJVU_RENDERER_NAME)
    {
        return Ok(renderer);
    }
    renderers
        .iter()
        .find(|renderer| renderer.renderer == "ngy-structural-png")
        .or_else(|| (renderers.len() == 1).then(|| &renderers[0]))
        .context("视觉任务恢复找不到当前来源所需的 renderer")
}

pub(crate) fn visual_job_matches_current(
    conn: &Connection,
    job: &IndexJob,
    expected: &VisualJobSpec,
) -> Result<bool> {
    if job.kind != "visual_render"
        || job.book_id != expected.book_id
        || job.source_id.as_deref() != Some(expected.source_id.as_str())
    {
        return Ok(false);
    }
    let Ok((stored, completed_pages)) = decode_persisted_visual_job(&job.cursor_json) else {
        return Ok(false);
    };
    if stored != *expected {
        return Ok(false);
    }
    if job.status != IndexJobStatus::Succeeded {
        let staged = visual_page_staging::list_for_job(conn, &job.id)?;
        return Ok(staged.len() == completed_pages
            && staged.iter().enumerate().all(|(index, staged)| {
                staged.page.page_index == index
                    && staged.page.book_id == expected.book_id
                    && staged.page.source_id == expected.source_id
                    && staged.page.document_revision == expected.document_revision.get()
                    && staged.page.renderer == expected.renderer
                    && staged.page.renderer_version == expected.renderer_version
                    && staged.page.profile_id == expected.profile.stable_id()
                    && staged.page.render_scale == expected.profile.scale()
                    && validate_visual_page_for_spec(expected, &staged.page).is_ok()
                    && validate_visual_page_unit(conn, expected, &staged.page).is_ok()
            }));
    }
    if visual_page_staging::count_for_job(conn, &job.id)? != 0 {
        return Ok(false);
    }
    let pages = visual_pages::list_for_source(conn, &expected.source_id)?;
    if pages.is_empty() || pages.len() != completed_pages {
        return Ok(false);
    }
    let all_preview_only = pages.iter().all(|page| page.content_unit_id.is_none());
    let all_unit_mapped = pages.iter().all(|page| page.content_unit_id.is_some());
    if !all_preview_only && !all_unit_mapped {
        return Ok(false);
    }
    let covered_units = pages
        .iter()
        .filter_map(|page| page.content_unit_id.as_deref())
        .collect::<HashSet<_>>();
    Ok((all_preview_only
        || expected
            .unit_ids
            .iter()
            .all(|unit_id| covered_units.contains(unit_id.as_str())))
        && pages.iter().enumerate().all(|(index, page)| {
            page.page_index == index
                && page.book_id == expected.book_id
                && page.source_id == expected.source_id
                && page.document_revision == expected.document_revision.get()
                && page.renderer == expected.renderer
                && page.renderer_version == expected.renderer_version
                && page.profile_id == expected.profile.stable_id()
                && page.render_scale == expected.profile.scale()
                && page
                    .content_unit_id
                    .as_deref()
                    .is_none_or(|unit_id| expected.unit_ids.iter().any(|id| id == unit_id))
                && validate_visual_page_for_spec(expected, page).is_ok()
                && validate_visual_page_unit(conn, expected, page).is_ok()
        }))
}
