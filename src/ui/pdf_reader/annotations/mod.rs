//! PDF annotations: note actions, page ownership and the AI note flow.

//! Trusted host boundary for the PDF reader's single-table notes.
//!
//! Marks, human thoughts and AI thoughts use the very same `annotations` table
//! and the very same exclusive-mark rules as the EPUB reader. Only the anchor
//! contract differs: a PDF anchor addresses the bundled PDF.js text layer of
//! one page instead of a reflowable chapter body, so the host verifies book and
//! unit ownership, both revisions and the anchor's bounds while the immutable
//! original plus the pinned PDF.js build keep the offsets reproducible.
use super::*;
use crate::ui::ai_sidebar::{AiAnswerCompleted, AiExplanationFailed, AiExplanationSubmitted};
use anyhow::ensure;
use ngy_book_studio::annotations::{Annotation, AnnotationDraft, AnnotationKind, TextAnchor};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

// This bounds only the derived display. Stored Markdown retains its existing
// limits and is never replaced with HTML.

pub(crate) const MAX_NOTE_DISPLAY_HTML_BYTES: usize = 8 * 1024 * 1024;

/// One continuous-scroll reading window is far smaller than any page count the
/// importer accepts, so a declaration and a multi-page listing both stay tiny.
pub(crate) const MAX_ANNOTATION_PAGES: usize = 64;
pub(crate) const MAX_ANNOTATION_PAGE: u32 = 1_000_000;

mod action;

mod app;

mod html;

mod reference;

mod state;

pub(crate) use action::*;

pub(crate) use html::*;

pub(crate) use reference::*;

pub(crate) use state::*;

#[cfg(test)]
mod tests;
