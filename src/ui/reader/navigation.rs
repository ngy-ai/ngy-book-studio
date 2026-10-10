//! Moving within a chapter: table of contents, chapter hops and citations.

use super::*;

impl ReaderApp {
    pub(crate) fn explain_selection(
        &mut self,
        url: &str,
        selected_text: &str,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.webview_build_gate.close_requested {
            return;
        }
        if self.book.spine_index_for_url(url) != Some(self.current_spine) {
            self.set_error(
                "所选文本已失效，请在当前章节重新选择后解释。".to_string(),
                cx,
            );
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
            let script = format!(
                "window.ngyAnnotations?.explainSelection({});",
                serde_json::json!(selected_text)
            );
            if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
                self.set_error(format!("无法读取笔记选区：{error}"), cx);
            }
        }
    }

    pub(crate) fn go_to_toc(&mut self, toc_index: usize, cx: &mut Context<Self>) {
        if self.closing || self.annotation_navigation_blocked(cx) {
            return;
        }
        let Some(item) = self.book.toc.get(toc_index) else {
            return;
        };
        let Some(href) = item.href.as_deref() else {
            return;
        };
        let url = OpenedBook::navigation_url_for_href(href);
        let Some(webview) = &self.webview else {
            return;
        };
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.notice = Some(Notice {
                text: format!("无法打开章节：{error}"),
                error: true,
            });
            cx.notify();
            return;
        }
        self.selected_toc = Some(toc_index);
        if let Some(spine_index) = item.spine_index {
            self.current_spine = spine_index;
            self.queue_current_progress(cx);
        }
        self.selected_text = None;
        self.sync_ai_reference(cx);
        cx.notify();
    }

    pub(crate) fn change_chapter(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let next = self.current_spine as isize + offset;
        if next < 0 || next >= self.book.spine.len() as isize {
            return;
        }
        self.navigate_to_spine(next as usize, cx);
    }

    pub(crate) fn navigate_to_spine(&mut self, spine_index: usize, cx: &mut Context<Self>) {
        if self.closing || self.annotation_navigation_blocked(cx) {
            return;
        }
        // 换章即停：正在朗读的段落与高亮都属于上一章的 DOM。
        // （关闭窗口不必显式停止：会话在 `ReadAloudHost` 里，随窗口释放一起收敛。）
        self.stop_read_aloud(cx);
        let Some(spine) = self.book.spine.get(spine_index) else {
            return;
        };
        let url = OpenedBook::navigation_url_for_href(&spine.href);
        let Some(webview) = &self.webview else {
            return;
        };
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.notice = Some(Notice {
                text: format!("无法打开章节：{error}"),
                error: true,
            });
            cx.notify();
            return;
        }
        self.current_spine = spine_index;
        self.selected_toc = self
            .book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(spine_index));
        self.selected_text = None;
        self.sync_ai_reference(cx);
        self.notice = None;
        self.queue_current_progress(cx);
        cx.notify();
    }

    pub(crate) fn navigate_to_citation(
        &mut self,
        target: ReflowableCitationTarget,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.annotation_navigation_blocked(cx) {
            return;
        }
        let Some(webview) = self.webview.as_ref() else {
            self.set_error("正文视图尚未就绪，无法打开引用".to_owned(), cx);
            return;
        };
        self.pending_citation_navigation = target.focus.map(|focus| {
            self.citation_request_id = if self.citation_request_id >= (1_u64 << 53) - 1 {
                1
            } else {
                self.citation_request_id + 1
            };
            PendingReaderCitationNavigation {
                request_id: self.citation_request_id,
                spine_index: target.spine_index,
                focus,
            }
        });
        let url = OpenedBook::navigation_url_for_href(&target.href);
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.pending_citation_navigation = None;
            self.set_error(format!("无法打开引用章节：{error}"), cx);
            return;
        }
        self.current_spine = target.spine_index;
        self.selected_toc = self
            .book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(target.spine_index));
        self.selected_text = None;
        self.sync_ai_reference(cx);
        self.notice = None;
        self.queue_current_progress(cx);
        cx.notify();
    }
}
