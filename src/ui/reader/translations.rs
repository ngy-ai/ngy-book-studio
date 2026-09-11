//! Host bridge that pushes whole-book translations into the reading WebView.
//!
//! Translations are looked up per content unit, matched to the chapter DOM by
//! their source text, and rendered as a sibling layer the notes runtime never
//! treats as book text. The host keeps only a monotonic session/generation so a
//! late completion for a previous chapter can never overwrite the current one.
use super::*;
use moye_epub_editor::services::{TranslatedBlock, TranslationDisplayMode};

pub(super) struct ReaderTranslations {
    session: String,
    generation: u64,
}

impl ReaderTranslations {
    pub(super) fn new() -> Self {
        Self {
            session: String::new(),
            generation: 0,
        }
    }
}

impl ReaderApp {
    fn translation_script(
        &mut self,
        method: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(webview) = self.webview.as_ref() else {
            return;
        };
        let script = format!("window.moyeTranslations?.{method}({value});");
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.set_error(format!("无法更新译文显示：{error}"), cx);
        }
    }

    /// Loads the current chapter's translations and pushes them into the page.
    /// A newer chapter/session always wins; stale completions are dropped.
    pub(super) fn configure_translations(&mut self, url: &str, cx: &mut Context<Self>) {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        self.translations.session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
        self.translations.generation = self.translations.generation.wrapping_add(1);
        let generation = self.translations.generation;
        let session = self.translations.session.clone();
        let Some(spine_index) = self.book.spine_index_for_url(url) else {
            return;
        };
        let Some(locator) = self.progress_locators.get(spine_index) else {
            return;
        };
        let unit_id = locator.unit_id.clone();
        let revision = self
            .annotations
            .revisions
            .1
            .get(spine_index)
            .copied()
            .unwrap_or(0);
        let book_id = self.book_id.clone();
        let display_mode = self
            .services
            .provider_settings()
            .map(|settings| settings.translation_display_mode)
            .unwrap_or_default();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services.translation_blocks_for_unit(book_id, unit_id).await;
            let _ = view.update(cx, |this, cx| {
                if this.closing || this.translations.generation != generation {
                    return;
                }
                let blocks = match result {
                    Ok(blocks) => blocks,
                    Err(error) => {
                        this.set_error(format!("无法加载译文：{error:#}"), cx);
                        return;
                    }
                };
                this.translation_script(
                    "configure",
                    serde_json::json!({
                        "session": session,
                        "revision": revision,
                        "displayMode": match display_mode {
                            TranslationDisplayMode::Bilingual => "bilingual",
                            TranslationDisplayMode::TranslationOnly => "translation-only",
                        },
                        "blocks": blocks.iter().map(translated_block_json).collect::<Vec<_>>(),
                    }),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Re-applies the translation display preference to the currently loaded page
    /// after it changes in the AI settings; new chapters pick it up automatically.
    pub(crate) fn apply_translation_display_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = self.current_reader_url.clone() {
            self.configure_translations(&url, cx);
        }
    }
}

fn translated_block_json(block: &TranslatedBlock) -> serde_json::Value {
    serde_json::json!({
        "key": block.key,
        "source": block.source,
        "translated": block.translated,
    })
}
