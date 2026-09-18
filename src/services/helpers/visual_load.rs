//! Loading the published visual pages of a library.

use super::*;

pub(crate) fn load_published_visual_page_rows(
    db_path: &Path,
    book_id: &str,
    kind: VisualPageSourceKind,
) -> Result<Vec<PublishedVisualPageRow>> {
    let expectation = published_page_expectation(kind)?;
    let label = visual_page_kind_label(kind);
    let conn = db::open_conn(db_path)?;
    let book = db::books::get(&conn, book_id)?.context("图书不存在")?;
    let source = db::book_sources::get_revision(&conn, book_id, book.revision)?
        .context("当前图书来源不存在")?;
    if kind == VisualPageSourceKind::OfficeEnhanced {
        ensure!(
            db::office_enhancements::is_enabled(&conn, book_id)?,
            "当前图书尚未启用 Office 增强预览"
        );
    }
    ensure!(
        source.source_kind == "original" && expectation.formats.contains(&source.format.as_str()),
        "当前图书来源不再是可渲染的 {label} 原件"
    );

    let job_id = format!("visual-render:{}", source.id);
    let job =
        db::index_jobs::get(&conn, &job_id)?.with_context(|| format!("{label} 视觉任务不存在"))?;
    ensure!(
        job.book_id == book.id
            && job.source_id.as_deref() == Some(source.id.as_str())
            && job.kind == "visual_render",
        "{label} 视觉任务不属于当前图书来源"
    );
    ensure!(
        job.status == db::index_jobs::IndexJobStatus::Succeeded,
        "{label} 视觉任务尚未成功完成"
    );
    let (spec, completed_pages) = crate::preview::decode_persisted_visual_job(&job.cursor_json)
        .with_context(|| format!("{label} 视觉任务游标无效"))?;
    ensure!(
        spec.id == job_id
            && spec.book_id == book.id
            && spec.source_id == source.id
            && spec.document_revision.get() == book.revision
            && spec.renderer == expectation.renderer
            && spec.fidelity == expectation.fidelity,
        "{label} 视觉任务不是当前图书版本的产物"
    );
    let units = db::content_units::list_for_source(&conn, &source.id)?
        .into_iter()
        .map(|unit| (unit.id, unit.revision))
        .collect::<BTreeMap<_, _>>();
    ensure!(!units.is_empty(), "当前 {label} 来源没有内容单元");
    ensure!(
        spec.unit_ids.is_empty()
            || (spec.unit_ids.len() == units.len()
                && spec
                    .unit_ids
                    .iter()
                    .all(|unit_id| units.contains_key(unit_id))),
        "{label} 视觉任务只包含部分内容单元"
    );
    let pages = db::visual_pages::list_for_source(&conn, &source.id)?;
    ensure!(
        !pages.is_empty() && pages.len() <= MAX_LOADED_VISUAL_PAGES,
        "{label} 页面数量无效或超过支持上限"
    );
    ensure!(
        completed_pages == pages.len(),
        "{label} 视觉任务完成进度与已发布页面不一致"
    );

    let profile_id = spec.profile.stable_id();
    let mut total_bytes = 0_u64;
    let mut rows = Vec::with_capacity(pages.len());
    for (expected_index, page) in pages.into_iter().enumerate() {
        ensure!(page.page_index == expected_index, "{label} 页面序号不连续");
        ensure!(
            page.book_id == book.id
                && page.source_id == source.id
                && page.document_revision == book.revision
                && page.renderer == spec.renderer
                && page.renderer_version == spec.renderer_version
                && page.profile_id == profile_id
                && page.render_scale == spec.profile.scale()
                && page.fidelity == render_fidelity_key(expectation.fidelity),
            "{label} 页面元数据与当前视觉任务不一致"
        );
        ensure!(
            page.width > 0 && page.height > 0 && page.render_scale.is_finite(),
            "{label} 页面尺寸或缩放无效"
        );
        let locator = serde_json::from_str::<crate::document::DocumentLocator>(&page.locator_json)
            .context("视觉页面定位信息无效")?;
        locator
            .validate()
            .context("视觉页面定位信息不符合统一模型约束")?;
        ensure!(locator.book_id == book.id, "视觉页面定位不属于当前图书");
        ensure!(
            locator.block_id.is_none() && locator.text_range.is_none() && locator.region.is_none(),
            "视觉页面定位不能携带块、选区或区域坐标"
        );
        match locator.source.as_ref() {
            Some(crate::document::SourceLocator::DjvuPage { page: source_page }) => {
                let expected_page = u32::try_from(expected_index)
                    .ok()
                    .and_then(|index| index.checked_add(1));
                ensure!(
                    !expectation.allow_preview_only
                        && page.content_unit_id.as_deref() == Some(locator.unit_id.as_str())
                        && expected_page == Some(*source_page),
                    "DjVu 页面与内容单元或源页码不一致"
                );
                let current_unit_revision = units.get(&locator.unit_id).with_context(|| {
                    format!("DjVu 页面引用了不存在的内容单元：{}", locator.unit_id)
                })?;
                ensure!(
                    page.unit_revision == *current_unit_revision,
                    "DjVu 页面内容单元版本已过期"
                );
            }
            Some(crate::document::SourceLocator::Slide { .. })
                if expectation.allow_preview_only =>
            {
                let content_unit_id = page
                    .content_unit_id
                    .as_deref()
                    .context("PowerPoint 增强页面缺少内容单元 ID")?;
                let current_unit_revision = units.get(content_unit_id).with_context(|| {
                    format!("Office 增强页面引用了不存在的内容单元：{content_unit_id}")
                })?;
                ensure!(
                    locator.unit_id == content_unit_id
                        && page.unit_revision == *current_unit_revision,
                    "PowerPoint 增强页面内容单元映射或版本已过期"
                );
            }
            Some(crate::document::SourceLocator::OfficeRenderedPage { page: source_page })
                if expectation.allow_preview_only =>
            {
                let expected_page = u32::try_from(expected_index)
                    .ok()
                    .and_then(|index| index.checked_add(1));
                ensure!(
                    page.content_unit_id.is_none()
                        && matches!(source.format.as_str(), "doc" | "docx" | "xlsx")
                        && page.unit_revision == crate::document::Revision::INITIAL.get()
                        && locator.unit_id == office_preview_unit_id(&book.id, &source.id)
                        && expected_page == Some(*source_page),
                    "Word/Excel 增强页面错误关联了内容单元或页面定位无效"
                );
            }
            _ => anyhow::bail!("{label} 页面缺少可验证的页面定位"),
        }
        let blob = db::blobs::get(&conn, &page.object_key)?.context("视觉页面对象元数据不存在")?;
        visual_page_image_format(&blob.media_type)?;
        BlobKey::parse(&blob.object_key).context("视觉页面对象键无效")?;
        total_bytes = total_bytes
            .checked_add(blob.byte_len)
            .context("视觉页面总大小溢出")?;
        ensure!(
            total_bytes <= MAX_LOADED_VISUAL_PAGE_BYTES,
            "视觉页面总大小超过支持上限"
        );
        rows.push(PublishedVisualPageRow {
            object_key: blob.object_key,
            media_type: blob.media_type,
            byte_len: blob.byte_len,
            hash: blob.hash,
            content_unit_id: page.content_unit_id,
            locator,
            width: page.width,
            height: page.height,
        });
    }
    Ok(rows)
}

pub(crate) fn visual_page_image_format(media_type: &str) -> Result<image::ImageFormat> {
    match media_type {
        "image/png" => Ok(image::ImageFormat::Png),
        "image/jpeg" => Ok(image::ImageFormat::Jpeg),
        _ => anyhow::bail!("视觉页面使用了不支持的图片媒体类型：{media_type}"),
    }
}

pub(crate) fn visual_page_file_name(page_index: usize, media_type: &str) -> Result<String> {
    let extension = match visual_page_image_format(media_type)? {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        _ => unreachable!("visual page media types are exhaustively checked"),
    };
    Ok(format!("Page{:05}.{extension}", page_index + 1))
}
