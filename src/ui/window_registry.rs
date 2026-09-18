//! Registries for book, singleton and background-jobs windows.

use super::*;

/// Queue native window removal only after the current draw has replaced the
/// old WebView-bearing frame and GPUI has released the WebView entity. On
/// Windows, minimizing first makes GPUI detach its vsync request-frame callback;
/// otherwise that callback can race a later registry removal.
pub(crate) fn remove_window_after_current_frame(
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
pub(crate) struct BookWindowRegistry {
    windows: Vec<BookWindowHandle>,
}

impl gpui::Global for BookWindowRegistry {}

pub(crate) struct BookWindowHandle {
    book_id: String,
    window: AnyWindowHandle,
    close: Box<dyn Fn(&mut Window, &mut App)>,
}

/// Registers `window` as belonging to `book_id`.
///
/// `close` tears the window down without the barriers a user-initiated close
/// uses: the book is already gone, so nothing is left to save and a failed
/// write must never keep the window open.
pub(crate) fn register_book_window(
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
pub(crate) fn close_book_windows(book_id: &str, cx: &mut App) {
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

/// Windows that must stay unique within their scope.
///
/// A second reader, editor or notes window for the same book would own a second
/// progress writer, a second unsaved draft or a second independent query, so a
/// repeat request activates the live window instead of opening a duplicate.
#[derive(Default)]
pub(crate) struct SingletonWindowRegistry {
    /// Open requests that passed the duplicate check but have not created their
    /// window yet. Every book open builds its document asynchronously first.
    pending: HashSet<String>,
    windows: BTreeMap<String, AnyWindowHandle>,
    /// Live reader entities keyed by their reader scope, so a repeat open can
    /// activate the window and navigate it instead of opening a duplicate.
    pub(crate) readers: BTreeMap<String, WeakEntity<ReaderApp>>,
    pdf_readers: BTreeMap<String, WeakEntity<PdfReaderApp>>,
    /// The single background task window. A repeat request retargets it at the
    /// new library scope instead of opening a second window, because every
    /// window would only drive the same shared job queue.
    background_jobs: Option<WeakEntity<BackgroundJobsWindow>>,
}

impl gpui::Global for SingletonWindowRegistry {}

pub(crate) enum SingletonWindowReservation {
    /// A live window already owns this key; activate it instead of opening one.
    Activate(AnyWindowHandle),
    /// Reserved for the caller, which must complete or release it exactly once.
    Reserved,
    /// Another open for this key is still in flight.
    InFlight,
}

pub(crate) fn singleton_window_key(kind: &str, scope: &str) -> String {
    format!("{kind}:{scope}")
}

pub(crate) fn reserve_singleton_window(key: &str, cx: &mut App) -> SingletonWindowReservation {
    if !cx.has_global::<SingletonWindowRegistry>() {
        cx.set_global(SingletonWindowRegistry::default());
    }
    if let Some(handle) = cx
        .global::<SingletonWindowRegistry>()
        .windows
        .get(key)
        .copied()
    {
        if cx.windows().contains(&handle) {
            return SingletonWindowReservation::Activate(handle);
        }
        cx.global_mut::<SingletonWindowRegistry>()
            .windows
            .remove(key);
    }
    let registry = cx.global_mut::<SingletonWindowRegistry>();
    if registry.pending.contains(key) {
        return SingletonWindowReservation::InFlight;
    }
    registry.pending.insert(key.to_string());
    SingletonWindowReservation::Reserved
}

pub(crate) fn complete_singleton_window(key: &str, window: AnyWindowHandle, cx: &mut App) {
    if !cx.has_global::<SingletonWindowRegistry>() {
        return;
    }
    let registry = cx.global_mut::<SingletonWindowRegistry>();
    registry.pending.remove(key);
    registry.windows.insert(key.to_string(), window);
}

/// Give up a reservation whose window never appeared, so a later request is not
/// blocked forever by a failed or cancelled open.
pub(crate) fn release_singleton_window(key: &str, cx: &mut App) {
    if cx.has_global::<SingletonWindowRegistry>() {
        cx.global_mut::<SingletonWindowRegistry>()
            .pending
            .remove(key);
    }
}

pub(crate) fn activate_singleton_window(handle: AnyWindowHandle, cx: &mut App) {
    let _ = handle.update(cx, |_, window, _| window.activate_window());
}

pub(crate) fn register_singleton_reader(key: &str, reader: WeakEntity<ReaderApp>, cx: &mut App) {
    if !cx.has_global::<SingletonWindowRegistry>() {
        return;
    }
    cx.global_mut::<SingletonWindowRegistry>()
        .readers
        .insert(key.to_string(), reader);
}

pub(crate) fn register_singleton_pdf_reader(
    key: &str,
    reader: WeakEntity<PdfReaderApp>,
    cx: &mut App,
) {
    if !cx.has_global::<SingletonWindowRegistry>() {
        return;
    }
    cx.global_mut::<SingletonWindowRegistry>()
        .pdf_readers
        .insert(key.to_string(), reader);
}

pub(crate) fn existing_singleton_reader(key: &str, cx: &mut App) -> Option<Entity<ReaderApp>> {
    cx.global::<SingletonWindowRegistry>()
        .readers
        .get(key)
        .and_then(WeakEntity::upgrade)
}

pub(crate) fn existing_singleton_pdf_reader(
    key: &str,
    cx: &mut App,
) -> Option<Entity<PdfReaderApp>> {
    cx.global::<SingletonWindowRegistry>()
        .pdf_readers
        .get(key)
        .and_then(WeakEntity::upgrade)
}

/// The single background task window's registry key. The window is app-global,
/// so it is not keyed by book or group.
pub(crate) fn background_jobs_window_key() -> String {
    singleton_window_key("background-jobs", "app")
}

/// The live background task window, if any; prunes a dead registration so a
/// later open is not blocked by a closed window.
pub(crate) fn existing_background_jobs_window(
    cx: &mut App,
) -> Option<Entity<BackgroundJobsWindow>> {
    if !cx.has_global::<SingletonWindowRegistry>() {
        return None;
    }
    let registry = cx.global_mut::<SingletonWindowRegistry>();
    let upgraded = registry
        .background_jobs
        .as_ref()
        .and_then(WeakEntity::upgrade);
    if upgraded.is_none() {
        registry.background_jobs = None;
    }
    upgraded
}

/// Remembers the background task window entity; the registration is weak so a
/// closed window naturally frees its single slot.
pub(crate) fn register_background_jobs_window(
    view: WeakEntity<BackgroundJobsWindow>,
    cx: &mut App,
) {
    if !cx.has_global::<SingletonWindowRegistry>() {
        cx.set_global(SingletonWindowRegistry::default());
    }
    cx.global_mut::<SingletonWindowRegistry>().background_jobs = Some(view);
}
