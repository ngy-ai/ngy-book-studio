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

    pub(crate) fn finish_manual_translation(
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
                self.set_error(format!("无法保存手工译文：{message}"), cx);
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
