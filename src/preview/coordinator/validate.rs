//! Validating the page a renderer produced.

use super::*;

pub(crate) fn validate_rendered_page(
    spec: &VisualJobSpec,
    page: &RenderedVisualPage,
    expected_index: usize,
) -> Result<()> {
    let profile_id = spec.profile.stable_id();
    ensure!(page.page_index == expected_index, "视觉页面序号不连续");
    ensure!(page.width > 0 && page.height > 0, "视觉页面尺寸无效");
    ensure!(!page.bytes.is_empty(), "视觉页面内容为空");
    page.locator.validate().context("视觉页面 locator 无效")?;
    ensure!(
        page.locator.book_id == spec.book_id,
        "视觉页面 locator 越权"
    );
    match page.content_unit_id.as_deref() {
        Some(unit_id) => ensure!(
            page.locator.unit_id == unit_id,
            "视觉页面 locator 与内容单元不匹配"
        ),
        None => ensure!(
            matches!(
                page.locator.source.as_ref(),
                Some(SourceLocator::OfficeRenderedPage { .. })
            ) && page.locator.block_id.is_none()
                && page.locator.text_range.is_none()
                && page.locator.region.is_none()
                && page.locator.unit_id == office_preview_unit_id(&spec.book_id, &spec.source_id)
                && page.metadata.fidelity == RenderFidelity::OfficeEnhanced
                && page.metadata.unit_revision == Revision::INITIAL,
            "无内容单元的视觉页面必须是 preview-only Office 页面"
        ),
    }
    ensure!(
        page.metadata.renderer == spec.renderer
            && page.metadata.renderer_version == spec.renderer_version
            && page.metadata.fidelity == spec.fidelity
            && page.metadata.document_revision == spec.document_revision
            && page.metadata.profile_id == profile_id,
        "视觉页面渲染元数据与任务不一致"
    );
    Ok(())
}
