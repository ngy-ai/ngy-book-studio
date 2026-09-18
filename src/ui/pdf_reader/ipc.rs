//! Handling the messages the viewer sends back.

use super::*;

impl PdfReaderApp {
    pub(crate) fn handle_ipc(&mut self, message: PdfIpcMessage, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        match message {
            PdfIpcMessage::ViewerReady => {
                // The document is loaded, so a preference saved while this
                // WebView was still building now reaches a live page.
                self.push_pdf_compact_reading(cx);
                self.push_pdf_zoom(cx);
                self.push_pdf_appearance(cx);
                self.notice = Some(Notice {
                    text: "正在解析本地 PDF…".to_string(),
                    error: false,
                });
            }
            PdfIpcMessage::Ready {
                request_id,
                page_count,
            } if request_id == self.page_request_id && page_count > 0 => {
                self.actual_page_count = page_count;
                self.current_page = self.current_page.min(page_count).max(1);
                self.pdf_ready = true;
                self.notice = None;
                self.configure_annotations(cx);
            }
            PdfIpcMessage::PageChanged {
                request_id,
                page_number,
                page_count,
            } if request_id == self.page_request_id
                && page_count > 0
                && (1..=page_count).contains(&page_number) =>
            {
                self.actual_page_count = page_count;
                self.current_page = page_number;
                self.pdf_selection = None;
                self.pdf_ready = true;
                self.sync_ai_reference(cx);
                self.notice = None;
                self.persist_progress(cx);
                self.configure_annotations(cx);
            }
            PdfIpcMessage::SelectionChanged {
                request_id,
                page_number,
                selected_text,
            } => {
                let Some(selection) = validated_pdf_selection(
                    self.page_request_id,
                    self.actual_page_count,
                    request_id,
                    page_number,
                    &selected_text,
                ) else {
                    return;
                };
                if self.pdf_selection != selection {
                    self.pdf_selection = selection;
                    self.sync_ai_reference(cx);
                }
            }
            PdfIpcMessage::Error {
                request_id,
                message,
            } if request_id == self.page_request_id => {
                self.notice = Some(Notice {
                    text: format!("PDF 预览失败：{message}"),
                    error: true,
                });
            }
            PdfIpcMessage::AnnotationAction { action } => {
                if action.valid() {
                    self.handle_annotation_action(action, cx);
                }
                return;
            }
            PdfIpcMessage::ExplainSelection { selected_text, url } => {
                if is_pdf_navigation_url(&url) {
                    self.explain_selection(&selected_text, cx);
                }
                return;
            }
            PdfIpcMessage::RequestPage { delta } if matches!(delta, -1 | 1) => {
                let current = self.current_page;
                let total = self.actual_page_count.max(1);
                let target = if delta < 0 {
                    if current <= 1 {
                        return;
                    }
                    current.saturating_sub(1)
                } else {
                    if current >= total {
                        return;
                    }
                    current.saturating_add(1)
                };
                self.request_page(target, cx);
            }
            // The viewer already re-laid out at the scale the reader wheeled to,
            // so the host only stores it. A scale outside the range the viewer
            // clamps means the message did not come from it, and is dropped
            // rather than clamped into a size the reader never chose.
            PdfIpcMessage::ZoomChanged { zoom_milli }
                if (PDF_ZOOM_MIN_MILLI..=PDF_ZOOM_MAX_MILLI).contains(&zoom_milli) =>
            {
                if self.pdf_zoom_milli != zoom_milli {
                    self.pdf_zoom_milli = zoom_milli;
                    self.schedule_pdf_zoom_save(cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }
}
