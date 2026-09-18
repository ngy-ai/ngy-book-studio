//! The diagnostic summary and the entry point that opens the window.

use super::*;

pub(crate) fn diagnostic_summary(job: &BackgroundJobSnapshot) -> String {
    let mut lines = detail_rows(job)
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>();
    if let Some(error) = &job.error {
        lines.push(format!("错误: {error}"));
    }
    if let Some(cursor) = &job.cursor_json {
        lines.push(format!("执行游标:\n{}", prettify_cursor(cursor)));
    }
    lines.join("\n")
}

pub(crate) fn open_background_jobs_window(
    services: Arc<AppServices>,
    books: Vec<BackgroundJobBook>,
    scope_label: String,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    // One background task window for the whole application: every window would
    // only drive the same shared job queue. A repeat request activates the live
    // window and retargets it at the new library scope.
    let key = background_jobs_window_key();
    match reserve_singleton_window(&key, cx) {
        SingletonWindowReservation::Activate(handle) => {
            if let Some(view) = existing_background_jobs_window(cx) {
                let _ = handle.update(cx, |_, window, cx| {
                    view.update(cx, |this, cx| {
                        this.apply_scope(books, scope_label, window, cx);
                    });
                    window.activate_window();
                });
            } else {
                activate_singleton_window(handle, cx);
            }
            return Ok(());
        }
        SingletonWindowReservation::InFlight => return Ok(()),
        SingletonWindowReservation::Reserved => {}
    }
    let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(900.), px(640.))),
            titlebar: Some(TitlebarOptions {
                title: Some("墨页 · 后台任务".into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio.background-jobs".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let jobs = cx.new(|cx| {
                BackgroundJobsWindow::new(Arc::clone(&services), books, scope_label, window, cx)
            });
            jobs.update(cx, |jobs, cx| jobs.refresh(window, cx));
            register_background_jobs_window(jobs.downgrade(), cx);
            on_window_close(window, cx, |_, _| true);
            cx.new(|cx| Root::new(jobs, window, cx))
        },
    );
    match opened {
        Ok(handle) => {
            complete_singleton_window(&key, handle.into(), cx);
            Ok(())
        }
        Err(error) => {
            release_singleton_window(&key, cx);
            Err(error).context("无法创建后台任务窗口")
        }
    }
}
