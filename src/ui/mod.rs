mod ai_controller;
mod ai_settings;
mod ai_sidebar;
mod background_jobs;
mod editor;
mod learning;
mod library;
mod office_slides;
mod pdf_reader;
mod reader;

pub(crate) use library::{EpubReaderApp, wrap_root};

use ai_controller::AiSidebarController;
use ai_settings::open_ai_settings_window;
use ai_sidebar::{
    AI_SIDEBAR_COLLAPSED_WIDTH, AI_SIDEBAR_MAX_WIDTH, AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_WIDTH,
    AiBookOption, AiReferenceHint, AiSidebar, AiSidebarEvent, AiSidebarScope, AiSourceLink,
};
use background_jobs::{BackgroundJobBook, open_background_jobs_window};
use editor::{
    EditorApp, EditorWebState, build_editor_webview, editor_chapters_from_document,
    suggested_epub_filename,
};
use learning::open_learning_window;
use office_slides::open_office_slides_window;
use pdf_reader::{PdfReaderApp, PdfReaderInit, PdfReaderPage, build_pdf_reader_webview};
use reader::{ReaderApp, ReaderWebViewBuildGate, build_reader_webview};

use std::{
    borrow::Cow,
    cell::Cell,
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Image, ImageFormat,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, Timer,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions, div, img,
    prelude::FluentBuilder as _, px, rgb, rgba, size, white,
};
use gpui_component::{
    Disableable as _, Icon, IconName, InteractiveElementExt as _, Root, Sizable as _,
    StyledExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    tab::{Tab, TabBar},
    webview::WebView,
    wry::raw_window_handle::{
        HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
    },
};
use moye_epub_editor::{
    chat::ChatWindowKind,
    document::{BookDocument, BookFormat, BookSource as CanonicalBookSource, SourceLocator},
    export::ExportFormat,
    library::{BookGroup, BookRecord, CoverDraft, ImportOutcome, LibraryStore, SearchHit},
    preview::bundled_pdfjs_asset,
    reader::OpenedBook,
    services::AppServices,
};
use native_dialog::DialogBuilder;
use serde::Deserialize;

const PAPER: u32 = 0xf7f5f0;
const SURFACE: u32 = 0xfffefa;
const SIDEBAR: u32 = 0xf1eee8;
const INK: u32 = 0x292621;
const MUTED: u32 = 0x777067;
const BORDER: u32 = 0xe1ddd5;
const ACCENT: u32 = 0xb95f42;
const ACCENT_DARK: u32 = 0x9f4d34;
const ACCENT_SOFT: u32 = 0xf1ddd5;
const DANGER: u32 = 0xb33a2b;
const STATUS_BAR_HEIGHT: f32 = 30.;
const MAX_EDITOR_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_EDITOR_IPC_BYTES: usize = 20 * 1024 * 1024;
const EDITOR_PAGE_HISTORY: usize = 4;
const EDITOR_READY_TIMEOUT: Duration = Duration::from_secs(5);
const WEBVIEW_RELEASE_POLL: Duration = Duration::from_millis(16);
const WINDOW_MINIMIZE_POLL: Duration = Duration::from_millis(16);
const WINDOW_MINIMIZE_SETTLE: Duration = Duration::from_millis(32);

/// Resolves a source link only after binding any native coordinate back to the
/// current canonical unit and the document's actual source format. Callers
/// may still apply a narrower window-specific type check (for example, PDF
/// viewers accept only `PdfPage`).
fn current_canonical_source_unit_index(
    source: &AiSourceLink,
    document: &BookDocument,
) -> std::result::Result<usize, String> {
    let unit_index = source
        .current_unit_index(document)
        .ok_or_else(|| "引用对应的文档或内容版本已失效".to_string())?;
    if source.locator.is_none() {
        return Ok(unit_index);
    }
    let locator = source
        .validated_locator()
        .ok_or_else(|| "引用定位无效".to_string())?;
    if let Some(native) = locator.source.as_ref() {
        let unit = &document.units[unit_index];
        if unit.source_locator.as_ref() != Some(native) {
            return Err("引用的原始来源位置与当前内容单元不匹配".to_string());
        }
        if !source_locator_matches_document(&document.source, native) {
            return Err("引用的原始来源类型与当前图书格式不匹配".to_string());
        }
    }
    Ok(unit_index)
}

fn source_locator_matches_document(source: &CanonicalBookSource, locator: &SourceLocator) -> bool {
    matches!(
        (source, locator),
        (CanonicalBookSource::Created, SourceLocator::Created)
            | (CanonicalBookSource::Imported { .. }, SourceLocator::Created)
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Epub,
                    ..
                },
                SourceLocator::Epub { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Pdf,
                    ..
                },
                SourceLocator::PdfPage { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Doc | BookFormat::Docx,
                    ..
                },
                SourceLocator::OfficeSection { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Pptx,
                    ..
                },
                SourceLocator::Slide { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Xlsx,
                    ..
                },
                SourceLocator::Worksheet { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Mobi | BookFormat::Azw | BookFormat::Azw3,
                    ..
                },
                SourceLocator::KindleSection { .. },
            )
    )
}

fn image_format_from_mime(mime: &str) -> Option<ImageFormat> {
    match mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/png" => Some(ImageFormat::Png),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

fn render_status_bar(
    status_icon: IconName,
    status_text: String,
    status_color: u32,
    summary: String,
    context: String,
) -> gpui::AnyElement {
    div()
        .h_flex()
        .h(px(STATUS_BAR_HEIGHT))
        .flex_none()
        .items_center()
        .justify_between()
        .gap_4()
        .px_5()
        .border_t_1()
        .border_color(rgb(BORDER))
        .bg(rgb(SURFACE))
        .text_xs()
        .text_color(rgb(MUTED))
        .child(
            div()
                .h_flex()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .gap_3()
                .child(
                    div()
                        .h_flex()
                        .flex_none()
                        .gap_1p5()
                        .text_color(rgb(status_color))
                        .child(Icon::new(status_icon).small())
                        .child(status_text),
                )
                .child(div().w(px(1.)).h(px(12.)).flex_none().bg(rgb(BORDER)))
                .child(div().min_w(px(0.)).truncate().child(summary)),
        )
        .child(
            div()
                .h_flex()
                .max_w(px(460.))
                .min_w(px(0.))
                .flex_none()
                .gap_3()
                .child(div().min_w(px(0.)).truncate().child(context))
                .child(div().w(px(1.)).h(px(12.)).flex_none().bg(rgb(BORDER)))
                .child(
                    div()
                        .flex_none()
                        .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                ),
        )
        .into_any_element()
}

/// Queue native window removal only after the current draw has replaced the
/// old WebView-bearing frame and GPUI has released the WebView entity. On
/// Windows, minimizing first makes GPUI detach its vsync request-frame callback;
/// otherwise that callback can race a later registry removal.
fn remove_window_after_current_frame(
    window: &Window,
    cx: &mut App,
    webview: Option<WeakEntity<WebView>>,
) {
    let native_window = ParentWindowHandle::capture(window).ok();
    window.defer(cx, move |window, cx| {
        window
            .spawn(cx, async move |cx| {
                if let Some(webview) = webview {
                    while webview.upgrade().is_some() {
                        Timer::after(WEBVIEW_RELEASE_POLL).await;
                        if cx.update(|window, _| window.refresh()).is_err() {
                            return;
                        }
                    }
                    // A zero strong-handle count enters GPUI's dropped queue;
                    // one more update flushes the actual WebView/Wry value.
                    if cx.update(|_, _| ()).is_err() {
                        return;
                    }
                }
                if cx.update(|window, _| window.minimize_window()).is_err() {
                    return;
                }
                #[cfg(target_os = "windows")]
                {
                    if let Some(native_window) = native_window {
                        while !native_window.is_minimized() {
                            Timer::after(WINDOW_MINIMIZE_POLL).await;
                        }
                    } else {
                        // Capture can already have failed while constructing a
                        // Reader/PDF view. No child WebView exists in that
                        // path, so do not strand an unclosable GPUI window;
                        // allow one frame for the minimize request to settle.
                        tracing::warn!("cannot verify window minimization during close");
                        Timer::after(WINDOW_MINIMIZE_POLL).await;
                    }
                }
                #[cfg(not(target_os = "windows"))]
                Timer::after(WINDOW_MINIMIZE_POLL).await;

                Timer::after(WINDOW_MINIMIZE_SETTLE).await;
                let _ = cx.update(|window, _| window.remove_window());
            })
            .detach();
    });
}

/// Windows that were opened for one specific book.
///
/// Reader, PDF/Office preview and editor windows keep their own library copy
/// and their own child WebView, so removing a book has to take them down
/// explicitly: left open they would keep persisting progress or saving edits
/// for a document that no longer exists.
#[derive(Default)]
struct BookWindowRegistry {
    windows: Vec<BookWindowHandle>,
}

impl gpui::Global for BookWindowRegistry {}

struct BookWindowHandle {
    book_id: String,
    window: AnyWindowHandle,
    close: Box<dyn Fn(&mut Window, &mut App)>,
}

/// Registers `window` as belonging to `book_id`.
///
/// `close` tears the window down without the barriers a user-initiated close
/// uses: the book is already gone, so nothing is left to save and a failed
/// write must never keep the window open.
fn register_book_window(
    book_id: String,
    window: &Window,
    close: impl Fn(&mut Window, &mut App) + 'static,
    cx: &mut App,
) {
    if !cx.has_global::<BookWindowRegistry>() {
        cx.set_global(BookWindowRegistry::default());
    }
    cx.global_mut::<BookWindowRegistry>()
        .windows
        .push(BookWindowHandle {
            book_id,
            window: window.window_handle(),
            close: Box::new(close),
        });
}

/// Closes every registered window of `book_id`, forgetting registrations whose
/// window has already disappeared.
fn close_book_windows(book_id: &str, cx: &mut App) {
    if !cx.has_global::<BookWindowRegistry>() {
        return;
    }
    let open = cx.windows();
    let registered = std::mem::take(&mut cx.global_mut::<BookWindowRegistry>().windows);
    let mut remaining = Vec::with_capacity(registered.len());
    let mut closing = Vec::new();
    for entry in registered {
        if !open.contains(&entry.window) {
            continue;
        }
        if entry.book_id == book_id {
            closing.push(entry);
        } else {
            remaining.push(entry);
        }
    }
    cx.global_mut::<BookWindowRegistry>().windows = remaining;
    for entry in closing {
        let _ = entry
            .window
            .update(cx, |_, window, cx| (entry.close)(window, cx));
    }
}

struct Notice {
    text: String,
    error: bool,
}

#[derive(Clone, Copy)]
struct ParentWindowHandle(Win32WindowHandle);

impl ParentWindowHandle {
    fn capture(window: &Window) -> Result<Self> {
        match HasWindowHandle::window_handle(window)
            .context("无法获取应用窗口句柄")?
            .as_raw()
        {
            RawWindowHandle::Win32(handle) => Ok(Self(handle)),
            _ => anyhow::bail!("GPUI 未返回 Win32 窗口句柄"),
        }
    }

    #[cfg(target_os = "windows")]
    fn is_minimized(&self) -> bool {
        use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsIconic};

        let hwnd = HWND(self.0.hwnd.get() as *mut std::ffi::c_void);
        unsafe { IsIconic(hwnd).as_bool() }
    }
}

impl HasWindowHandle for ParentWindowHandle {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        // SAFETY: each child window's close veto keeps this HWND alive while
        // the detached foreground task builds its WebView.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(self.0)) })
    }
}

#[cfg(test)]
mod book_window_tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{cell::RefCell, rc::Rc};

    struct TrackedWindow;

    impl Render for TrackedWindow {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
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
