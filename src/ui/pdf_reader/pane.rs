//! The pane drag and resolving a source to a page.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PdfPaneResizeDrag(pub(crate) ReaderResizablePane);

pub(crate) fn pdf_page_for_source(pages: &[PdfReaderPage], source: &AiSourceLink) -> Option<u32> {
    if source.stale {
        return None;
    }
    if let Some(locator) = source.validated_locator() {
        match locator.source.as_ref() {
            Some(SourceLocator::PdfPage { page }) => {
                return pages
                    .iter()
                    .any(|candidate| {
                        candidate.page_number == *page
                            && candidate.unit_id.as_deref() == Some(source.unit_id.as_str())
                    })
                    .then_some(*page);
            }
            Some(_) => return None,
            None => {}
        }
    }
    if source.locator.is_some() && source.validated_locator().is_none() {
        return None;
    }
    pages
        .iter()
        .find(|page| page.unit_id.as_deref() == Some(source.unit_id.as_str()))
        .map(|page| page.page_number)
}

pub(crate) fn pdf_source_requires_exact_page(source: &AiSourceLink) -> bool {
    source.locator.as_ref().is_some_and(|_| {
        source
            .validated_locator()
            .is_none_or(|locator| locator.source.is_some())
    })
}
