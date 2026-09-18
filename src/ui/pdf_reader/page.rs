//! Requesting a page and resolving the AI reference for it.

use super::*;

impl PdfReaderApp {
    pub(crate) fn request_page(&mut self, page_number: u32, cx: &mut Context<Self>) {
        if self.closing || !self.pdf_ready || self.annotation_navigation_blocked(cx) {
            return;
        }
        let page_number = page_number.clamp(1, self.actual_page_count.max(1));
        let Some(webview) = self.webview.clone() else {
            return;
        };
        if self.pdf_selection.take().is_some() {
            self.sync_ai_reference(cx);
        }
        self.page_request_id = self.page_request_id.wrapping_add(1).max(1);
        let payload = serde_json::json!({
            "type": "ngy-pdf-go-to",
            "requestId": self.page_request_id,
            "pageNumber": page_number,
        });
        let script = format!("window.postMessage({payload}, window.location.origin);");
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.notice = Some(Notice {
                text: format!("无法打开第 {page_number} 页：{error}"),
                error: true,
            });
            cx.notify();
        }
    }

    pub(crate) fn current_page_info(&self) -> Option<&PdfReaderPage> {
        self.pages
            .iter()
            .find(|page| page.page_number == self.current_page)
    }

    pub(crate) fn page_for_unit_index(&self, unit_index: Option<usize>) -> Option<u32> {
        let unit_index = unit_index?;
        self.pages
            .iter()
            .find(|page| page.unit_index == Some(unit_index))
            .map(|page| page.page_number)
    }

    pub(crate) fn open_canonical_unit(
        &mut self,
        unit_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page_number) = self.page_for_unit_index(Some(unit_index)) {
            self.request_page(page_number, cx);
            return;
        }
        let book_id = self.book_id.clone();
        self.library_view.update(cx, |library, cx| {
            library.open_book_at(book_id, Some(unit_index), window, cx);
        });
    }

    pub(crate) fn sync_ai_reference(&mut self, cx: &mut Context<Self>) {
        let references = pdf_reference_hints(
            &self.book_id,
            &self.pages,
            self.current_page,
            self.page_request_id,
            self.pdf_selection.as_ref(),
        );
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }
}
