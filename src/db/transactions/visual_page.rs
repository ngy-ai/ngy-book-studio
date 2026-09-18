//! Visual pages: replacement, checkpointing and staged publication.

use super::*;

/// Atomically publishes a fully rendered visual-page set after the caller has
/// durably written every immutable object. The returned rows are objects from
/// the replaced page set that have no remaining database reference after the
/// commit; the caller may remove their bytes and then their metadata. Objects
/// left by a failed transaction are intentionally not deleted here; startup GC
/// owns that cleanup.
pub(crate) fn replace_visual_pages(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    revision: u64,
    page_blobs: &[BlobRecord],
    pages: &[VisualPage],
) -> Result<Vec<BlobRecord>> {
    ensure!(!book_id.trim().is_empty(), "视觉页面 book ID 不能为空");
    ensure!(!source_id.trim().is_empty(), "视觉页面 source ID 不能为空");
    let blob_keys = page_blobs
        .iter()
        .map(|blob| blob.object_key.as_str())
        .collect::<HashSet<_>>();
    ensure!(
        blob_keys.len() == page_blobs.len(),
        "视觉页面包含重复的对象元数据"
    );
    ensure!(
        pages.iter().all(|page| {
            page.book_id == book_id
                && page.source_id == source_id
                && page.document_revision == revision
                && !page.renderer.trim().is_empty()
                && !page.renderer_version.trim().is_empty()
                && !page.profile_id.trim().is_empty()
                && matches!(
                    page.fidelity.as_str(),
                    "normalized" | "structural" | "office_enhanced"
                )
                && blob_keys.contains(page.object_key.as_str())
        }),
        "视觉页面与图书、来源、渲染元数据或对象元数据不匹配"
    );
    ensure!(
        pages
            .iter()
            .enumerate()
            .all(|(index, page)| page.page_index == index),
        "视觉页面序号必须连续且从 0 开始"
    );

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面替换事务")?;
    let book = books::get(&tx, book_id)?.context("视觉页面所属图书不存在")?;
    ensure!(
        book.revision == revision,
        "视觉页面对应的图书 revision 已过期"
    );
    let source = super::super::book_sources::get(&tx, source_id)?.context("视觉页面来源不存在")?;
    ensure!(
        source.book_id == book_id && source.revision == revision,
        "视觉页面来源与图书 revision 不匹配"
    );
    for page in pages {
        validate_visual_page_unit_for_source(&tx, book_id, source_id, page)?;
    }
    for blob in page_blobs {
        blobs::ensure_present(&tx, blob)?;
    }
    let replaced_object_keys = visual_pages::list_for_source(&tx, source_id)?
        .into_iter()
        .map(|page| page.object_key)
        .collect::<HashSet<_>>();
    visual_pages::delete_for_source(&tx, source_id)?;
    for page in pages {
        visual_pages::upsert(&tx, page)?;
    }
    let unreferenced = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉页面替换事务")?;
    Ok(unreferenced)
}

/// Durably checkpoints one newly rendered page. The immutable object must be
/// written before this function is called. Its metadata, the staging
/// reference, and the visual job cursor advance in one transaction, so a
/// recovered cursor can never point past its durable page prefix.
pub(crate) fn checkpoint_visual_page(
    conn: &mut Connection,
    spec: &VisualJobSpec,
    blob: &BlobRecord,
    page: &VisualPage,
    completed_pages: usize,
    now: u64,
) -> Result<()> {
    spec.validate()?;
    ensure!(
        page.page_index.checked_add(1) == Some(completed_pages),
        "视觉页面断点与页面序号不一致"
    );
    validate_visual_page_for_spec(spec, page)?;
    ensure!(blob.object_key == page.object_key, "视觉页面断点对象不匹配");

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面断点事务")?;
    let book = books::get(&tx, &spec.book_id)?.context("视觉页面断点所属图书不存在")?;
    let source = book_sources::get(&tx, &spec.source_id)?.context("视觉页面断点来源不存在")?;
    ensure!(
        book.revision == spec.document_revision.get()
            && source.book_id == book.id
            && source.revision == book.revision,
        "视觉页面断点不能写入过期来源"
    );
    validate_visual_page_unit(&tx, spec, page)?;

    let job = index_jobs::get(&tx, &spec.id)?.context("视觉页面断点任务不存在")?;
    ensure!(
        job.kind == "visual_render"
            && job.status == IndexJobStatus::Running
            && job.book_id == spec.book_id
            && job.source_id.as_deref() == Some(spec.source_id.as_str()),
        "视觉页面断点任务状态或归属不匹配"
    );
    let (stored_spec, stored_completed) = decode_persisted_visual_job(&job.cursor_json)?;
    ensure!(stored_spec == *spec, "视觉页面断点任务规格已经变化");
    ensure!(
        stored_completed == page.page_index,
        "视觉页面断点不是当前持久前缀的下一页"
    );
    let prefix = visual_page_staging::prefix_stats(&tx, &spec.id)?;
    ensure!(
        prefix.is_contiguous_prefix(stored_completed),
        "视觉页面持久断点不连续"
    );
    ensure!(
        prefix
            .total_bytes
            .checked_add(blob.byte_len)
            .is_some_and(|total| total <= MAX_STAGED_VISUAL_BYTES),
        "视觉页面断点超过批次总大小上限"
    );

    blobs::ensure_present(&tx, blob)?;
    ensure!(
        visual_page_staging::insert(&tx, &spec.id, page)? == 1,
        "视觉页面断点没有写入"
    );
    let cursor_json = encode_persisted_visual_job(spec.clone(), completed_pages)?;
    ensure!(
        index_jobs::update_state_from(
            &tx,
            &spec.id,
            IndexJobStatus::Running,
            IndexJobStatus::Running,
            &cursor_json,
            None,
            now,
            None,
            None,
        )? == 1,
        "视觉页面断点游标没有更新"
    );
    tx.commit().context("无法提交视觉页面断点事务")?;
    Ok(())
}

/// Atomically promotes a complete staged prefix to the visible visual page
/// set and marks its job succeeded. Until this transaction commits, an older
/// final page set remains fully visible.
pub(crate) fn publish_staged_visual_pages(
    conn: &mut Connection,
    spec: &VisualJobSpec,
    total_pages: usize,
    now: u64,
) -> Result<Vec<BlobRecord>> {
    spec.validate()?;
    ensure!(total_pages > 0, "视觉任务没有可发布页面");
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面发布事务")?;
    let book = books::get(&tx, &spec.book_id)?.context("视觉页面所属图书不存在")?;
    let source = book_sources::get(&tx, &spec.source_id)?.context("视觉页面来源不存在")?;
    ensure!(
        book.revision == spec.document_revision.get()
            && source.book_id == book.id
            && source.revision == book.revision,
        "视觉页面不能发布到过期来源"
    );
    let job = index_jobs::get(&tx, &spec.id)?.context("视觉页面任务不存在")?;
    ensure!(
        job.kind == "visual_render"
            && job.status == IndexJobStatus::Running
            && job.book_id == spec.book_id
            && job.source_id.as_deref() == Some(spec.source_id.as_str())
            && !job.pause_requested
            && !job.cancel_requested,
        "视觉页面任务状态已变化，拒绝发布"
    );
    let (stored_spec, completed_pages) = decode_persisted_visual_job(&job.cursor_json)?;
    ensure!(stored_spec == *spec, "视觉页面任务规格已经变化");
    ensure!(completed_pages == total_pages, "视觉页面断点尚未完成");

    let staged = visual_page_staging::list_for_job(&tx, &spec.id)?;
    ensure!(staged.len() == total_pages, "视觉页面断点数量不完整");
    for (index, staged) in staged.iter().enumerate() {
        ensure!(
            staged.job_id == spec.id && staged.page.page_index == index,
            "视觉页面断点序号不连续"
        );
        validate_visual_page_for_spec(spec, &staged.page)?;
        validate_visual_page_unit(&tx, spec, &staged.page)?;
        ensure!(
            blobs::get(&tx, &staged.page.object_key)?.is_some(),
            "视觉页面断点对象元数据不存在"
        );
    }

    let replaced_object_keys = visual_pages::list_for_source(&tx, &spec.source_id)?
        .into_iter()
        .map(|page| page.object_key)
        .collect::<HashSet<_>>();
    visual_pages::delete_for_source(&tx, &spec.source_id)?;
    for staged in &staged {
        visual_pages::upsert(&tx, &staged.page)?;
    }
    visual_page_staging::delete_for_job(&tx, &spec.id)?;
    let cursor_json = encode_persisted_visual_job(spec.clone(), total_pages)?;
    ensure!(
        index_jobs::update_state_from(
            &tx,
            &spec.id,
            IndexJobStatus::Running,
            IndexJobStatus::Succeeded,
            &cursor_json,
            None,
            now,
            None,
            Some(now),
        )? == 1,
        "视觉页面任务完成状态没有更新"
    );
    let unreferenced = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉页面发布事务")?;
    Ok(unreferenced)
}

/// Drops a failed/cancelled job's resumable prefix before an explicit retry.
/// Returned objects lost their last reference in this transaction and may be
/// reclaimed after the caller releases the publication lock.
pub(crate) fn clear_visual_page_staging(
    conn: &mut Connection,
    spec: &VisualJobSpec,
) -> Result<Vec<BlobRecord>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面断点清理事务")?;
    let job = index_jobs::get(&tx, &spec.id)?.context("视觉页面断点任务不存在")?;
    let (stored_spec, _) = decode_persisted_visual_job(&job.cursor_json)?;
    ensure!(
        job.kind == "visual_render"
            && job.book_id == spec.book_id
            && job.source_id.as_deref() == Some(spec.source_id.as_str())
            && stored_spec == *spec,
        "视觉页面断点清理目标不匹配"
    );
    let staged_keys = visual_page_staging::list_for_job(&tx, &spec.id)?
        .into_iter()
        .map(|staged| staged.page.object_key)
        .collect::<HashSet<_>>();
    visual_page_staging::delete_for_job(&tx, &spec.id)?;
    let unreferenced = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| staged_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉页面断点清理事务")?;
    Ok(unreferenced)
}

pub(crate) fn validate_visual_page_for_spec(spec: &VisualJobSpec, page: &VisualPage) -> Result<()> {
    ensure!(
        page.book_id == spec.book_id
            && page.source_id == spec.source_id
            && page.document_revision == spec.document_revision.get()
            && page.render_scale == spec.profile.scale()
            && page.renderer == spec.renderer
            && page.renderer_version == spec.renderer_version
            && page.profile_id == spec.profile.stable_id()
            && page.fidelity
                == match spec.fidelity {
                    crate::preview::RenderFidelity::Normalized => "normalized",
                    crate::preview::RenderFidelity::Structural => "structural",
                    crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
                }
            && page.width > 0
            && page.height > 0
            && !page.object_key.trim().is_empty()
            && !page.locator_json.trim().is_empty(),
        "视觉页面与任务规格不匹配"
    );
    Ok(())
}

pub(crate) fn validate_visual_page_unit(
    conn: &Connection,
    spec: &VisualJobSpec,
    page: &VisualPage,
) -> Result<()> {
    validate_visual_page_unit_for_source(conn, &spec.book_id, &spec.source_id, page)?;
    Ok(())
}

pub(crate) fn validate_visual_page_unit_for_source(
    conn: &Connection,
    book_id: &str,
    source_id: &str,
    page: &VisualPage,
) -> Result<()> {
    let source = book_sources::get(conn, source_id)?.context("视觉页面来源不存在")?;
    ensure!(source.book_id == book_id, "视觉页面来源不属于当前图书");
    let locator = serde_json::from_str::<DocumentLocator>(&page.locator_json)
        .context("视觉页面 locator JSON 无效")?;
    locator.validate().context("视觉页面 locator 无效")?;
    ensure!(
        locator.book_id == book_id,
        "视觉页面 locator 不属于当前图书"
    );
    match page.content_unit_id.as_deref() {
        Some(unit_id) => {
            ensure!(
                locator.unit_id == unit_id
                    && !matches!(
                        locator.source.as_ref(),
                        Some(SourceLocator::OfficeRenderedPage { .. })
                    ),
                "视觉页面 locator 与内容单元不匹配"
            );
            let unit = content_units::get(conn, unit_id)?.context("视觉页面内容单元不存在")?;
            ensure!(
                unit.book_id == book_id
                    && unit.source_id == source_id
                    && unit.revision == page.unit_revision,
                "视觉页面内容单元 revision 不匹配"
            );
        }
        None => {
            let expected_page = u32::try_from(page.page_index)
                .ok()
                .and_then(|index| index.checked_add(1));
            ensure!(
                page.renderer == "ngy-office-com-enhanced"
                    && page.fidelity == "office_enhanced"
                    && matches!(source.format.as_str(), "doc" | "docx" | "xlsx")
                    && page.unit_revision == Revision::INITIAL.get()
                    && locator.unit_id == office_preview_unit_id(book_id, source_id)
                    && locator.block_id.is_none()
                    && locator.text_range.is_none()
                    && locator.region.is_none()
                    && matches!(
                        (locator.source.as_ref(), expected_page),
                        (
                            Some(SourceLocator::OfficeRenderedPage { page }),
                            Some(expected_page)
                        ) if *page == expected_page
                    ),
                "无内容单元的视觉页面不是有效的 Office preview-only 页面"
            );
        }
    }
    Ok(())
}
