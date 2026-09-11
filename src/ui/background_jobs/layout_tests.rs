//! Render the complete production window through GPUI layout and mouse input.
//! Synthetic task snapshots isolate layout from provider and worker timing;
//! services use a fresh local directory and in-memory credentials only.

use super::*;
use gpui::{
    Modifiers, Pixels, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext, point,
};
use moye_epub_editor::{
    credentials::MemoryCredentialStore,
    job_diagnostics::{BackgroundJobLogEntry, JobLogLevel, JobLogMetrics},
    services::BackgroundJobProgress,
};

fn task_id(index: usize) -> String {
    format!("translation:layout-source-{index:04}-0123456789abcdef:zh-Hans")
}

fn snapshots() -> Vec<BackgroundJobSnapshot> {
    (0..56)
        .map(|index| BackgroundJobSnapshot {
            id: task_id(index),
            book_id: format!("book-{}", index / 14),
            source_id: Some(format!("layout-source-{index:04}")),
            kind: if index % 2 == 0 {
                "translation"
            } else {
                "embedding"
            }
            .into(),
            status: if index % 3 == 0 {
                BackgroundJobStatus::Failed
            } else {
                BackgroundJobStatus::Paused
            },
            pause_requested: false,
            cancel_requested: false,
            attempts: 3,
            progress: BackgroundJobProgress {
                completed: 17,
                total: Some(78),
            },
            error: Some("当前模型请求失败。请检查模型服务后重试。".repeat(20)),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_010,
            started_at: Some(1_700_000_001),
            finished_at: Some(1_700_000_010),
            cursor_json: Some(
                serde_json::json!({
                    "next_ordinal": 17,
                    "unit_ids": (0..80).map(|unit| format!("unit-{unit:04}")).collect::<Vec<_>>()
                })
                .to_string(),
            ),
        })
        .collect()
}

fn open_fixture(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    Entity<BackgroundJobsWindow>,
    &mut VisualTestContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let services = Arc::new(
        AppServices::open_with_credentials(
            directory.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap(),
    );
    cx.update(gpui_component::init);
    let mut view = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let jobs = cx.new(|cx| {
            let books = (0..4)
                .map(|index| BackgroundJobBook {
                    id: format!("book-{index}"),
                    title: format!("Layout book {index} — 长书名验证任务列表与详情区域保持可见"),
                })
                .collect();
            let mut jobs =
                BackgroundJobsWindow::new(services, books, "隔离布局回归".into(), window, cx);
            jobs.auto_refresh = false;
            // Cancel the real periodic timer: this test drives explicit state
            // and mouse events and never needs to contact the task workers.
            jobs._poll_task = Task::ready(());
            jobs.jobs = snapshots();
            jobs.loaded = true;
            jobs.reconcile_selection();
            jobs
        });
        view = Some(jobs.clone());
        Root::new(jobs, window, cx)
    });
    (directory, view.unwrap(), visual)
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing rendered region {selector}"))
}

fn assert_regions_fit(cx: &mut VisualTestContext, width: f32, height: f32) {
    let root = bounds(cx, "jobs-layout-root");
    assert_eq!(root.size, size(px(width), px(height)));
    let selectors = [
        "jobs-layout-header",
        "jobs-layout-filters",
        "jobs-layout-list",
        "jobs-layout-footer",
        "jobs-layout-detail",
    ];
    let regions: Vec<_> = selectors
        .iter()
        .map(|selector| {
            let region = bounds(cx, selector);
            assert!(
                region.size.width > px(0.) && region.size.height > px(0.),
                "{selector} collapsed at {width}x{height}: {region:?}"
            );
            assert!(
                region.left() >= root.left() - px(0.5)
                    && region.top() >= root.top() - px(0.5)
                    && region.right() <= root.right() + px(0.5)
                    && region.bottom() <= root.bottom() + px(0.5),
                "{selector} escaped {width}x{height}: {region:?}, root={root:?}"
            );
            region
        })
        .collect();
    for (index, adjacent) in regions.windows(2).enumerate() {
        assert!(
            adjacent[0].bottom() <= adjacent[1].top() + px(0.5),
            "{} overlaps {} at {width}x{height}: {:?}",
            selectors[index],
            selectors[index + 1],
            adjacent
        );
    }
}

#[gpui::test]
fn complete_background_task_layout_keeps_all_controls_inside_both_window_sizes(
    cx: &mut TestAppContext,
) {
    let (_directory, view, visual) = open_fixture(cx);
    for (width, height) in [(900., 640.), (1180., 820.)] {
        visual.simulate_resize(size(px(width), px(height)));
        for show_logs in [false, true] {
            visual.update(|_, cx| {
                view.update(cx, |jobs, cx| {
                    jobs.show_logs = show_logs;
                    jobs.logs = Some(BackgroundJobLogSnapshot {
                        entries: (0..500)
                            .map(|ordinal| BackgroundJobLogEntry {
                                timestamp_ms: 1_700_000_000_000 + ordinal,
                                level: JobLogLevel::Info,
                                stage: "persist".into(),
                                message: "当前单元结果已持久化".into(),
                                metrics: JobLogMetrics {
                                    ordinal: Some(ordinal),
                                    total: Some(500),
                                    ..Default::default()
                                },
                            })
                            .collect(),
                        truncated: true,
                    });
                    cx.notify();
                });
            });
            redraw(visual);
            assert_regions_fit(visual, width, height);
        }
    }
}

#[gpui::test]
fn pagination_clicks_select_the_current_page_after_filter_changes(cx: &mut TestAppContext) {
    let (_directory, view, visual) = open_fixture(cx);
    visual.simulate_resize(size(px(900.), px(640.)));
    redraw(visual);
    let list = bounds(visual, "jobs-layout-list");
    visual.simulate_event(ScrollWheelEvent {
        position: list.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-1_000.))),
        ..Default::default()
    });
    redraw(visual);
    visual.update(|_, cx| {
        assert!(view.read(cx).list_scroll.offset().y < px(0.));
    });
    for (page, index) in [(1, 12), (2, 24), (3, 36), (4, 48)] {
        let next = bounds(visual, "jobs-next");
        visual.simulate_click(next.center(), Modifiers::none());
        redraw(visual);
        visual.update(|_, cx| {
            let jobs = view.read(cx);
            assert_eq!(jobs.page, page);
            assert_eq!(jobs.selected_job_id, Some(task_id(index)));
            assert_eq!(jobs.list_scroll.offset().y, px(0.));
        });
        assert_regions_fit(visual, 900., 640.);
    }
    visual.update(|window, cx| {
        view.update(cx, |jobs, cx| {
            jobs.kind = "translation";
            jobs.filters_changed(window, cx);
            assert_eq!(jobs.page, 0);
            assert_eq!(jobs.filtered_jobs().len(), 28);
            assert_eq!(jobs.selected_job_id, Some(task_id(0)));
        });
    });
    redraw(visual);
    let next = bounds(visual, "jobs-next");
    visual.simulate_click(next.center(), Modifiers::none());
    redraw(visual);
    visual.update(|_, cx| {
        let jobs = view.read(cx);
        assert_eq!(jobs.page, 1);
        assert_eq!(jobs.selected_job_id, Some(task_id(24)));
    });
    visual.update(|window, cx| {
        view.update(cx, |jobs, cx| {
            jobs.query = "Layout book 1".into();
            jobs.filters_changed(window, cx);
            assert_eq!(jobs.page, 0);
            assert_eq!(jobs.filtered_jobs().len(), 7);
            assert_eq!(jobs.selected_job_id, Some(task_id(24)));
            jobs.status = Some(BackgroundJobStatus::Failed);
            jobs.filters_changed(window, cx);
            assert_eq!(jobs.filtered_jobs().len(), 2);
            assert_eq!(jobs.selected_job_id, Some(task_id(24)));
            jobs.query = "Layout book 0".into();
            jobs.filters_changed(window, cx);
            assert_eq!(jobs.filtered_jobs().len(), 3);
            assert_eq!(jobs.selected_job_id, Some(task_id(0)));
        });
    });
    redraw(visual);
    assert_regions_fit(visual, 900., 640.);
}
