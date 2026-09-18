//! Progress, timing and fingerprint helpers for translations.

use super::*;

/// Highest block count reported by the book's unfinished translation task.
/// `None` means no translation task is queued, running, or paused, which is
/// also the signal that the last committed blocks are worth one final reload.
pub(crate) async fn active_translation_progress(
    services: &AppServices,
    book_id: String,
) -> Option<usize> {
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
pub(crate) fn translation_refresh_due(
    observed: &mut Option<usize>,
    current: Option<usize>,
) -> bool {
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
pub(crate) fn manual_edit_belongs_to_current_chapter(
    edited_chapter: Option<usize>,
    current_chapter: usize,
    pushed_revision: Option<u64>,
    requested_revision: u64,
) -> bool {
    edited_chapter == Some(current_chapter) && pushed_revision.unwrap_or(0) == requested_revision
}

pub(crate) fn translation_display_mode_label(mode: TranslationDisplayMode) -> &'static str {
    match mode {
        TranslationDisplayMode::Bilingual => "bilingual",
        TranslationDisplayMode::OriginalOnly => "original-only",
        TranslationDisplayMode::TranslationOnly => "translation-only",
    }
}

/// Fingerprint of one chapter's rendered translation layer. The session string
/// is deliberately excluded: it changes on every load, while the fingerprint
/// must only change when the chapter or its translations really did.
pub(crate) fn translation_fingerprint(
    unit_id: &str,
    display_mode: &str,
    blocks: &[serde_json::Value],
) -> u64 {
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

pub(crate) fn translated_block_json(block: &TranslatedBlock) -> serde_json::Value {
    serde_json::json!({
        "key": block.key,
        "source": block.source,
        "segments": block.segments,
        "manual": block.manual,
    })
}
