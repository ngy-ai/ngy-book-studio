//! Re-running the machine translation of one block from the WebView.

use super::*;

impl ReaderApp {
    /// Re-runs the model translation behind one block the reader is looking at.
    ///
    /// 入口约束与手工译文完全相同：请求只能来自当前正在显示的那一章、且带着推送时的
    /// 修订。阅读端只说得出块标识，序号由服务层在任务自己的文本块列表里查出来 ——
    /// 阅读端既不知道、也不该猜「第几块」。
    pub(crate) fn handle_retranslate_block(
        &mut self,
        url: &str,
        request: RetranslateBlockRequest,
        cx: &mut Context<Self>,
    ) {
        // The page can only answer with the revision it was pushed with.
        let pushed_revision = self
            .annotations
            .revisions
            .1
            .get(self.current_spine)
            .copied();
        if self.closing
            || self.webview_build_gate.close_requested
            || !manual_edit_belongs_to_current_chapter(
                self.book.spine_index_for_url(url),
                self.current_spine,
                pushed_revision,
                request.revision,
            )
        {
            return;
        }
        let Some(locator) = self.progress_locators.get(self.current_spine) else {
            return;
        };
        let unit_id = locator.unit_id.clone();
        let book_id = self.book_id.clone();
        let key = request.key.clone();
        let request_id = request.request_id;
        let url = url.to_string();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services
                .retranslate_reader_translation_block(book_id, unit_id, key)
                .await;
            let _ = view.update(cx, |this, cx| {
                this.finish_retranslate_block(&url, request_id, result, cx);
            });
        })
        .detach();
    }

    pub(crate) fn finish_retranslate_block(
        &mut self,
        url: &str,
        request_id: u64,
        result: anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.current_reader_url.as_deref() != Some(url) {
            return;
        }
        let payload = match &result {
            Ok(()) => serde_json::json!({ "request_id": request_id, "ok": true }),
            Err(error) => {
                let message = format!("{error:#}");
                self.set_error(format!("无法重新翻译该文本块：{message}"), cx);
                serde_json::json!({ "request_id": request_id, "ok": false, "error": message })
            }
        };
        self.translation_script("result", payload, cx);
        // 这里**不重载**译文：重跑的那一块还没翻完，库里还是旧文本，立刻重推只会把
        // 旧文本再推一次。后台轮询（`start_translation_refresh`）会在新译文落库时
        // 重新推送这一章 —— 页面的提示语正是这么写的。
    }
}
