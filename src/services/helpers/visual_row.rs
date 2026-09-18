//! Building a visual page row from a prepared page.

use super::*;

pub(crate) fn visual_page_row(
    spec: &crate::preview::VisualJobSpec,
    page: RenderedVisualPage,
    object_key: String,
    created_at: u64,
) -> Result<db::visual_pages::VisualPage> {
    let fidelity = match page.metadata.fidelity {
        crate::preview::RenderFidelity::Normalized => "normalized",
        crate::preview::RenderFidelity::Structural => "structural",
        crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
    }
    .to_string();
    Ok(db::visual_pages::VisualPage {
        id: page.id,
        book_id: spec.book_id.clone(),
        source_id: spec.source_id.clone(),
        content_unit_id: page.content_unit_id,
        page_index: page.page_index,
        object_key,
        width: page.width,
        height: page.height,
        render_scale: spec.profile.scale(),
        renderer: page.metadata.renderer,
        renderer_version: page.metadata.renderer_version,
        document_revision: page.metadata.document_revision.get(),
        unit_revision: page.metadata.unit_revision.get(),
        profile_id: page.metadata.profile_id,
        fidelity,
        locator_json: serde_json::to_string(&page.locator)
            .context("failed to serialize visual page locator")?,
        created_at,
    })
}
