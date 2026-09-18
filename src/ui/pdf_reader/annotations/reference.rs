//! Building the AI explanation reference for a note.

use super::*;

/// Freezes the selected page text onto the current page's canonical reference.
pub(crate) fn pdf_explanation_reference(
    book_id: &str,
    pages: &[PdfReaderPage],
    current_page: u32,
    quote: &str,
) -> Option<AiReferenceHint> {
    let selection = PdfTextSelection {
        request_id: 0,
        page_number: current_page,
        text: normalize_pdf_selection(quote),
    };
    if selection.text.is_empty() || selection.text.len() > MAX_PDF_SELECTION_BYTES {
        return None;
    }
    pdf_reference_hints(book_id, pages, current_page, 0, Some(&selection))
        .into_iter()
        .find(|reference| reference.frozen_text.is_some())
}
