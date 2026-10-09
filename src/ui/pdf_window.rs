//! The PDF reader window registry, display modes and the parent handle.

use super::*;

/// Every live PDF reading window, including the non-singleton Office preview.
///
/// A global reading preference must reach documents that are already open, and
/// no other registry enumerates them: the singleton table only keeps the single
/// reading window of a book, and the book window table only remembers how to
/// close them.
#[derive(Default)]
pub(crate) struct PdfReaderWindowRegistry {
    readers: Vec<WeakEntity<PdfReaderApp>>,
}

impl gpui_kit::Global for PdfReaderWindowRegistry {}

/// Remembers a PDF reader window and drops registrations whose window is gone.
pub(crate) fn register_pdf_reader_window(reader: WeakEntity<PdfReaderApp>, cx: &mut App) {
    if !cx.has_global::<PdfReaderWindowRegistry>() {
        cx.set_global(PdfReaderWindowRegistry::default());
    }
    let registry = cx.global_mut::<PdfReaderWindowRegistry>();
    registry
        .readers
        .retain(|candidate| candidate.upgrade().is_some());
    registry.readers.push(reader);
}

/// Applies the compact reading preference to every open PDF reader.
///
/// The update is a single attribute write per viewer, so it neither queries the
/// database nor touches the shared library projection; a window that disappears
/// while iterating is simply pruned.
pub(crate) fn apply_pdf_compact_reading(compact_reading: bool, cx: &mut App) {
    if !cx.has_global::<PdfReaderWindowRegistry>() {
        return;
    }
    let registered = std::mem::take(&mut cx.global_mut::<PdfReaderWindowRegistry>().readers);
    let mut remaining = Vec::with_capacity(registered.len());
    for weak in registered {
        let Some(reader) = weak.upgrade() else {
            continue;
        };
        reader.update(cx, |reader, cx| {
            reader.apply_pdf_compact_reading(compact_reading, cx)
        });
        remaining.push(weak);
    }
    cx.global_mut::<PdfReaderWindowRegistry>().readers = remaining;
}

/// Re-applies the translation display preference to every open EPUB reader so a
/// settings change takes effect on already-loaded chapters without reopening.
pub(crate) fn apply_translation_display_mode(cx: &mut App) {
    if !cx.has_global::<SingletonWindowRegistry>() {
        return;
    }
    let readers = cx
        .global::<SingletonWindowRegistry>()
        .readers
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for weak in readers {
        if let Some(reader) = weak.upgrade() {
            reader.update(cx, |reader, cx| {
                reader.apply_translation_display_mode(cx);
            });
        }
    }
}

pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) error: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct ParentWindowHandle(Win32WindowHandle);

impl ParentWindowHandle {
    pub(crate) fn capture(window: &Window) -> Result<Self> {
        match HasWindowHandle::window_handle(window)
            .context("无法获取应用窗口句柄")?
            .as_raw()
        {
            RawWindowHandle::Win32(handle) => Ok(Self(handle)),
            _ => anyhow::bail!("GPUI 未返回 Win32 窗口句柄"),
        }
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn is_minimized(&self) -> bool {
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
