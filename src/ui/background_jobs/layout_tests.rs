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

/// Synthetic block list matching the snapshot progress: a book long enough to
/// need several inspector pages, with empty chapter titles to exercise the
/// fallback label.
fn block_list(total: usize) -> TranslationBlockList {
    TranslationBlockList {
        target_language: "zh-Hans".into(),
        total,
        next_ordinal: 17,
        blocks: (0..total)
            .map(|ordinal| TranslationBlockInfo {
                ordinal,
                unit_ordinal: ordinal / 4,
                unit_title: if ordinal % 4 == 0 {
                    String::new()
                } else {
                    format!("第 {} 节 — 用于验证标题截断的长章节名", ordinal / 4 + 1)
                },
                source_preview: format!(
                    "文本块 {ordinal} 的原文预览内容，用于验证单行截断与状态徽章的对齐"
                ),
            })
            .collect(),
    }
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
        "jobs-layout-kinds",
        "jobs-layout-filters",
        "jobs-layout-list",
        "jobs-layout-footer",
        "jobs-layout-detail",
        "jobs-layout-resize-handle",
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

    // The kind switcher spans the top; below it the body is split into a
    // stacked list column on the left and the detail panel as the main subject
    // on the right.
    let header = &regions[0];
    let kinds = &regions[1];
    let filters = &regions[2];
    let list = &regions[3];
    let footer = &regions[4];
    let detail = &regions[5];
    let handle = &regions[6];
    assert!(
        header.bottom() <= kinds.top() + px(0.5),
        "kinds overlap header at {width}x{height}: {header:?}, {kinds:?}"
    );
    assert!(
        kinds.bottom() <= filters.top() + px(0.5),
        "filters overlap kinds at {width}x{height}: {kinds:?}, {filters:?}"
    );
    assert!(
        kinds.bottom() <= detail.top() + px(0.5),
        "detail overlaps kinds at {width}x{height}: {kinds:?}, {detail:?}"
    );
    assert!(
        filters.bottom() <= list.top() + px(0.5),
        "list overlaps filters at {width}x{height}: {filters:?}, {list:?}"
    );
    assert!(
        list.bottom() <= footer.top() + px(0.5),
        "footer overlaps list at {width}x{height}: {list:?}, {footer:?}"
    );
    assert!(
        list.right() <= handle.left() + px(0.5) && handle.right() <= detail.left() + px(0.5),
        "resize handle is not between the list and detail columns at {width}x{height}: {list:?}, {handle:?}, {detail:?}"
    );
    assert!(
        handle.top() <= filters.top() + px(0.5) && handle.bottom() >= footer.bottom() - px(0.5),
        "resize handle does not span the body at {width}x{height}: {handle:?}, {filters:?}, {footer:?}"
    );
    assert!(
        detail.top() <= filters.top() + px(0.5) && filters.top() <= detail.top() + px(0.5),
        "detail and list column do not share the body top at {width}x{height}: {detail:?}, {filters:?}"
    );
    assert!(
        detail.bottom() <= root.bottom() + px(0.5) && footer.bottom() <= root.bottom() + px(0.5),
        "body overflows root at {width}x{height}: footer={footer:?}, detail={detail:?}, root={root:?}"
    );
}

#[gpui::test]
fn complete_background_task_layout_keeps_all_controls_inside_both_window_sizes(
    cx: &mut TestAppContext,
) {
    let (_directory, view, visual) = open_fixture(cx);
    for (width, height) in [(900., 640.), (1180., 820.)] {
        visual.simulate_resize(size(px(width), px(height)));
        for tab in [DetailTab::Summary, DetailTab::Logs, DetailTab::Blocks] {
            visual.update(|_, cx| {
                view.update(cx, |jobs, cx| {
                    jobs.tab = tab;
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
                    jobs.blocks = Some(block_list(78));
                    if let Some(job) = jobs.jobs.first_mut() {
                        job.status = BackgroundJobStatus::Running;
                    }
                    cx.notify();
                });
            });
            redraw(visual);
            assert_regions_fit(visual, width, height);
        }
    }
}

#[test]
fn translation_block_inspector_pages_filters_and_marks_the_in_flight_block() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    let progress = translation_progress(&job, blocks.total);
    assert_eq!(
        (progress.done, progress.processing, progress.pending),
        (17, 1, 60)
    );

    let first = block_page_indices(&blocks, progress, None, 0);
    assert_eq!(first, (0..BLOCKS_PER_PAGE).collect::<Vec<_>>());
    let second = block_page_indices(&blocks, progress, None, 1);
    assert_eq!(second.first(), Some(&50));
    assert_eq!(second.last(), Some(&77));

    assert_eq!(
        block_page_indices(&blocks, progress, Some(BlockState::Done), 0),
        (0..17).collect::<Vec<_>>()
    );
    assert_eq!(
        block_page_indices(&blocks, progress, Some(BlockState::Processing), 0),
        vec![17]
    );
    let pending = block_page_indices(&blocks, progress, Some(BlockState::Pending), 0);
    assert_eq!(pending.first(), Some(&18));
    assert_eq!(pending.len(), BLOCKS_PER_PAGE);

    // A finished task leaves no block in flight, and the page controls cannot
    // scroll past the last block.
    job.progress.completed = blocks.total;
    let finished = translation_progress(&job, blocks.total);
    assert_eq!(
        (
            finished.done,
            finished.processing,
            finished.pending,
            finished.running
        ),
        (78, 0, 0, false)
    );
    assert!(block_page_indices(&blocks, finished, Some(BlockState::Processing), 0).is_empty());
    // Past the last page there is nothing left to show.
    assert!(block_page_indices(&blocks, finished, None, 2).is_empty());

    // A stale snapshot that overran the current block list stays consistent.
    job.progress.completed = 200;
    let stale = translation_progress(&job, blocks.total);
    assert_eq!((stale.done, stale.processing, stale.pending), (78, 0, 0));
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

#[gpui::test]
fn divider_drag_clamps_the_list_column_and_keeps_the_detail_readable(cx: &mut TestAppContext) {
    let (_directory, view, visual) = open_fixture(cx);
    visual.simulate_resize(size(px(900.), px(640.)));
    redraw(visual);
    let maximum = 900. - JOBS_DETAIL_MIN_WIDTH - JOBS_RESIZE_HANDLE_WIDTH;
    let handle = bounds(visual, "jobs-layout-resize-handle");
    assert_eq!(handle.size.width, px(JOBS_RESIZE_HANDLE_WIDTH));
    visual.update(|_, cx| assert_eq!(view.read(cx).list_width, JOBS_LIST_DEFAULT_WIDTH));

    // The divider follows the pointer, which sits on the handle center, so half
    // the handle belongs to the column on its left.
    visual.update(|_, cx| {
        view.update(cx, |jobs, cx| {
            assert!(jobs.begin_list_resize());
            assert!(jobs.resize_list_from_pointer(
                px(500. + JOBS_RESIZE_HANDLE_WIDTH / 2.),
                px(900.),
            ));
            assert_eq!(jobs.list_width, 500.);
            assert!(jobs.finish_list_resize());
            cx.notify();
        });
    });
    redraw(visual);
    assert_eq!(bounds(visual, "jobs-layout-list-column").size.width, px(500.));
    assert_regions_fit(visual, 900., 640.);

    // The column is clamped on both sides: it never grows past what the detail
    // panel's minimum leaves, and never drops below the readable minimum.
    visual.update(|_, cx| {
        view.update(cx, |jobs, cx| {
            assert!(jobs.begin_list_resize());
            assert!(jobs.resize_list_from_pointer(px(900.), px(900.)));
            assert_eq!(jobs.list_width, maximum);
            assert!(jobs.resize_list_from_pointer(px(0.), px(900.)));
            assert_eq!(jobs.list_width, JOBS_LIST_MIN_WIDTH);
            assert!(jobs.finish_list_resize());
            assert!(!jobs.resize_list_from_pointer(px(600.), px(900.)));
            cx.notify();
        });
    });
    redraw(visual);
    assert_regions_fit(visual, 900., 640.);

    // A stored width that no longer fits is clamped while rendering, so a
    // shrinking window cannot squeeze the detail panel below its minimum.
    visual.update(|_, cx| {
        view.update(cx, |jobs, cx| {
            jobs.list_width = JOBS_LIST_MAX_WIDTH;
            cx.notify();
        });
    });
    redraw(visual);
    visual.update(|_, cx| {
        let jobs = view.read(cx);
        assert_eq!(jobs.effective_list_width(px(900.)), maximum);
        assert_eq!(jobs.effective_list_width(px(1600.)), JOBS_LIST_MAX_WIDTH);
    });
    assert_regions_fit(visual, 900., 640.);
}
