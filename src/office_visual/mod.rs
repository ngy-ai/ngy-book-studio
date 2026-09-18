//! Persistent visual renderer backed by the optional Microsoft Office COM
//! enhancement. The renderer consumes only the byte-exact, book-scoped
//! original supplied by [`VisualDocumentSource`]; temporary Office artifacts
//! never become document-domain state.

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, ensure};

use crate::{
    document::{
        BookDocument, BookFormat, BookSource, DocumentLocator, Revision, SourceLocator,
        deterministic_id,
    },
    office_com::{OfficeCancellation, OfficeEnhancer},
    office_preview::{EnhancedOfficePreview, OfficePreviewService},
    preview::{
        PreviewFuture, RenderControl, RenderFidelity, RenderMetadata, RenderPageEmitter,
        RenderUnitsRequest, RenderedVisualPage, RendererDescriptor, VisualRenderer,
        VisualSourcePayload, office_preview_unit_id,
    },
    windows_pdf_renderer::{PdfRasterTarget, rasterize_pdf_bytes},
};

mod enhance;

mod renderer;

mod slides;

mod units;

pub(crate) use enhance::*;

pub(crate) use slides::*;

pub(crate) use units::*;

#[cfg(test)]
mod tests;
pub(crate) const OFFICE_ENHANCED_RENDERER_NAME: &str = "ngy-office-com-enhanced";
pub(crate) const MAX_ENHANCED_PAGES: usize = 20_000;
pub(crate) const MAX_ENHANCED_PDF_PAGES: usize = 4_096;
pub(crate) const MAX_ENHANCED_SLIDE_BYTES: usize = 12 * 1024 * 1024;
pub(crate) const MAX_ENHANCED_BATCH_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_ENHANCED_SLIDE_DIMENSION: u32 = 8_192;
pub(crate) const MAX_ENHANCED_SLIDE_PIXELS: u64 = 32 * 1024 * 1024;
pub(crate) const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct OfficeEnhancedRenderer {
    pub(crate) preview: OfficePreviewService,
}

impl std::fmt::Debug for OfficeEnhancedRenderer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OfficeEnhancedRenderer")
            .field("descriptor", &self.descriptor())
            .finish_non_exhaustive()
    }
}

impl OfficeEnhancedRenderer {
    pub fn new(enhancer: Arc<dyn OfficeEnhancer>) -> Self {
        Self {
            preview: OfficePreviewService::new(enhancer),
        }
    }
}
