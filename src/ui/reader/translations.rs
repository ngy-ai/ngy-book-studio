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
use moye_epub_editor::translation::TranslationSegment;

/// Task kind of a whole-book translation job, as persisted in `index_jobs`.
const TRANSLATION_JOB_KIND: &str = "translation";
/// How often the reader asks whether the book's translation task advanced.
const TRANSLATION_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
/// Upper bounds for one manual translation request. The host re-reads the stored
/// row and revalidates the edit, so these only reject nonsense before the work
/// starts; the same limits are enforced again by the service.
const MAX_MANUAL_TRANSLATION_KEY_BYTES: usize = 256;
const MAX_MANUAL_TRANSLATION_SEGMENTS: usize = 512;
const MAX_MANUAL_TRANSLATION_SEGMENT_BYTES: usize = 8 * 1024;
const MAX_MANUAL_TRANSLATION_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::ui) enum ManualTranslationAction {
    /// Replace the model output of one block with the reader's own text.
    Update,
    /// Drop the reader's own text and show the model output again.
    Restore,
}

/// One manual translation edit submitted by the reading window.
///
/// The reader may only describe the edit. Which block it belongs to, what its
/// original text is and whether it is still current are decided by the host from
/// the stored row, never from this payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(in crate::ui) struct ManualTranslationRequest {
    action: ManualTranslationAction,
    revision: u64,
    request_id: u64,
    key: String,
    #[serde(default)]
    segments: Vec<TranslationSegment>,
}

impl ManualTranslationRequest {
    pub(super) fn valid(&self) -> bool {
        let mut total = 0;
        let segments_ok = self.segments.iter().all(|segment| {
            // Count both halves: the host stores the sources beside the text, and
            // this bound stands in for that serialized row.
            total += segment.source.len() + segment.translated.len();
            // A translation that is only whitespace cannot have come from a leaf
            // that had visible text, and the service rejects it anyway.
            !segment.source.trim().is_empty()
                && !segment.translated.trim().is_empty()
                && segment.source.len() <= MAX_MANUAL_TRANSLATION_SEGMENT_BYTES
                && segment.translated.len() <= MAX_MANUAL_TRANSLATION_SEGMENT_BYTES
        });
        !self.key.is_empty()
            && self.key.len() <= MAX_MANUAL_TRANSLATION_KEY_BYTES
            && (1..=(1_u64 << 53) - 1).contains(&self.request_id)
            && self.segments.len() <= MAX_MANUAL_TRANSLATION_SEGMENTS
            && segments_ok
            && total <= MAX_MANUAL_TRANSLATION_BYTES
            && match self.action {
                ManualTranslationAction::Update => !self.segments.is_empty(),
                ManualTranslationAction::Restore => self.segments.is_empty(),
            }
    }
}

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

    /// Saves or clears one block's manual translation.
    ///
    /// The request is only accepted from the chapter document the window is
    /// actually showing, at the revision it was pushed with; the service then
    /// revalidates the whole edit against the stored row, so a stale or forged
    /// payload can never write another block's text.
    pub(super) fn handle_manual_translation(
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

    fn finish_manual_translation(
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
        }
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

/// Whether a manual edit may be applied to the chapter the window is showing.
///
/// The page can only answer for the chapter the host pushed into it and with
/// the revision that push carried, so an edit naming another chapter, or an
/// older payload than the one on screen, is dropped here before any database
/// work starts. A chapter without a revision entry is pushed as 0 and therefore
/// has to accept an echoed 0: rejecting that would forbid editing the
/// translations of such a chapter outright.
fn manual_edit_belongs_to_current_chapter(
    edited_chapter: Option<usize>,
    current_chapter: usize,
    pushed_revision: Option<u64>,
    requested_revision: u64,
) -> bool {
    edited_chapter == Some(current_chapter) && pushed_revision.unwrap_or(0) == requested_revision
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
        "manual": block.manual,
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
    fn a_manual_edit_must_come_from_the_chapter_the_host_pushed() {
        // 正常路径：窗口推的是第 3 章、版本 7，页面就带着这两个值回来。
        assert!(manual_edit_belongs_to_current_chapter(
            Some(3),
            3,
            Some(7),
            7
        ));
        // 另一章的 URL：即使版本号凑巧相同也不能写进当前章。
        assert!(!manual_edit_belongs_to_current_chapter(
            Some(2),
            3,
            Some(7),
            7
        ));
        assert!(!manual_edit_belongs_to_current_chapter(
            Some(4),
            3,
            Some(7),
            7
        ));
        // 不属于本书的 URL 解析不出章号。
        assert!(!manual_edit_belongs_to_current_chapter(None, 3, Some(7), 7));
        // 上一版载荷的陈旧请求：页面还没换成当前的译文。
        assert!(!manual_edit_belongs_to_current_chapter(
            Some(3),
            3,
            Some(7),
            6
        ));
        // 没有版本记录时宿主推送 0，页面回传 0 必须被接受，
        // 否则该章的手工译文永远写不进去。
        assert!(manual_edit_belongs_to_current_chapter(Some(3), 3, None, 0));
        assert!(!manual_edit_belongs_to_current_chapter(Some(3), 3, None, 1));
        // 有版本记录时伪造成 0 同样被拒。
        assert!(!manual_edit_belongs_to_current_chapter(
            Some(3),
            3,
            Some(7),
            0
        ));
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

    #[test]
    fn manual_translation_ipc_bounds_the_edit_before_the_host_reads_the_database() {
        let own_uri = "epubreader://book/Text/one.xhtml".parse().unwrap();
        let accepted = reader_ipc_event(
            &own_uri,
            r#"{"type":"manual_translation","action":"update","revision":3,"request_id":9,"key":"block-1","segments":[{"source":"One","translated":"一个"}]}"#,
        );
        let Some(ReaderWebEvent::ManualTranslation { url, request }) = accepted else {
            panic!("a bounded manual translation edit must be accepted");
        };
        assert_eq!(url, "epubreader://book/Text/one.xhtml");
        assert_eq!(request.action, ManualTranslationAction::Update);
        assert_eq!(request.revision, 3);
        assert_eq!(request.request_id, 9);
        assert_eq!(request.key, "block-1");
        assert_eq!(request.segments.len(), 1);
        assert_eq!(request.segments[0].source, "One");
        assert_eq!(request.segments[0].translated, "一个");

        // 恢复只表达“删掉手工译文”，所以它不携带任何文本。
        assert!(matches!(
            reader_ipc_event(
                &own_uri,
                r#"{"type":"manual_translation","action":"restore","revision":3,"request_id":10,"key":"block-1","segments":[]}"#,
            ),
            Some(ReaderWebEvent::ManualTranslation { .. })
        ));

        for forged in [
            // 没有文本块标识，宿主无法定位已存的机器译文行
            r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"","segments":[{"source":"One","translated":"一个"}]}"#,
            // 改译文必须带文本，恢复必须不带
            r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"block-1","segments":[]}"#,
            r#"{"type":"manual_translation","action":"restore","revision":3,"request_id":1,"key":"block-1","segments":[{"source":"One","translated":"一个"}]}"#,
            // 空译文与空原文都不是阅读窗口能产生的编辑
            r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"block-1","segments":[{"source":"One","translated":"   "}]}"#,
            r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"block-1","segments":[{"source":"","translated":"一个"}]}"#,
            // 请求序号 0 永远不是一个真实的请求
            r#"{"type":"manual_translation","action":"restore","revision":3,"request_id":0,"key":"block-1","segments":[]}"#,
            // 未定义的动作
            r#"{"type":"manual_translation","action":"delete","revision":3,"request_id":1,"key":"block-1","segments":[]}"#,
        ] {
            assert!(reader_ipc_event(&own_uri, forged).is_none(), "{forged}");
        }

        // 超长译文与超长标识在同一条流水线上被拒绝，而不是写进数据库。
        let oversized_segment = format!(
            r#"{{"type":"manual_translation","action":"update","revision":1,"request_id":1,"key":"block-1","segments":[{{"source":"One","translated":"{}"}}]}}"#,
            "字".repeat(MAX_MANUAL_TRANSLATION_SEGMENT_BYTES)
        );
        assert!(reader_ipc_event(&own_uri, &oversized_segment).is_none());
        let oversized_key = format!(
            r#"{{"type":"manual_translation","action":"restore","revision":1,"request_id":1,"key":"{}","segments":[]}}"#,
            "k".repeat(MAX_MANUAL_TRANSLATION_KEY_BYTES + 1)
        );
        assert!(reader_ipc_event(&own_uri, &oversized_key).is_none());
        let too_many_segments = format!(
            r#"{{"type":"manual_translation","action":"update","revision":1,"request_id":1,"key":"block-1","segments":[{}]}}"#,
            vec![r#"{"source":"One","translated":"一"}"#; MAX_MANUAL_TRANSLATION_SEGMENTS + 1]
                .join(",")
        );
        assert!(reader_ipc_event(&own_uri, &too_many_segments).is_none());
    }
}
