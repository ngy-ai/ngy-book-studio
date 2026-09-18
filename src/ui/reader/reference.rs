//! Building the reference hints handed to the AI sidebar.

use super::*;

pub(crate) fn reader_explanation_reference(
    book_id: &str,
    book: &OpenedBook,
    progress_locators: &[DocumentLocator],
    current_spine: usize,
    url: &str,
    selected_text: &str,
    displayed_text: Option<&str>,
) -> Option<AiReferenceHint> {
    let uri = url.parse().ok()?;
    if !is_reader_document_uri(&uri) || book.spine_index_for_url(url) != Some(current_spine) {
        return None;
    }
    let selected_text = normalize_reader_selection(selected_text)??;
    let mut reference = reader_reference_hints(
        book_id,
        &book.spine,
        progress_locators,
        current_spine,
        Some(&selected_text),
    )
    .into_iter()
    .find(|reference| reference.frozen_text.is_some())?;
    // The reader may have selected text of a reading-time translation. The model
    // then explains what the reader saw, while the frozen quote stays the
    // original behind every citation, note anchor and version check. An empty or
    // oversized report is simply dropped: the explanation falls back to the
    // original passage.
    reference.displayed_text = displayed_text
        .and_then(normalize_reader_selection)
        .flatten()
        .filter(|text| text.len() <= MAX_READER_SELECTION_BYTES);
    Some(reference)
}

pub(crate) fn reader_reference_hints(
    book_id: &str,
    spine: &[ngy_book_studio::reader::SpineItem],
    progress_locators: &[DocumentLocator],
    current_spine: usize,
    selected_text: Option<&str>,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..spine.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != current_spine, *index));
    indices
        .into_iter()
        .filter_map(|index| {
            let spine = spine.get(index)?;
            let locator = progress_locators.get(index)?;
            if locator.book_id != book_id || locator.validate().is_err() {
                return None;
            }
            let mut reference = AiReferenceHint::chapter(
                book_id,
                locator.unit_id.clone(),
                index,
                if index == current_spine {
                    if selected_text.is_some() {
                        format!("当前章节高亮 · {}", spine.title)
                    } else {
                        format!("当前章节 · {}", spine.title)
                    }
                } else {
                    spine.title.clone()
                },
            );
            reference.locator = Some(locator.clone());
            if index == current_spine {
                reference.frozen_text = selected_text.map(str::to_string);
            }
            Some(reference)
        })
        .collect()
}
