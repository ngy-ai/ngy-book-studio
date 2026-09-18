//! Building the reference hints handed to the AI sidebar.

use super::*;

pub(crate) fn pdf_reference_hints(
    book_id: &str,
    pages: &[PdfReaderPage],
    current_page: u32,
    current_request_id: u64,
    selection: Option<&PdfTextSelection>,
) -> Vec<AiReferenceHint> {
    let mut pages = pages
        .iter()
        .filter(|page| page.unit_id.is_some())
        .collect::<Vec<_>>();
    pages.sort_by_key(|page| (page.page_number != current_page, page.page_number));
    pages
        .into_iter()
        .filter_map(|page| {
            let unit_id = page.unit_id.as_ref()?;
            let selected_text = selection
                .filter(|selection| {
                    selection.request_id == current_request_id
                        && selection.page_number == page.page_number
                })
                .map(|selection| selection.text.clone());
            Some(AiReferenceHint {
                book_id: book_id.to_string(),
                unit_id: unit_id.clone(),
                unit_index: page.unit_index,
                locator: Some(
                    DocumentLocator::unit(book_id, unit_id)
                        .with_source(SourceLocator::pdf_page(page.page_number)),
                ),
                label: if selected_text.is_some() {
                    // A continuous reader can select text on a page that is not
                    // the one reported as current, so name the page instead.
                    format!("选中高亮 · 第 {} 页 · {}", page.page_number, page.title)
                } else if page.page_number == current_page {
                    format!("当前页 · 第 {} 页 · {}", page.page_number, page.title)
                } else {
                    format!("第 {} 页 · {}", page.page_number, page.title)
                },
                frozen_text: selected_text,
                displayed_text: None,
                revision: None,
            })
        })
        .collect()
}
