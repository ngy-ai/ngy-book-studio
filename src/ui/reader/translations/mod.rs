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
use ngy_book_studio::ai::translation::TranslationSegment;
use ngy_book_studio::services::{BackgroundJobStatus, TranslatedBlock, TranslationDisplayMode};

/// Task kind of a whole-book translation job, as persisted in `index_jobs`.
mod app;

mod helpers;

mod load;

mod manual;

mod model;

mod retranslate;

mod state;

pub(crate) use helpers::*;

pub(crate) use model::*;

pub(crate) use state::*;

#[cfg(test)]
mod tests;
