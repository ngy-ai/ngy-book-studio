//! Opening and activating the AI settings window.

use super::*;

/// 打开五个设置窗口里的一个。
pub(crate) fn open_ai_settings_window(
    services: Arc<AppServices>,
    section: AiSettingsSection,
    cx: &mut App,
) -> Result<()> {
    // 配置读不出来就不开窗口：让调用方把原因显示在主窗口的错误栏里。
    let settings = services.provider_settings()?;
    match section {
        AiSettingsSection::Endpoint => {
            open_settings_window::<EndpointSettingsWindow>(cx, move |window, cx| {
                EndpointSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::Models => {
            open_settings_window::<ModelsSettingsWindow>(cx, move |window, cx| {
                ModelsSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::WebSearch => {
            open_settings_window::<WebSearchSettingsWindow>(cx, move |window, cx| {
                WebSearchSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::BackgroundJobs => {
            open_settings_window::<BackgroundJobsSettingsWindow>(cx, move |window, cx| {
                BackgroundJobsSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::System => {
            open_settings_window::<SystemSettingsWindow>(cx, move |window, cx| {
                SystemSettingsWindow::new(services, settings, window, cx)
            })
        }
    }
}

pub(crate) fn open_settings_window<V: SettingsWindowView>(
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    ensure_settings_window_trackers(cx);
    if activate_existing_settings_window::<V>(cx)? {
        return Ok(());
    }
    let section = V::section();
    {
        let tracker = V::tracker(cx);
        tracker.opening = true;
        tracker.closing = false;
        tracker.handle = None;
        tracker.view = None;
    }
    let opened: Result<gpui::AnyWindowHandle> = (|| {
        let bounds = Bounds::centered(None, section.window_size(), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(section.window_min_size()),
                    titlebar: Some(TitlebarOptions {
                        title: Some(section.window_title().into()),
                        ..Default::default()
                    }),
                    app_id: Some(section.app_id()),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| build(window, cx));
                    V::tracker(cx).view = Some(view.downgrade());
                    let settings_window = gpui::Window::window_handle(window);
                    let close_view = view.downgrade();
                    let close_requested = Cell::new(false);
                    on_window_close(window, cx, move |window, cx| {
                        // 退出软件时才等这次保存的结果（`ApplicationExit` 逐个关子窗口，
                        // 已接受的凭据/SQLite 写入不能在半路被丢掉）。用户自己关窗不拦：
                        // 写入仍由运行中的任务提交或回滚，视图消失只是丢掉结果回调
                        // （下面的 `let _ = view.update(...)` 本来就能容忍）。
                        // 无条件拦下来会把一次被后台任务堵住的保存变成关不掉的死窗口
                        // （现场 2026-09-17）；等待的上界由 [`SAVE_WAIT_LIMIT`] 给出。
                        if application_is_exiting(cx)
                            && close_view.upgrade().is_some_and(|view| {
                                view.read(cx).state().operation == PendingOperation::Saving
                            })
                        {
                            return false;
                        }
                        let tracker = V::tracker(cx);
                        if tracker.handle == Some(settings_window) {
                            tracker.closing = true;
                        }
                        window.minimize_window();
                        if !close_requested.replace(true) {
                            remove_window_after_current_frame(window, cx, None);
                        }
                        false
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .with_context(|| format!("无法创建「{}」设置窗口", section.label()))?;
        Ok(handle.into())
    })();

    let tracker = V::tracker(cx);
    tracker.opening = false;
    match opened {
        Ok(handle) => {
            tracker.handle = Some(handle);
            Ok(())
        }
        Err(error) => {
            tracker.closing = false;
            tracker.handle = None;
            tracker.view = None;
            Err(error)
        }
    }
}

pub(crate) fn activate_existing_settings_window<V: SettingsWindowView>(
    cx: &mut App,
) -> Result<bool> {
    let (opening, closing, handle, view_is_alive) = {
        let tracker = V::tracker(cx);
        (
            tracker.opening,
            tracker.closing,
            tracker.handle,
            tracker
                .view
                .as_ref()
                .is_some_and(|view| view.upgrade().is_some()),
        )
    };
    let open_windows = cx.windows();
    match existing_window_disposition(opening, closing, handle, view_is_alive, |handle| {
        open_windows.contains(&handle)
    }) {
        ExistingWindowDisposition::OpenNew => Ok(false),
        ExistingWindowDisposition::SuppressReentrantOpen => Ok(true),
        ExistingWindowDisposition::ClearStale(handle) => {
            let tracker = V::tracker(cx);
            if tracker.handle == Some(handle) {
                tracker.closing = false;
                tracker.handle = None;
                tracker.view = None;
            }
            Ok(false)
        }
        ExistingWindowDisposition::Activate(handle) => {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .with_context(|| {
                    format!("无法激活已经打开的「{}」设置窗口", V::section().label())
                })?;
            Ok(true)
        }
    }
}
