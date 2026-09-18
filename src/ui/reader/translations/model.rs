//! The manual translation request and its limits.

use super::*;

/// Task kind of a whole-book translation job, as persisted in `index_jobs`.
pub(crate) const TRANSLATION_JOB_KIND: &str = "translation";
/// How often the reader asks whether the book's translation task advanced.
pub(crate) const TRANSLATION_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
/// Upper bounds for one manual translation request. The host re-reads the stored
/// row and revalidates the edit, so these only reject nonsense before the work
/// starts; the same limits are enforced again by the service.
pub(crate) const MAX_MANUAL_TRANSLATION_KEY_BYTES: usize = 256;
pub(crate) const MAX_MANUAL_TRANSLATION_SEGMENTS: usize = 512;
pub(crate) const MAX_MANUAL_TRANSLATION_SEGMENT_BYTES: usize = 8 * 1024;
pub(crate) const MAX_MANUAL_TRANSLATION_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManualTranslationAction {
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
pub(crate) struct ManualTranslationRequest {
    pub(crate) action: ManualTranslationAction,
    pub(crate) revision: u64,
    pub(crate) request_id: u64,
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) segments: Vec<TranslationSegment>,
}

impl ManualTranslationRequest {
    pub(crate) fn valid(&self) -> bool {
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
