//! Applying a manual translation from the WebView.

use super::*;

impl ReaderApp {
    /// Saves or clears one block's manual translation.
    ///
    /// The request is only accepted from the chapter document the window is
    /// actually showing, at the revision it was pushed with; the service then
    /// revalidates the whole edit against the stored row, so a stale or forged
    /// payload can never write another block's text.
    pub(crate) fn handle_manual_translation(
        &mut self,
        url: &str,
        request: ManualTranslationRequest,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.webview_build_gate.close_requested {
            return;
        }
        // The page can only answer with the revision it was pushed with.
        let pushed_revision = self
            .annotations
            .revisions
            .1
            .get(self.current_spine)
            .copied();
        if !manual_edit_belongs_to_current_chapter(
            self.book.spine_index_for_url(url),
            self.current_spine,
            pushed_revision,
            request.revision,
        ) {
            // 页面在收到结果之前会一直显示「保存中…」/提交态；静默丢弃会把它卡死。
            self.reject_manual_translation(
                request.request_id,
                "该文本块不属于当前章节，或本章内容已更新，请重新打开本章后再试",
                "stale_page",
                cx,
            );
            return;
        }
        let Some(locator) = self.progress_locators.get(self.current_spine) else {
            self.reject_manual_translation(
                request.request_id,
                "本章缺少定位信息，请重新打开本书后再试",
                "missing_locator",
                cx,
            );
            return;
        };
        let unit_id = locator.unit_id.clone();
        let segments =
            (request.action == ManualTranslationAction::Update).then(|| request.segments.clone());
        let book_id = self.book_id.clone();
        let key = request.key.clone();
        let request_id = request.request_id;
        let url = url.to_string();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services
                .set_manual_translation(book_id, unit_id, key, segments)
                .await;
            let _ = view.update(cx, |this, cx| {
                this.finish_manual_translation(&url, request_id, result, cx);
            });
        })
        .detach();
    }

    /// Answers a save/clear request the window refused, so the page can leave
    /// its submitting state with the reason instead of waiting forever.
    fn reject_manual_translation(
        &mut self,
        request_id: u64,
        reason: &str,
        stage: &'static str,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(
            target: "ngy_reader",
            stage = "reader_manual_translation_rejected",
            reason = stage,
            "Reader manual translation request rejected"
        );
        self.translation_script(
            "result",
            serde_json::json!({
                "request_id": request_id,
                "ok": false,
                "error": format!("无法保存手工译文：{reason}"),
            }),
            cx,
        );
    }

    pub(crate) fn finish_manual_translation(
        &mut self,
        url: &str,
        request_id: u64,
        result: anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        // The page may have moved on; replying is harmless there (its pending
        // map is gone), but the error belongs to the chapter that was shown.
        let stale_page = self.current_reader_url.as_deref() != Some(url);
        let payload = match &result {
            Ok(()) => serde_json::json!({ "request_id": request_id, "ok": true }),
            Err(error) => {
                let message = format!("{error:#}");
                if !stale_page {
                    self.set_error(format!("无法保存手工译文：{message}"), cx);
                }
                serde_json::json!({ "request_id": request_id, "ok": false, "error": message })
            }
        };
        self.translation_script("result", payload, cx);
        // Only a stored edit changes what the chapter should show. A failed
        // request leaves the editor in place with the reader's own text, so
        // nothing is republished and nothing typed is thrown away.
        if result.is_ok() {
            self.load_translations(url, true, cx);
            // 手工改过的可能就是本章标题，目录随之换字。
            self.refresh_navigation_labels(cx);
        }
    }

    /// Loads the book's stored display choice once per window. Until it arrives
    /// the window shows the global preference, so an unreadable row keeps that
    /// default rather than blocking the chapter.
    pub(crate) fn load_translation_display_override(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let stored = services.translation_display_override(book_id).await;
            let _ = view.update(cx, |this, cx| {
                let stored = match stored {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(%error, "cannot read the book's translation display preference");
                        return;
                    }
                };
                if this.translations.override_mode == stored {
                    return;
                }
                this.translations.override_mode = stored;
                this.apply_translation_display_mode(cx);
            });
        })
        .detach();
    }
}
