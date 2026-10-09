use super::*;
use super::{
    ai_controller::AiSidebarController,
    ai_sidebar::{
        AiCitationNavigationTarget, AiCitationTextFocus, AiQuestionRequest, AiSidebarEvent,
        AiSourceLink, citation_dom_navigation_script,
    },
};
use gpui_kit::{Pixels, PromptButton, PromptLevel};
use ngy_book_studio::{
    ai::chat::ChatWindowKind,
    document::{
        AssetRef, AssetRole, Block, BookDocument, BookFormat, BookSource, ContentUnitKind, Inline,
        Revision, TocNode, deterministic_id,
    },
    editing::{DocumentEditor, NewContentUnit},
    library::{MediaDraft, MediaKind},
    markup::{ParsedSource, parse_source_for_unit, serialize_source, serialize_xhtml},
    media::{MediaBackend, MediaMetadata, MediaResponse, MediaService},
    services::{AppServices, LibraryMutation},
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

mod navigation;
use navigation::{editor_toc_rows, toc_indent_destination, toc_outdent_destination};

mod action;

mod app;

mod chapter;

mod close;

mod cover;

mod document;

mod hints;

mod ipc;

mod media;

mod media_edit;

mod model;

mod protocol;

mod render;

mod render_app;

mod search;

mod shell;

mod view;

mod web_state;

mod webview;

mod webview_build;

mod write;

pub(crate) use document::*;

pub(crate) use hints::*;

pub(crate) use ipc::*;

pub(crate) use media_edit::*;

pub(crate) use model::*;

pub(crate) use protocol::*;

pub(crate) use render::*;

pub(crate) use search::*;

pub(crate) use shell::*;

pub(crate) use web_state::*;

// The editor surface the rest of the UI uses, unchanged by the split.
pub use app::EditorApp;
pub(super) use ipc::{EditorIpcUpdate, EditorWebEvent};
pub use model::EditorChapter;
pub(super) use model::editor_chapters_from_document;
pub(super) use search::suggested_epub_filename;
pub(super) use shell::editor_appearance_script;
pub use web_state::EditorWebState;
pub(super) use webview_build::build_editor_webview;

#[cfg(test)]
mod close_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod xhtml_test;
const EDITOR_SHELL_HOST: &str = "shell";
const EDITOR_CONTENT_HOST: &str = "content";
#[cfg(target_os = "windows")]
const EDITOR_SHELL_ORIGIN: &str = "http://epubeditor.shell";
#[cfg(not(target_os = "windows"))]
const EDITOR_SHELL_ORIGIN: &str = "epubeditor://shell";
const EDITOR_SHELL_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src http://epubeditor.content epubeditor://content data:; media-src http://epubeditor.content epubeditor://content data:; connect-src http://epubeditor.content epubeditor://content; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'";
const EDITOR_CONTENT_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; media-src 'self' data:; connect-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'";
const EDITOR_TRUSTED_SHELL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\" lang=\"zh-CN\"><head><meta charset=\"utf-8\"/><title>墨页富文本编辑器</title></head><body><main data-ngy-editor-shell=\"trusted\"></main></body></html>";
const EDITOR_ASSET_PATH_PREFIX: &str = ".ngy/assets/";
const MAX_EDITOR_ASSET_ID_BYTES: usize = 256;
const MAX_EDITOR_SELECTION_BYTES: usize = 32 * 1024;
const MAX_EDITOR_MEDIA_TITLE_CHARS: usize = 512;
const MAX_EDITOR_MEDIA_DESCRIPTION_CHARS: usize = 16 * 1024;
const MAX_EDITOR_ID_BYTES: usize = 4 * 1024;
const MAX_EDITOR_REQUEST_ID: u64 = (1_u64 << 53) - 1;
const EDITOR_LEFT_SIDEBAR_DEFAULT_WIDTH: f32 = 252.;
const EDITOR_LEFT_SIDEBAR_MIN_WIDTH: f32 = 200.;
const EDITOR_LEFT_SIDEBAR_MAX_WIDTH: f32 = 480.;
const EDITOR_CONTENT_MIN_WIDTH: f32 = 360.;
const EDITOR_RESIZE_HANDLE_WIDTH: f32 = 7.;

static NEXT_EDITOR_SESSION_ID: AtomicU64 = AtomicU64::new(1);
