//! The published visual page row and its expectations.

use super::*;

#[derive(Debug)]
pub(crate) struct PublishedVisualPageRow {
    pub(crate) object_key: String,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) hash: String,
    pub(crate) content_unit_id: Option<String>,
    pub(crate) locator: crate::document::DocumentLocator,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Expected renderer identity and page semantics for one visual page set.
pub(crate) struct PublishedPageExpectation {
    pub(crate) renderer: &'static str,
    pub(crate) fidelity: crate::preview::RenderFidelity,
    /// Source formats whose originals this pipeline may render.
    pub(crate) formats: &'static [&'static str],
    /// Whether the renderer may publish pages without a content unit
    /// (Office Word/Excel repagination previews).
    pub(crate) allow_preview_only: bool,
}

pub(crate) fn published_page_expectation(
    kind: VisualPageSourceKind,
) -> Result<PublishedPageExpectation> {
    match kind {
        #[cfg(target_os = "windows")]
        VisualPageSourceKind::OfficeEnhanced => Ok(PublishedPageExpectation {
            renderer: OFFICE_ENHANCED_RENDERER_NAME,
            fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
            formats: &["doc", "docx", "pptx", "xlsx"],
            allow_preview_only: true,
        }),
        #[cfg(not(target_os = "windows"))]
        VisualPageSourceKind::OfficeEnhanced => {
            anyhow::bail!("Office 增强预览仅支持 Windows")
        }
        VisualPageSourceKind::Djvu => Ok(PublishedPageExpectation {
            renderer: DJVU_RENDERER_NAME,
            fidelity: crate::preview::RenderFidelity::Structural,
            formats: &["djvu"],
            allow_preview_only: false,
        }),
    }
}

/// The persisted `visual_pages.fidelity` string for a render fidelity.
pub(crate) fn render_fidelity_key(fidelity: crate::preview::RenderFidelity) -> &'static str {
    match fidelity {
        crate::preview::RenderFidelity::Normalized => "normalized",
        crate::preview::RenderFidelity::Structural => "structural",
        crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
    }
}

pub(crate) fn visual_page_kind_label(kind: VisualPageSourceKind) -> &'static str {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => "Office 增强",
        VisualPageSourceKind::Djvu => "DjVu",
    }
}
