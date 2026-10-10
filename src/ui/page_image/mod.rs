//! Shared page-image reading window for persisted visual page sets.
//!
//! Two sources reuse this window: Microsoft Office enhanced previews
//! (`OfficeEnhanced`) and imported DjVu scans (`Djvu`). They differ only in
//! copy, AI-reference eligibility and whether a page text panel is offered;
//! page loading, ordering, zoom, window registration, close handling and the
//! AI sidebar are identical.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::{AnyElement, ScrollWheelEvent, rems};

use super::reader::{ReadingProgressWrite, ReadingProgressWriteEvent, ReadingProgressWriter};
use super::*;
use ngy_book_studio::{
    document::{DocumentLocator, SourceLocator},
    services::{PublishedVisualPage, ReaderZoomSurface, VisualPageSourceKind},
};

/// Zoom steps are thousandths; `ZOOM_FIT` means "fit the window".
mod app;

mod launch;

mod layout;

mod model;

mod reference;

mod render;

mod speech;

mod text;

mod view;

mod webview_events;

pub(crate) use launch::*;

pub(crate) use layout::*;

pub(crate) use model::*;

pub(crate) use reference::*;

pub(crate) use text::*;

#[cfg(test)]
mod tests;
pub(crate) struct PageImageApp {
    pub(crate) book_id: String,
    pub(crate) book_title: String,
    pub(crate) kind: VisualPageSourceKind,
    pub(crate) pages: Vec<PageImage>,
    pub(crate) current: usize,
    /// `ZOOM_FIT` renders the whole page fitted to the window; any other value
    /// is an explicit pixel zoom in thousandths.
    pub(crate) zoom_milli: u32,
    /// Pending write of this book's page size, replaced on every new change.
    pub(crate) zoom_save_task: Option<Task<()>>,
    /// Page text loaded from the canonical units; present only for DjVu, where
    /// the hidden text layer is the page's searchable text.
    pub(crate) page_texts: Vec<Option<String>>,
    pub(crate) text_panel_open: bool,
    /// 朗读（系统语音）的会话状态。进程级资源：同一时刻只有一个窗口在朗读。
    pub(crate) read_aloud: ReadAloudHost,
    /// 正在朗读的段落索引（「本页文字」面板里的第 n 段），用于面板高亮。
    pub(crate) read_aloud_paragraph: Option<usize>,
    pub(crate) progress_writer: Option<ReadingProgressWriter>,
    pub(crate) progress_sync_task: Option<Task<()>>,
    pub(crate) services: Arc<AppServices>,
    pub(crate) library_view: Entity<EpubReaderApp>,
    pub(crate) ai_sidebar: Entity<AiSidebar>,
    pub(crate) ai_controller: AiSidebarController,
    pub(crate) _ai_subscription: Subscription,
    pub(crate) notice: Option<Notice>,
    pub(crate) closing: bool,
}
