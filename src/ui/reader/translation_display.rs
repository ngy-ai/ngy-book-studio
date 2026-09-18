//! The translation display modes offered in the reader.

use super::*;

/// Reading-window switch order: the same three choices the system configuration
/// offers, with the translation-only mode last because it is the default.
pub(crate) const TRANSLATION_DISPLAY_MODES: [TranslationDisplayMode; 3] = [
    TranslationDisplayMode::Bilingual,
    TranslationDisplayMode::OriginalOnly,
    TranslationDisplayMode::TranslationOnly,
];

pub(crate) fn translation_display_mode(index: usize) -> TranslationDisplayMode {
    TRANSLATION_DISPLAY_MODES
        .get(index)
        .copied()
        .unwrap_or_default()
}

pub(crate) fn translation_display_mode_name(mode: TranslationDisplayMode) -> &'static str {
    match mode {
        TranslationDisplayMode::Bilingual => "双语",
        TranslationDisplayMode::OriginalOnly => "原文",
        TranslationDisplayMode::TranslationOnly => "译文",
    }
}
