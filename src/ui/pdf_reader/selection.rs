//! Validating the text selection reported by the PDF viewer.

use super::*;

pub(crate) fn normalize_pdf_selection(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut separated = false;
    for character in text.chars() {
        if character.is_whitespace() || character.is_control() {
            separated = !normalized.is_empty();
        } else {
            if separated {
                normalized.push(' ');
                separated = false;
            }
            normalized.push(character);
        }
    }
    normalized
}

/// Validates a selection reported by the continuous reader.
///
/// The viewer reports the page that owns the selection, which is not always the
/// page it is reporting as current while the document scrolls, so the page is
/// checked against the document instead of the current position. The request id
/// still binds the message to the navigation that produced it, and an empty
/// selection clears the frozen reference.
pub(crate) fn validated_pdf_selection(
    expected_request_id: u64,
    page_count: u32,
    request_id: u64,
    page_number: u32,
    selected_text: &str,
) -> Option<Option<PdfTextSelection>> {
    if request_id != expected_request_id
        || page_number == 0
        || page_number > page_count.max(1)
        || selected_text.len() > MAX_PDF_SELECTION_BYTES
    {
        return None;
    }
    let text = normalize_pdf_selection(selected_text);
    if text.len() > MAX_PDF_SELECTION_BYTES {
        return None;
    }
    Some((!text.is_empty()).then_some(PdfTextSelection {
        request_id,
        page_number,
        text,
    }))
}

pub(crate) fn is_pdf_navigation_url(url: &str) -> bool {
    url.starts_with("ngypdf://viewer/")
        || url.starts_with("http://ngypdf.viewer/")
        || url.starts_with("https://ngypdf.viewer/")
}
