//! Re-running the machine translation of one block from the WebView.

use super::*;

impl ReaderApp {
    /// Re-runs the model translation behind one block the reader is looking at.
    ///
    /// 入口约束与手工译文相同：请求只能来自当前正在显示的那一章、且带着推送时的
    /// 修订。阅读端只说得出块标识，序号由服务层在任务自己的文本块列表里查出来 ——
    /// 阅读端既不知道、也不该猜「第几块」。
    ///
    /// 被拒绝的请求也必须回答：页面在收到结果之前会一直显示「正在提交重新翻译…」，
    /// 静默 return 会把这一行永远卡在提交中。服务层随后按块标识在当前修订里查找，
    /// 所以陈旧的页面载荷不会写到另一块上 —— 页面 revision 只用于诊断。
    pub(crate) fn handle_retranslate_block(
        &mut self,
        url: &str,
        request: RetranslateBlockRequest,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.webview_build_gate.close_requested {
            return;
        }
        let Some(spine) = self.book.spine_index_for_url(url) else {
            self.reject_retranslate_block(
                request.request_id,
                "该文本块不属于这本书的任何一章",
                "unknown_chapter",
                cx,
            );
            return;
        };
        if spine != self.current_spine {
            self.reject_retranslate_block(
                request.request_id,
                "该文本块不在当前章节",
                "other_chapter",
                cx,
            );
            return;
        }
        let Some(locator) = self.progress_locators.get(self.current_spine) else {
            self.reject_retranslate_block(
                request.request_id,
                "本章缺少定位信息，请重新打开本书后再试",
                "missing_locator",
                cx,
            );
            return;
        };
        let pushed_revision = self
            .annotations
            .revisions
            .1
            .get(self.current_spine)
            .copied()
            .unwrap_or(0);
        if pushed_revision != request.revision {
            // 块标识才是写入目标：服务层会在任务自己的当前块列表里重新查它，查不到
            // 就报错。陈旧页面因此不会挑错块，但留一条读数方便定位。
            tracing::info!(
                target: "ngy_reader",
                stage = "reader_retranslate_stale_page",
                pushed_revision,
                requested_revision = request.revision,
                "Reader retranslation request came from an older payload"
            );
        }
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

    /// Answers a request the window refused to schedule, so the page can leave
    /// its「正在提交重新翻译…」state with the reason instead of waiting forever.
    fn reject_retranslate_block(
        &mut self,
        request_id: u64,
        reason: &str,
        stage: &'static str,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(
            target: "ngy_reader",
            stage = "reader_retranslate_rejected",
            reason = stage,
            "Reader retranslation request rejected"
        );
        self.translation_script(
            "result",
            serde_json::json!({
                "request_id": request_id,
                "ok": false,
                "error": format!("无法重新翻译该文本块：{reason}"),
            }),
            cx,
        );
    }

    pub(crate) fn finish_retranslate_block(
        &mut self,
        url: &str,
        request_id: u64,
        result: anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        // The page may have moved to another chapter while the task was planned.
        // Its `pending` map goes away with the old document, so replying is
        // harmless there; showing the error in the new chapter would not be.
        let stale_page = self.current_reader_url.as_deref() != Some(url);
        let payload = match &result {
            Ok(()) => serde_json::json!({ "request_id": request_id, "ok": true }),
            Err(error) => {
                let message = format!("{error:#}");
                if !stale_page {
                    self.set_error(format!("无法重新翻译该文本块：{message}"), cx);
                }
                serde_json::json!({ "request_id": request_id, "ok": false, "error": message })
            }
        };
        self.translation_script("result", payload, cx);
        // 这里**不重载**译文：重跑的那一块还没翻完，库里还是旧文本，立刻重推只会把
        // 旧文本再推一次。后台轮询（`start_translation_refresh`）会在新译文落库时
        // 重新推送这一章 —— 页面的提示语正是这么写的。
    }
}
