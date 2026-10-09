//! Application window lifecycle: the exit handshake and its guard rails.

use super::*;

pub(crate) const STATUS_BAR_HEIGHT: f32 = 30.;
pub(crate) const MAX_EDITOR_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_EDITOR_IPC_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const EDITOR_PAGE_HISTORY: usize = 4;
pub(crate) const EDITOR_READY_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const WEBVIEW_RELEASE_POLL: Duration = Duration::from_millis(16);
pub(crate) const WINDOW_MINIMIZE_POLL: Duration = Duration::from_millis(16);
pub(crate) const WINDOW_MINIMIZE_SETTLE: Duration = Duration::from_millis(32);

pub(crate) type WindowCloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Default)]
pub(crate) struct ApplicationWindowLifecycle {
    handlers: Vec<(AnyWindowHandle, WindowCloseHandler)>,
    pub(crate) prompt_open: bool,
    pub(crate) exit: Option<ApplicationExit>,
    _closed_subscription: Option<Subscription>,
}

impl gpui_kit::Global for ApplicationWindowLifecycle {}

pub(crate) struct ApplicationExit {
    pub(crate) main_window: AnyWindowHandle,
    pub(crate) close_main: WindowCloseHandler,
    pub(crate) waiting_for: Option<AnyWindowHandle>,
    pub(crate) main_close_requested: bool,
    pub(crate) main_ready: bool,
    pub(crate) removal_scheduled: bool,
}

pub(crate) fn init_application_window_lifecycle(cx: &mut App) {
    if cx.has_global::<ApplicationWindowLifecycle>() {
        return;
    }
    let subscription = cx.on_window_closed(|cx, _window_id| {
        // The closing window can still be on the update stack. Advance only
        // after its callback and entity borrows have been released.
        cx.defer(advance_application_exit);
    });
    cx.set_global(ApplicationWindowLifecycle {
        _closed_subscription: Some(subscription),
        ..Default::default()
    });
}

/// Use the very same close barriers for the titlebar and application exit.
/// `false` may mean asynchronous saving/building, not a rejected close.
pub(crate) fn on_window_close(
    window: &mut Window,
    cx: &mut App,
    close: impl Fn(&mut Window, &mut App) -> bool + 'static,
) {
    init_application_window_lifecycle(cx);
    let handler: WindowCloseHandler = Rc::new(move |window, cx| {
        if close(window, cx) {
            remove_window_after_current_frame(window, cx, None);
        }
    });
    let handle = Window::window_handle(window);
    let lifecycle = cx.global_mut::<ApplicationWindowLifecycle>();
    lifecycle
        .handlers
        .retain(|(existing, _)| *existing != handle);
    lifecycle.handlers.push((handle, Rc::clone(&handler)));
    window.on_window_should_close(cx, move |window, cx| {
        handler(window, cx);
        false
    });
    // An accepted asynchronous open may finish while exit is already waiting.
    cx.defer(advance_application_exit);
}

pub(crate) fn request_application_exit(
    window: &mut Window,
    cx: &mut App,
    close_main: WindowCloseHandler,
) {
    init_application_window_lifecycle(cx);
    let lifecycle = cx.global_mut::<ApplicationWindowLifecycle>();
    if lifecycle.prompt_open || lifecycle.exit.is_some() {
        return;
    }
    lifecycle.prompt_open = true;
    let answer = window.prompt(
        gpui_kit::PromptLevel::Warning,
        "是否退出墨页？",
        Some("退出将关闭图书阅读、图书编辑、AI 配置等全部窗口。编辑窗口会继续询问是否保存修改。"),
        &[
            gpui_kit::PromptButton::ok("退出软件"),
            gpui_kit::PromptButton::cancel("继续运行"),
        ],
        cx,
    );
    window
        .spawn(cx, async move |cx| {
            let confirmed = matches!(answer.await, Ok(0));
            let _ = cx.update(|window, cx| {
                let lifecycle = cx.global_mut::<ApplicationWindowLifecycle>();
                lifecycle.prompt_open = false;
                if confirmed {
                    lifecycle.exit = Some(ApplicationExit {
                        main_window: Window::window_handle(window),
                        close_main,
                        waiting_for: None,
                        main_close_requested: false,
                        main_ready: false,
                        removal_scheduled: false,
                    });
                    cx.defer(advance_application_exit);
                }
            });
        })
        .detach();
}

/// A cancelled prompt or failed persistence must also cancel the enclosing
/// exit attempt. Closing that child later must not unexpectedly quit the app.
pub(crate) fn cancel_application_exit(cx: &mut App) {
    if cx.has_global::<ApplicationWindowLifecycle>() {
        cx.global_mut::<ApplicationWindowLifecycle>().exit = None;
    }
}

pub(crate) fn application_is_exiting(cx: &App) -> bool {
    cx.has_global::<ApplicationWindowLifecycle>()
        && cx.global::<ApplicationWindowLifecycle>().exit.is_some()
}

pub(crate) fn advance_application_exit(cx: &mut App) {
    let open = cx.windows();
    let lifecycle = cx.global_mut::<ApplicationWindowLifecycle>();
    lifecycle
        .handlers
        .retain(|(handle, _)| open.contains(handle));
    let Some(exit) = lifecycle.exit.as_mut() else {
        return;
    };
    if exit.removal_scheduled
        || exit
            .waiting_for
            .is_some_and(|handle| open.contains(&handle))
    {
        return;
    }
    if !open.contains(&exit.main_window) {
        lifecycle.exit = None;
        return;
    }
    exit.waiting_for = None;
    if let Some(child) = open
        .iter()
        .find(|handle| **handle != exit.main_window)
        .copied()
    {
        exit.waiting_for = Some(child);
        let handler = lifecycle
            .handlers
            .iter()
            .find(|(handle, _)| *handle == child)
            .map(|(_, handler)| Rc::clone(handler));
        let _ = child.update(cx, |_, window, cx| {
            window.activate_window();
            if let Some(handler) = handler {
                handler(window, cx);
            } else {
                // Plain auxiliary windows have no persistence/WebView barrier.
                remove_window_after_current_frame(window, cx, None);
            }
        });
    } else if !exit.main_close_requested {
        exit.main_close_requested = true;
        let main = exit.main_window;
        let close = Rc::clone(&exit.close_main);
        let _ = main.update(cx, |_, window, cx| close(window, cx));
    } else if exit.main_ready {
        exit.removal_scheduled = true;
        let main = exit.main_window;
        let _ = main.update(cx, |_, window, cx| {
            remove_window_after_current_frame(window, cx, None);
        });
    }
}

pub(crate) fn finish_library_window_close(window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    if cx.has_global::<ApplicationWindowLifecycle>() {
        let lifecycle = cx.global_mut::<ApplicationWindowLifecycle>();
        if let Some(exit) = lifecycle
            .exit
            .as_mut()
            .filter(|exit| exit.main_window == handle)
        {
            exit.main_ready = true;
            cx.defer(advance_application_exit);
            return;
        }
    }
    remove_window_after_current_frame(window, cx, None);
}
