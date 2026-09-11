//! Host bridge that pushes whole-book translations into the reading WebView.
//!
//! Translations are looked up per content unit, matched to the chapter DOM by
//! their source text, and rendered as a sibling layer the notes runtime never
//! treats as book text. The host keeps only a monotonic session/generation so a
//! late completion for a previous chapter can never overwrite the current one.
//!
//! A chapter becomes readable as soon as its own blocks are translated: the
//! background task commits every block on its own and nothing waits for the
//! whole book. The reader therefore polls the task cursor and reloads the
//! chapter being read when new blocks land, so a finished chapter appears
//! without switching away and back.
use super::*;
use moye_epub_editor::services::{BackgroundJobStatus, TranslatedBlock, TranslationDisplayMode};

/// Task kind of a whole-book translation job, as persisted in `index_jobs`.
const TRANSLATION_JOB_KIND: &str = "translation";
/// How often the reader asks whether the book's translation task advanced.
const TRANSLATION_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub(super) struct ReaderTranslations {
    session: String,
    generation: u64,
    /// Highest block count reported by the book's unfinished translation task
    /// at the last poll; `None` while nothing is queued, running, or paused.
    observed_progress: Option<usize>,
    /// Chapter and payload fingerprint the page currently shows, so the poll
    /// never rewrites the DOM with translations that are already displayed.
    pushed: Option<(String, u64)>,
    /// Set while a poll is in flight so a slow query cannot stack ticks.
    polling: bool,
    /// Choice made in this reading window for this book. `None` means the book
    /// still follows the global "system configuration" preference; a stored
    /// choice always wins over it.
    override_mode: Option<TranslationDisplayMode>,
    /// Mode currently applied to the page: the book's choice, or the global
    /// preference while the book has none.
    effective: TranslationDisplayMode,
    /// Keeps the refresh task alive for the window's lifetime.
    _refresh_task: Option<Task<()>>,
}

impl ReaderTranslations {
    pub(super) fn new(global: TranslationDisplayMode) -> Self {
        Self {
            session: String::new(),
            generation: 0,
            observed_progress: None,
            pushed: None,
            polling: false,
            override_mode: None,
            effective: global,
            _refresh_task: None,
        }
    }

    /// The mode the reading window switch shows and the page renders with.
    pub(super) fn effective(&self) -> TranslationDisplayMode {
        self.effective
    }

    /// Whether the book carries its own choice instead of following the global
    /// preference. The reading window only offers "跟随全局" while it does.
    pub(super) fn overrides_global(&self) -> bool {
        self.override_mode.is_some()
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

    /// Starts the background poll that surfaces the translations of the chapter
    /// being read as soon as the task commits its blocks.
    pub(super) fn start_translation_refresh(&mut self, cx: &mut Context<Self>) {
        self.translations._refresh_task = Some(cx.spawn(async move |view, cx| {
            loop {
                Timer::after(TRANSLATION_REFRESH_INTERVAL).await;
                let alive = view.update(cx, |this, cx| this.poll_translation_progress(cx));
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    fn poll_translation_progress(&mut self, cx: &mut Context<Self>) {
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
                }
            });
        })
        .detach();
    }

    /// Loads the current chapter's translations and pushes them into the page.
    /// A newer chapter/session always wins; stale completions are dropped.
    pub(super) fn configure_translations(&mut self, url: &str, cx: &mut Context<Self>) {
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
    pub(super) fn set_translation_display(
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
    pub(super) fn follow_global_translation_display(&mut self, cx: &mut Context<Self>) {
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

    /// Loads the book's stored display choice once per window. Until it arrives
    /// the window shows the global preference, so an unreadable row keeps that
    /// default rather than blocking the chapter.
    pub(super) fn load_translation_display_override(&mut self, cx: &mut Context<Self>) {
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

    /// Reloads one chapter's translations. `force` marks the loads that must
    /// republish unconditionally (chapter change, display preference change);
    /// the background refresh passes `false` and skips a push whose chapter and
    /// payload are already on screen.
    fn load_translations(&mut self, url: &str, force: bool, cx: &mut Context<Self>) {
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

/// Highest block count reported by the book's unfinished translation task.
/// `None` means no translation task is queued, running, or paused, which is
/// also the signal that the last committed blocks are worth one final reload.
async fn active_translation_progress(services: &AppServices, book_id: String) -> Option<usize> {
    let jobs = services
        .background_jobs_for_books(vec![book_id])
        .await
        .ok()?;
    jobs.into_iter()
        .filter(|job| {
            job.kind == TRANSLATION_JOB_KIND
                && matches!(
                    job.status,
                    BackgroundJobStatus::Queued
                        | BackgroundJobStatus::Running
                        | BackgroundJobStatus::Paused
                )
        })
        .map(|job| job.progress.completed)
        .max()
}

/// Whether the observed task progress means the current chapter must be
/// reloaded. A running task is polled block by block, and the transition to
/// "no task" reloads once so blocks committed between the last two polls are
/// not lost; an idle book never triggers a query.
fn translation_refresh_due(observed: &mut Option<usize>, current: Option<usize>) -> bool {
    let changed = *observed != current;
    let was_active = observed.is_some();
    *observed = current;
    changed && (current.is_some() || was_active)
}

fn translation_display_mode_label(mode: TranslationDisplayMode) -> &'static str {
    match mode {
        TranslationDisplayMode::Bilingual => "bilingual",
        TranslationDisplayMode::OriginalOnly => "original-only",
        TranslationDisplayMode::TranslationOnly => "translation-only",
    }
}

/// Fingerprint of one chapter's rendered translation layer. The session string
/// is deliberately excluded: it changes on every load, while the fingerprint
/// must only change when the chapter or its translations really did.
fn translation_fingerprint(unit_id: &str, display_mode: &str, blocks: &[serde_json::Value]) -> u64 {
    let payload = serde_json::to_string(blocks).unwrap_or_default();
    let mut buffer = Vec::with_capacity(unit_id.len() + display_mode.len() + payload.len() + 2);
    buffer.extend_from_slice(unit_id.as_bytes());
    buffer.push(0);
    buffer.extend_from_slice(display_mode.as_bytes());
    buffer.push(0);
    buffer.extend_from_slice(payload.as_bytes());
    let digest = blake3::hash(&buffer);
    let mut truncated = [0u8; 8];
    truncated.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(truncated)
}

fn translated_block_json(block: &TranslatedBlock) -> serde_json::Value {
    serde_json::json!({
        "key": block.key,
        "source": block.source,
        "segments": block.segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_follows_task_progress_and_settles_when_idle() {
        let mut observed = None;
        assert!(!translation_refresh_due(&mut observed, None));
        assert_eq!(observed, None);

        assert!(translation_refresh_due(&mut observed, Some(0)));
        assert_eq!(observed, Some(0));
        assert!(!translation_refresh_due(&mut observed, Some(0)));

        assert!(translation_refresh_due(&mut observed, Some(7)));
        assert_eq!(observed, Some(7));

        // The task finished: reload once more for the blocks committed between
        // the last two polls, then stay quiet.
        assert!(translation_refresh_due(&mut observed, None));
        assert_eq!(observed, None);
        assert!(!translation_refresh_due(&mut observed, None));
    }

    #[test]
    fn every_display_mode_reaches_the_page_with_its_own_label() {
        assert_eq!(
            translation_display_mode_label(TranslationDisplayMode::Bilingual),
            "bilingual"
        );
        assert_eq!(
            translation_display_mode_label(TranslationDisplayMode::OriginalOnly),
            "original-only"
        );
        assert_eq!(
            translation_display_mode_label(TranslationDisplayMode::TranslationOnly),
            "translation-only"
        );
        // Switching mode must rewrite the page even when the blocks are equal,
        // so the label is part of the fingerprint.
        let blocks = serde_json::json!([{ "key": "b1", "source": "one", "segments": [] }]);
        assert_ne!(
            translation_fingerprint("unit-1", "translation-only", std::slice::from_ref(&blocks)),
            translation_fingerprint("unit-1", "original-only", std::slice::from_ref(&blocks))
        );
    }

    #[test]
    fn fingerprint_only_moves_with_the_pushed_payload() {
        let blocks = serde_json::json!([{ "key": "b1", "source": "one", "segments": [] }]);
        let translated = serde_json::json!([{
            "key": "b1",
            "source": "one",
            "segments": [{ "source": "one", "translated": "一" }],
        }]);
        let base = translation_fingerprint("unit-1", "bilingual", std::slice::from_ref(&blocks));
        assert_eq!(
            base,
            translation_fingerprint("unit-1", "bilingual", std::slice::from_ref(&blocks))
        );
        assert_ne!(
            base,
            translation_fingerprint("unit-2", "bilingual", std::slice::from_ref(&blocks))
        );
        assert_ne!(
            base,
            translation_fingerprint("unit-1", "translation-only", std::slice::from_ref(&blocks))
        );
        assert_ne!(
            base,
            translation_fingerprint("unit-1", "bilingual", std::slice::from_ref(&translated))
        );
    }
}
