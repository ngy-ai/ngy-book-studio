//! Opening or focusing the learning window.

use super::*;

pub(crate) fn open_learning_window(services: Arc<AppServices>, cx: &mut App) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    if !cx.has_global::<LearningWindowTracker>() {
        cx.set_global(LearningWindowTracker::default());
    }
    let tracker = cx.global::<LearningWindowTracker>();
    if tracker.opening {
        return Ok(());
    }
    if let Some(handle) = tracker.handle
        && cx.windows().contains(&handle)
    {
        if !tracker.closing {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .context("无法激活学习窗口")?;
        }
        return Ok(());
    }
    {
        let tracker = cx.global_mut::<LearningWindowTracker>();
        tracker.opening = true;
        tracker.closing = false;
        tracker.handle = None;
    }
    let service = services.learning();
    let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(1120.), px(700.))),
            titlebar: Some(TitlebarOptions {
                title: Some("墨页 · AI Agent 学习".into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio.learning".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let learning = cx.new(|cx| LearningWindow::new(service, window, cx));
            let weak = learning.downgrade();
            on_window_close(window, cx, move |window, cx| {
                if weak
                    .update(cx, |this, cx| this.request_close(window, cx))
                    .is_err()
                {
                    remove_window_after_current_frame(window, cx, None);
                }
                false
            });
            learning.update(cx, |this, cx| this.load(window, cx));
            cx.new(|cx| Root::new(learning, window, cx))
        },
    );
    let tracker = cx.global_mut::<LearningWindowTracker>();
    tracker.opening = false;
    match opened {
        Ok(handle) => {
            tracker.handle = Some(handle.into());
            Ok(())
        }
        Err(error) => {
            tracker.closing = false;
            Err(error).context("无法创建学习窗口")
        }
    }
}
