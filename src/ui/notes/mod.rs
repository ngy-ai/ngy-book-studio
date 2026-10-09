use super::ai_sidebar::markdown::assistant_display_markdown;
use super::*;

use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::{ClipboardItem, Pixels, ScrollHandle};
use ngy_book_studio::annotations::{AnnotationKind, AnnotationOverview};
use std::collections::HashMap;

mod app;

mod filter;

mod labels;

mod launch;

mod listing;

mod markdown;

mod model;

mod open;

mod render;

mod render_card;

mod render_header;

mod render_list;

mod render_note;

pub(crate) use filter::*;

pub(crate) use labels::*;

pub(crate) use launch::*;

pub(crate) use listing::*;

pub(crate) use markdown::*;

pub(crate) use model::*;

pub(crate) use render_note::*;

#[cfg(test)]
mod tests;
pub(crate) const NOTES_PER_PAGE: usize = 30;

pub(crate) struct NotesWindow {
    pub(crate) services: Arc<AppServices>,
    pub(crate) library_view: Entity<EpubReaderApp>,
    pub(crate) book: Option<(String, String)>,
    pub(crate) listing: NoteListing,
    pub(crate) search: Entity<InputState>,
    pub(crate) _search_subscription: Subscription,
    pub(crate) scroll: ScrollHandle,
    pub(crate) generation: u64,
    pub(crate) loading: bool,
    pub(crate) loaded: bool,
    pub(crate) error: Option<String>,
    pub(crate) copied_note: Option<String>,
    pub(crate) opening_note: Option<String>,
}
