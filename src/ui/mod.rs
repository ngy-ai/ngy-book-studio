mod ai_controller;
mod ai_settings;
mod ai_sidebar;
mod assets;
mod background_jobs;
mod data_dir_setup;
mod editor;
mod learning;
mod library;
mod notes;
mod page_image;
mod pdf_reader;
mod read_aloud;
mod reader;

#[cfg(test)]
mod application_exit_test;

#[cfg(test)]
mod test_support;

#[cfg(test)]
pub(crate) use test_support::init_ui_test;

pub(crate) use assets::AppAssets;
pub(crate) use data_dir_setup::{DataDirSetupWindow, open_data_dir_setup_window};
pub(crate) use library::{EpubReaderApp, wrap_root};

use ai_controller::AiSidebarController;
use ai_settings::{AiSettingsSection, open_ai_settings_window};
use ai_sidebar::{
    AI_SIDEBAR_COLLAPSED_WIDTH, AI_SIDEBAR_MAX_WIDTH, AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_WIDTH,
    AiBookOption, AiReferenceHint, AiSidebar, AiSidebarEvent, AiSidebarScope, AiSourceLink,
};
use background_jobs::{BackgroundJobBook, BackgroundJobsWindow, open_background_jobs_window};
use editor::{
    EditorApp, EditorWebState, build_editor_webview, editor_chapters_from_document,
    suggested_epub_filename,
};
use learning::open_learning_window;
use notes::open_notes_window;
use page_image::open_page_image_window;
use pdf_reader::{PdfReaderApp, PdfReaderInit, PdfReaderPage, build_pdf_reader_webview};
use reader::{ReaderApp, ReaderWebViewBuildGate, build_reader_webview};

use std::{
    borrow::Cow,
    cell::Cell,
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result};
use gpui_kit::component::{
    Disableable as _, Icon, IconName, InteractiveElementExt as _, Root, Selectable as _,
    Sizable as _, StyledExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::{Editor, EditorState, Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    popover::Popover,
    scroll::ScrollableElement as _,
    tab::{Tab, TabBar},
};
use gpui_kit::{
    Anchor, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Image, ImageFormat,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions, div, img,
    prelude::FluentBuilder as _, px, rgb, rgba, size, white,
};
use gpui_wry::WebView;
use native_dialog::DialogBuilder;
use ngy_book_studio::{
    ai::chat::ChatWindowKind,
    document::{BookDocument, BookFormat, BookSource as CanonicalBookSource, SourceLocator},
    export::ExportFormat,
    library::{
        BookGroup, BookRecord, CoverDraft, ImportOutcome, LibraryStore, SearchHit,
        ShareBookOutcome, ShareExportSummary, ShareImportOutcome,
    },
    preview::bundled_pdfjs_asset,
    reader::OpenedBook,
    services::AppServices,
    share,
};
use serde::Deserialize;
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};

/// 当前皮肤。窗口在渲染或推送 WebView 时读它；改名成 `current_appearance` 是为了
/// 不和界面代码里的同名字段/局部变量撞名。
pub(crate) use ngy_book_studio::theme::appearance as current_appearance;
/// 皮肤与调色板是本模块所有窗口共用的取色入口：颜色只在 `ngy_book_studio::theme`
/// 里定义一次，各窗口用 `use super::*;` 拿到 [`palette`] 后按语义名取色。任何一个
/// 窗口都不应该自己再写死颜色常量，否则它会在切换皮肤时留在旧配色上。
pub(crate) use ngy_book_studio::theme::{Appearance, Palette, palette, set_appearance};

mod lifecycle;

mod pdf_window;

mod source;

mod status_bar;

mod window_registry;

pub(crate) use lifecycle::*;

pub(crate) use pdf_window::*;

pub(crate) use read_aloud::*;

pub(crate) use source::*;

pub(crate) use status_bar::*;

pub(crate) use window_registry::*;

#[cfg(test)]
mod book_window_tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use std::{cell::RefCell, rc::Rc};

    struct TrackedWindow;

    impl Render for TrackedWindow {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    fn closing_a_book_closes_only_its_own_windows(cx: &mut TestAppContext) {
        let closed: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let first: AnyWindowHandle = cx.add_window(|_, _| TrackedWindow).into();
        let second: AnyWindowHandle = cx.add_window(|_, _| TrackedWindow).into();
        let register = |cx: &mut TestAppContext, window: AnyWindowHandle, book_id: &str| {
            let closed = Rc::clone(&closed);
            let book_id = book_id.to_string();
            cx.update_window(window, |_, window, cx| {
                register_book_window(
                    book_id.clone(),
                    window,
                    move |window, _| {
                        closed.borrow_mut().push(book_id.clone());
                        window.remove_window();
                    },
                    cx,
                );
            })
            .expect("测试窗口仍然存在");
        };
        register(cx, first, "book-a");
        register(cx, second, "book-b");

        cx.update(|cx| close_book_windows("book-a", cx));
        assert_eq!(*closed.borrow(), vec!["book-a".to_string()]);
        let open = cx.windows();
        assert!(!open.contains(&first));
        assert!(open.contains(&second));

        // A closed book leaves no registration behind, and the other book's
        // window is untouched until its own book goes away.
        cx.update(|cx| close_book_windows("book-a", cx));
        assert_eq!(*closed.borrow(), vec!["book-a".to_string()]);
        cx.update(|cx| close_book_windows("book-b", cx));
        assert_eq!(
            *closed.borrow(),
            vec!["book-a".to_string(), "book-b".to_string()]
        );
        assert!(!cx.windows().contains(&second));
    }
}

#[cfg(test)]
mod singleton_window_tests {
    use super::*;
    use gpui_kit::TestAppContext;

    struct TrackedWindow;

    impl Render for TrackedWindow {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    fn a_reserved_window_is_reused_until_it_closes(cx: &mut TestAppContext) {
        let window: AnyWindowHandle = cx.add_window(|_, _| TrackedWindow).into();
        let key = singleton_window_key("reader", "book-a");
        // The asynchronous document load must not let a second request through.
        cx.update(|cx| {
            assert!(matches!(
                reserve_singleton_window(&key, cx),
                SingletonWindowReservation::Reserved
            ));
            assert!(matches!(
                reserve_singleton_window(&key, cx),
                SingletonWindowReservation::InFlight
            ));
            complete_singleton_window(&key, window, cx);
        });
        let reservation = cx.update(|cx| reserve_singleton_window(&key, cx));
        match reservation {
            SingletonWindowReservation::Activate(handle) => assert!(handle == window),
            _ => panic!("已经打开的窗口应当被激活，而不是再开一个"),
        }
    }

    #[gpui_kit::test]
    fn closed_and_failed_windows_free_their_scope(cx: &mut TestAppContext) {
        let window: AnyWindowHandle = cx.add_window(|_, _| TrackedWindow).into();
        let key = singleton_window_key("editor", "book-a");
        cx.update(|cx| complete_singleton_window(&key, window, cx));
        cx.update_window(window, |_, window, _| window.remove_window())
            .expect("测试窗口仍然存在");
        assert!(!cx.windows().contains(&window));

        cx.update(|cx| {
            assert!(matches!(
                reserve_singleton_window(&key, cx),
                SingletonWindowReservation::Reserved
            ));
            // A failed open hands the reservation back instead of blocking the
            // book forever.
            release_singleton_window(&key, cx);
            assert!(matches!(
                reserve_singleton_window(&key, cx),
                SingletonWindowReservation::Reserved
            ));
        });
    }
}
