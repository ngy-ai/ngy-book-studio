//! Loading persisted translations and display overrides.

use super::*;

impl ReaderApp {
    /// Reloads one chapter's translations. `force` marks the loads that must
    /// republish unconditionally (chapter change, display preference change);
    /// the background refresh passes `false` and skips a push whose chapter and
    /// payload are already on screen.
    pub(crate) fn load_translations(&mut self, url: &str, force: bool, cx: &mut Context<Self>) {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
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
        let display_mode = self.translations.effective;
        // A forced load supersedes any in-flight load immediately. The
        // background refresh only supersedes one once it has something new to
        // push, so a skipped poll leaves the in-flight load alone.
        let generation = if force {
            self.translations.generation = self.translations.generation.wrapping_add(1);
            self.translations.generation
        } else {
            self.translations.generation
        };
        // The original-only mode shows no translation layer at all. Pushing an
        // empty payload clears whatever the page still shows after a mode change
        // without asking the database for blocks nobody will display.
        if display_mode == TranslationDisplayMode::OriginalOnly {
            let label = translation_display_mode_label(display_mode);
            let fingerprint = translation_fingerprint(&unit_id, label, &[]);
            if !force && self.translations.pushed.as_ref() == Some(&(unit_id.clone(), fingerprint))
            {
                return;
            }
            let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
            self.translations.session = session.clone();
            self.translations.pushed = Some((unit_id, fingerprint));
            self.translation_script(
                "configure",
                serde_json::json!({
                    "session": session,
                    "revision": revision,
                    "displayMode": label,
                    "blocks": Vec::<serde_json::Value>::new(),
                }),
                cx,
            );
            return;
        }
        let book_id = self.book_id.clone();
        let url = url.to_string();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services
                .translation_blocks_for_unit(book_id, unit_id.clone())
                .await;
            let _ = view.update(cx, |this, cx| {
                if this.closing
                    || this.translations.generation != generation
                    || this.current_reader_url.as_deref() != Some(url.as_str())
                {
                    return;
                }
                let blocks = match result {
                    Ok(blocks) => blocks,
                    Err(error) => {
                        this.set_error(format!("无法加载译文：{error:#}"), cx);
                        return;
                    }
                };
                let payload = blocks.iter().map(translated_block_json).collect::<Vec<_>>();
                let display_mode = translation_display_mode_label(display_mode);
                let fingerprint = translation_fingerprint(&unit_id, display_mode, &payload);
                if !force
                    && this.translations.pushed.as_ref() == Some(&(unit_id.clone(), fingerprint))
                {
                    return;
                }
                let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
                this.translations.session = session.clone();
                this.translations.generation = generation.wrapping_add(1);
                this.translations.pushed = Some((unit_id.clone(), fingerprint));
                this.translation_script(
                    "configure",
                    serde_json::json!({
                        "session": session,
                        "revision": revision,
                        "displayMode": display_mode,
                        "blocks": payload,
                    }),
                    cx,
                );
            });
        })
        .detach();
    }
}
