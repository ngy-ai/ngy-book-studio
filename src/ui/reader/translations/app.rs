//! Wiring translation progress into the reader window.

use super::*;

impl ReaderApp {
    pub(crate) fn translation_script(
        &mut self,
        method: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let script = format!("window.ngyTranslations?.{method}({value});");
        self.run_reader_script(
            script,
            |this, error, cx| this.set_error(format!("无法更新译文显示：{error}"), cx),
            cx,
        );
    }

    /// Starts the background poll that surfaces the translations of the chapter
    /// being read as soon as the task commits its blocks.
    pub(crate) fn start_translation_refresh(&mut self, cx: &mut Context<Self>) {
        self.translations._refresh_task = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(TRANSLATION_REFRESH_INTERVAL)
                    .await;
                let alive = view.update(cx, |this, cx| this.poll_translation_progress(cx));
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    pub(crate) fn poll_translation_progress(&mut self, cx: &mut Context<Self>) {
        if self.closing || self.translations.polling {
            return;
        }
        let Some(url) = self.current_reader_url.clone() else {
            return;
        };
        // Without a target language nothing is ever displayed, so an idle book
        // must not pay for a task query every two seconds.
        let enabled = self
            .services
            .provider_settings()
            .is_ok_and(|settings| settings.default_language.is_some());
        if !enabled {
            return;
        }
        // The original-only mode shows no translation layer, so a moving cursor
        // cannot change the page: switching back to a translated mode forces its
        // own reload instead.
        if self.translations.effective == TranslationDisplayMode::OriginalOnly {
            return;
        }
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        self.translations.polling = true;
        cx.spawn(async move |view, cx| {
            let progress = active_translation_progress(&services, book_id).await;
            let _ = view.update(cx, |this, cx| {
                this.translations.polling = false;
                if this.closing {
                    return;
                }
                if translation_refresh_due(&mut this.translations.observed_progress, progress) {
                    this.load_translations(&url, false, cx);
                    // 任务每推进一段，目录里就可能有新的标题译好。
                    this.refresh_navigation_labels(cx);
                }
            });
        })
        .detach();
    }

    /// Loads the current chapter's translations and pushes them into the page.
    /// A newer chapter/session always wins; stale completions are dropped.
    pub(crate) fn configure_translations(&mut self, url: &str, cx: &mut Context<Self>) {
        self.load_translations(url, true, cx);
    }

    /// Re-resolves the display mode after the global preference changed in the
    /// AI settings. A book that carries its own choice keeps it; every other book
    /// follows the new default. New chapters pick the mode up on their own.
    pub(crate) fn apply_translation_display_mode(&mut self, cx: &mut Context<Self>) {
        let global = self
            .services
            .provider_settings()
            .map(|settings| settings.translation_display_mode)
            .unwrap_or_default();
        self.translations.effective = self.translations.override_mode.unwrap_or(global);
        if let Some(url) = self.current_reader_url.clone() {
            self.configure_translations(&url, cx);
        }
        cx.notify();
    }

    /// Applies the choice made in this reading window. The choice belongs to the
    /// book, so from here on it wins over the global preference.
    pub(crate) fn set_translation_display(
        &mut self,
        mode: TranslationDisplayMode,
        cx: &mut Context<Self>,
    ) {
        if self.translations.override_mode == Some(mode) && self.translations.effective == mode {
            return;
        }
        let changed = self.translations.effective != mode;
        self.translations.override_mode = Some(mode);
        self.translations.effective = mode;
        if changed {
            // 显示方式变了，正在朗读的文字也会变（译文 ↔ 原文）：先停下来，
            // 让用户从新文字重新开始，而不是让朗读跳变。
            self.stop_read_aloud(cx);
            if let Some(url) = self.current_reader_url.clone() {
                self.configure_translations(&url, cx);
            }
        }
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services
                .set_translation_display_override(book_id, Some(mode))
                .await;
            let _ = view.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.set_error(format!("无法保存本书的译文显示方式：{error:#}"), cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Drops the book's own choice so it follows the global preference again,
    /// which is also what the next window open will read back.
    pub(crate) fn follow_global_translation_display(&mut self, cx: &mut Context<Self>) {
        if self.translations.override_mode.is_none() {
            return;
        }
        self.translations.override_mode = None;
        self.apply_translation_display_mode(cx);
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services
                .set_translation_display_override(book_id, None)
                .await;
            let _ = view.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.set_error(format!("无法恢复跟随全局显示方式：{error:#}"), cx);
                }
            });
        })
        .detach();
    }
}
