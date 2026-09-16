//! Render the complete production window through GPUI layout and mouse input.
//! Synthetic task snapshots isolate layout from provider and worker timing;
//! services use a fresh local directory and in-memory credentials only.

use super::*;
use gpui::{
    Modifiers, Pixels, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext, point,
};
use ngy_book_studio::{
    credentials::MemoryCredentialStore,
    job_diagnostics::{BackgroundJobLogEntry, JobLogLevel, JobLogMetrics},
    services::{BackgroundJobProgress, TranslationBlockRequest},
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
                                event: JobLogEvent::ItemSaved,
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

/// One persisted diagnostic line, reduced to what the inspector reads.
fn log_entry(
    event: JobLogEvent,
    ordinal: Option<u64>,
    error_kind: Option<JobLogErrorKind>,
) -> BackgroundJobLogEntry {
    BackgroundJobLogEntry {
        event,
        timestamp_ms: 1_700_000_000_000,
        level: JobLogLevel::Warning,
        stage: "item".into(),
        message: String::new(),
        metrics: JobLogMetrics {
            ordinal,
            error_kind,
            ..Default::default()
        },
    }
}

fn log_snapshot(entries: Vec<BackgroundJobLogEntry>) -> BackgroundJobLogSnapshot {
    BackgroundJobLogSnapshot {
        entries,
        truncated: false,
    }
}

#[test]
fn translation_block_inspector_pages_filters_and_marks_the_in_flight_blocks() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    // 并发 3 的一次运行：游标只说「第一块没完成的」，处理中的三块由窗口集合给出。
    job.cursor_json =
        Some(serde_json::json!({ "next_ordinal": 17, "inflight": [17, 18, 19] }).to_string());
    let inspector = BlockInspector::new(&job, blocks.total, None);
    let progress = &inspector.progress;
    assert_eq!(
        (progress.done, progress.processing.clone(), progress.pending),
        (17, vec![17, 18, 19], 58)
    );
    assert_eq!(inspector.state(16), BlockState::Done);
    assert_eq!(inspector.state(17), BlockState::Processing);
    assert_eq!(inspector.state(19), BlockState::Processing);
    assert_eq!(inspector.state(20), BlockState::Pending);

    let first = block_page_indices(&blocks, &inspector, None, 0);
    assert_eq!(first, (0..BLOCKS_PER_PAGE).collect::<Vec<_>>());
    let second = block_page_indices(&blocks, &inspector, None, 1);
    assert_eq!(second.first(), Some(&50));
    assert_eq!(second.last(), Some(&77));

    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Done), 0),
        (0..17).collect::<Vec<_>>()
    );
    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Processing), 0),
        vec![17, 18, 19]
    );
    let pending = block_page_indices(&blocks, &inspector, Some(BlockState::Pending), 0);
    assert_eq!(pending.first(), Some(&20));
    assert_eq!(pending.len(), BLOCKS_PER_PAGE);

    // A task whose row says running but whose cursor carries no window (an older
    // format, or a process that died between two rounds) claims nothing is in
    // flight instead of inventing one block at the cursor.
    job.cursor_json = Some(serde_json::json!({ "next_ordinal": 17 }).to_string());
    let unwindowed = BlockInspector::new(&job, blocks.total, None);
    assert!(unwindowed.progress.processing.is_empty());
    assert_eq!(unwindowed.progress.pending, 61);
    assert_eq!(
        block_page_indices(&blocks, &unwindowed, Some(BlockState::Processing), 0),
        Vec::<usize>::new()
    );

    // A finished task leaves no block in flight, and the page controls cannot
    // scroll past the last block.
    job.cursor_json =
        Some(serde_json::json!({ "next_ordinal": 17, "inflight": [17, 18, 19] }).to_string());
    job.progress.completed = blocks.total;
    let finished = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(
        (
            finished.progress.done,
            finished.progress.processing.clone(),
            finished.progress.pending
        ),
        (78, Vec::new(), 0)
    );
    assert!(block_page_indices(&blocks, &finished, Some(BlockState::Processing), 0).is_empty());
    // Past the last page there is nothing left to show.
    assert!(block_page_indices(&blocks, &finished, None, 2).is_empty());

    // A stale snapshot that overran the current block list stays consistent.
    job.progress.completed = 200;
    let stale = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(
        (
            stale.progress.done,
            stale.progress.processing.clone(),
            stale.progress.pending
        ),
        (78, Vec::new(), 0)
    );
}

/// The durable cursor alone can only say how far a failed task got: the blocks
/// it skipped are named by the per-block diagnostics, and those are what the
/// window has to show.
#[test]
fn translation_block_inspector_names_the_blocks_that_failed() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Failed;
    job.progress.completed = 60;
    let logs = log_snapshot(vec![
        log_entry(JobLogEvent::RunStarted, None, None),
        log_entry(JobLogEvent::ItemSaved, Some(1), None),
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(58),
            Some(JobLogErrorKind::InvalidSchema),
        ),
        log_entry(
            JobLogEvent::ProtocolRejected,
            Some(61),
            Some(JobLogErrorKind::SegmentCountMismatch),
        ),
        log_entry(
            JobLogEvent::StepFailed,
            Some(61),
            Some(JobLogErrorKind::SegmentCountMismatch),
        ),
        // The headline failure carries the run position, not a block.
        log_entry(
            JobLogEvent::RunFailed,
            Some(60),
            Some(JobLogErrorKind::Timeout),
        ),
    ]);
    let inspector = BlockInspector::new(&job, blocks.total, Some(&logs));

    assert_eq!(inspector.progress.failed, 2);
    assert_eq!(inspector.state(58), BlockState::Failed);
    assert_eq!(inspector.state(61), BlockState::Failed);
    assert_eq!(inspector.reason(58), Some("响应结构不符合分段协议"));
    assert_eq!(inspector.reason(61), Some("片段数量与原文不一致"));
    // A rejected attempt that was retried into a valid answer is not a failure.
    assert_eq!(inspector.reason(60), None);
    assert_eq!(inspector.state(60), BlockState::Pending);
    assert_eq!(inspector.reason(1), None);
    assert_eq!(inspector.state(1), BlockState::Done);

    // The failed blocks leave the state the cursor gave them, so the header
    // still adds up to the number of blocks.
    let progress = &inspector.progress;
    assert_eq!(progress.cursor, 60);
    assert_eq!(progress.done, 59);
    assert_eq!(
        progress.done + progress.processing.len() + progress.failed + progress.pending,
        progress.total
    );

    // They can be filtered and jumped to on their own.
    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Failed), 0),
        vec![58, 61]
    );
    assert_eq!(
        anchor_page(&blocks, &inspector, Some(BlockState::Failed)),
        0
    );
    assert_eq!(
        anchor_page(&blocks, &inspector, Some(BlockState::Pending)),
        0
    );
    assert_eq!(
        untranslated_summary(&inspector).as_deref(),
        Some("本次执行未翻译的文本块：#58、#61")
    );
    // A failed task must explain itself in the inspector, not only in the log.
    assert!(failure_banner(&job, &inspector).is_some());
}

#[test]
fn translation_block_inspector_reports_every_untranslated_block_in_the_banner() {
    let blocks = block_list(200);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Failed;
    job.progress.completed = 0;
    let mut entries = vec![log_entry(JobLogEvent::RunStarted, None, None)];
    entries.extend((0..30).map(|ordinal| {
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(ordinal),
            Some(JobLogErrorKind::InvalidSchema),
        )
    }));
    let logs = log_snapshot(entries);
    let inspector = BlockInspector::new(&job, blocks.total, Some(&logs));
    assert_eq!(inspector.progress.failed, 30);
    let summary = untranslated_summary(&inspector).expect("失败块必须能被列出来");
    assert!(summary.contains("#0、#1"), "{summary}");
    assert!(summary.contains("共 30 个"), "{summary}");
    assert!(
        !summary.contains("#12"),
        "清单必须有上限，否则横幅会变成数字墙：{summary}"
    );
}

/// A retry must not inherit the failures of the attempt before it, and a
/// controlled stop is not a block failure.
#[test]
fn translation_block_inspector_scopes_failures_to_the_newest_execution() {
    let blocks = block_list(20);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    job.progress.completed = 12;
    let logs = log_snapshot(vec![
        log_entry(JobLogEvent::RunStarted, None, None),
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(3),
            Some(JobLogErrorKind::InvalidSchema),
        ),
        log_entry(JobLogEvent::RunFailed, Some(3), None),
        // A new execution translated what the previous one skipped.
        log_entry(JobLogEvent::RunStarted, None, None),
        log_entry(JobLogEvent::ItemSaved, Some(3), None),
        log_entry(
            JobLogEvent::StepFailed,
            Some(11),
            Some(JobLogErrorKind::Cancelled),
        ),
    ]);
    let inspector = BlockInspector::new(&job, blocks.total, Some(&logs));
    assert_eq!(inspector.progress.failed, 0);
    assert_eq!(inspector.state(3), BlockState::Done);
    assert!(failure_banner(&job, &inspector).is_none());

    // Without any log at all the inspector still renders from the cursor: 已处理与待处理
    // 由游标决定，处理中由游标里的窗口集合决定（单块在飞时就是游标那一块）。
    job.cursor_json = Some(serde_json::json!({ "next_ordinal": 12, "inflight": [12] }).to_string());
    let bare = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(bare.progress.failed, 0);
    assert_eq!(bare.state(11), BlockState::Done);
    assert_eq!(bare.state(12), BlockState::Processing);
    assert_eq!(bare.state(13), BlockState::Pending);
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
            assert!(
                jobs.resize_list_from_pointer(px(500. + JOBS_RESIZE_HANDLE_WIDTH / 2.), px(900.),)
            );
            assert_eq!(jobs.list_width, 500.);
            assert!(jobs.finish_list_resize());
            cx.notify();
        });
    });
    redraw(visual);
    assert_eq!(
        bounds(visual, "jobs-layout-list-column").size.width,
        px(500.)
    );
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

/// The two repair routes of a finished translation: the header's 重试失败块,
/// sitting in front of 重新翻译 and re-sending every untranslated block, and a
/// failed row's own 重试, which re-sends exactly that block. A translated row
/// offers neither, because there is nothing to repair and no request to spend.
#[gpui::test]
fn failed_blocks_offer_a_bulk_retry_before_retranslate_and_a_row_retry(cx: &mut TestAppContext) {
    let (_directory, view, visual) = open_fixture(cx);
    visual.simulate_resize(size(px(1180.), px(820.)));
    redraw(visual);
    // The state the screenshot shows: the task finished as `Succeeded` with two
    // blocks the newest execution left untranslated (18 and 19, page 0).
    visual.update(|window, cx| {
        view.update(cx, |jobs, cx| {
            jobs.tab = DetailTab::Blocks;
            jobs.blocks = Some(block_list(78));
            jobs.blocks_job_id = Some(task_id(0));
            jobs.blocks_error = None;
            jobs.blocks_loading = false;
            jobs.logs = Some(log_snapshot(vec![
                log_entry(JobLogEvent::RunStarted, None, None),
                log_entry(
                    JobLogEvent::ProtocolSkipped,
                    Some(18),
                    Some(JobLogErrorKind::InvalidSchema),
                ),
                log_entry(
                    JobLogEvent::ProtocolSkipped,
                    Some(19),
                    Some(JobLogErrorKind::InvalidSchema),
                ),
            ]));
            let job = jobs
                .jobs
                .iter_mut()
                .find(|job| job.id == task_id(0))
                .unwrap();
            job.status = BackgroundJobStatus::Succeeded;
            job.error = None;
            job.progress.completed = job.progress.total.unwrap_or_default();
            cx.notify();
        });
        // Draw this frame directly: `run_until_parked` would let the queued load
        // for the synthetic task run again and replace the list with an error.
        window.draw(cx).clear();
    });

    // Reproduce the state the inspector derives from: failed blocks 18 and 19.
    let inspector = visual.update(|_, cx| {
        let jobs = view.read(cx);
        let job = jobs.jobs.iter().find(|job| job.id == task_id(0)).unwrap();
        BlockInspector::new(job, 78, jobs.logs.as_ref())
    });
    assert_eq!(inspector.state(18), BlockState::Failed);
    assert_eq!(inspector.state(19), BlockState::Failed);
    assert_ne!(inspector.state(0), BlockState::Failed);

    // A finished task has nothing left to resume, so 重新翻译 is its only header
    // action — and the repair button has to fit in front of it.
    let bulk = bounds(visual, "jobs-blocks-retry-failed");
    let retranslate = bounds(visual, "background-job-action-retranslate");
    assert!(
        bulk.right() <= retranslate.left(),
        "重试失败块 must sit in front of 重新翻译: bulk={bulk:?}, retranslate={retranslate:?}"
    );
    let copy = bounds(visual, "jobs-copy-diagnostics");
    assert!(
        retranslate.right() <= copy.left(),
        "重新翻译 stays in front of the copy button: retranslate={retranslate:?}, copy={copy:?}"
    );

    // The per-row route: exactly the block it belongs to, after 查看.
    let retry = bounds(visual, "jobs-block-retry-18");
    let view_button = bounds(visual, "jobs-block-view-18");
    assert!(
        retry.left() >= view_button.right(),
        "retry follows 查看 in the same row: retry={retry:?}, view={view_button:?}"
    );
    // A translated block has no retry at all.
    assert!(
        visual.debug_bounds("jobs-block-retry-0").is_none(),
        "a block with a translation must not offer a retry"
    );
    assert_regions_fit(visual, 1180., 820.);
}

/// The debug panel of one block: what it shows, that it never grows past the
/// window, and that a click on the overlay closes it without also activating
/// the block row underneath.
#[gpui::test]
fn block_detail_panel_shows_every_payload_and_stays_inside_the_window(cx: &mut TestAppContext) {
    let (_directory, view, visual) = open_fixture(cx);
    visual.simulate_resize(size(px(900.), px(640.)));
    redraw(visual);
    visual.update(|window, cx| {
        view.update(cx, |jobs, cx| {
            jobs.tab = DetailTab::Blocks;
            jobs.blocks = Some(block_list(78));
            jobs.blocks_job_id = Some(task_id(0));
            assert!(jobs.blocks_error.is_none());
            jobs.open_block_detail(0, false, window, cx);
        });
    });
    redraw(visual);
    // Loading is driven by the real service, which has no such task here, so the
    // panel must report the failure instead of showing a stale or empty body.
    visual.update(|_, cx| {
        let state = view.read(cx).block_detail.as_ref().unwrap();
        assert_eq!(state.ordinal, 0);
        assert_eq!(state.tab, BlockDetailTab::Overview);
        assert!(state.detail.is_none(), "the synthetic task has no blocks");
    });

    // Feed a real payload so every tab renders its own text.
    let detail = TranslationBlockDetail {
        ordinal: 0,
        unit_id: "unit-0000".into(),
        unit_ordinal: 0,
        unit_title: "第一章".into(),
        unit_revision: 1,
        block_id: "unit-0000::h0".into(),
        source_text: "原文".repeat(200),
        segments: vec!["原文".repeat(100), "第二段".to_string()],
        request: TranslationBlockRequest {
            model: "chat-test".into(),
            target_language: "zh-Hans".into(),
            source_language: Some("en".into()),
            max_tokens: 4_096,
            temperature: 0.0,
            body: serde_json::json!({
                "model": "chat-test",
                "messages": [
                    { "role": "system", "content": "系统提示词" },
                    { "role": "user", "content": "用户内容" }
                ]
            })
            .to_string(),
            system_prompt: "系统提示词".into(),
            user_content: "用户内容".into(),
        },
    };
    visual.update(|_, cx| {
        view.update(cx, |jobs, cx| {
            let state = jobs.block_detail.as_mut().unwrap();
            state.loading = false;
            state.error = None;
            state.detail = Some(detail.clone());
            // A failed block, with a replay already answered: this is the one tab
            // whose text does not come from the pinned payload, and it still has
            // to keep the panel inside the window.
            state.failed = true;
            state.probe = BlockProbeState {
                loading: false,
                result: Some(TranslationBlockProbe {
                    ordinal: 0,
                    model: "chat-test".into(),
                    response_text: "{\"translations\":[{\"id\":0,\"text\":\"甲\"}]}".into(),
                    response_bytes: 39,
                    end: "done",
                    finish_reason: Some("stop"),
                    decoded_segments: Some(1),
                    decode_error_kind: None,
                    decode_error_line: None,
                    decode_error_column: None,
                    elapsed_ms: 1200,
                }),
                error: None,
            };
            cx.notify();
        });
    });
    redraw(visual);
    assert!(bounds(visual, "jobs-block-detail-panel").size.height > px(0.));
    let panel = bounds(visual, "jobs-block-detail-panel");
    let root = bounds(visual, "jobs-layout-root");
    assert!(
        panel.top() >= root.top() && panel.bottom() <= root.bottom(),
        "the panel must stay inside the window: panel={panel:?}, root={root:?}"
    );

    for tab in block_detail_tabs(true) {
        visual.update(|_, cx| {
            view.update(cx, |jobs, cx| {
                jobs.block_detail.as_mut().unwrap().tab = tab;
                cx.notify();
            });
        });
        redraw(visual);
        let text = visual.update(|_, cx| {
            let jobs = view.read(cx);
            block_detail_text(jobs.block_detail.as_ref().unwrap(), Some(&detail))
        });
        let text = text.expect("a loaded block always has text");
        assert!(!text.is_empty(), "{tab:?} rendered nothing");
        match tab {
            BlockDetailTab::Body => {
                // The body tab is the raw wire payload, so it has to carry both
                // messages and stay parseable.
                assert!(text.contains("系统提示词") && text.contains("用户内容"));
                assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
            }
            BlockDetailTab::Prompt => assert_eq!(text, detail.request.system_prompt),
            BlockDetailTab::Message => assert_eq!(text, detail.request.user_content),
            BlockDetailTab::Source => assert_eq!(text, detail.source_text),
            BlockDetailTab::Response => {
                assert!(text.contains("甲"), "the replayed answer is shown: {text}");
                assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
                // The replay panel is the body here, not the plain text block.
                assert!(bounds(visual, "jobs-block-probe-text").size.height > px(0.));
            }
            BlockDetailTab::Overview => {
                assert!(text.contains("chat-test"));
                assert!(text.contains("#0"));
                assert!(text.contains("中文（简体）"));
            }
        }
        // Every tab keeps the panel and its scroll body laid out.
        assert!(bounds(visual, "jobs-block-detail-body").size.height > px(0.));
        let panel = bounds(visual, "jobs-block-detail-panel");
        assert!(
            panel.top() >= root.top() && panel.bottom() <= root.bottom(),
            "{tab:?} pushed the panel out of the window: panel={panel:?}, root={root:?}"
        );
    }
    // The underlying list is still rendered, so the panel really is an overlay.
    assert!(bounds(visual, "jobs-layout-detail").size.height > px(0.));

    // Clicking the overlay outside the panel is a dismissal, not a block row
    // activation: the hitbox barrier must swallow the press.
    let overlay_point = point(root.left() + px(4.), root.center().y);
    visual.simulate_click(overlay_point, Modifiers::none());
    redraw(visual);
    visual.update(|_, cx| {
        assert!(
            view.read(cx).block_detail.is_none(),
            "a click outside the panel closes it"
        );
    });
    // Closing must restore the normal layout. `debug_bounds` reports the last
    // rendered frame, so the window is asserted through its regions instead of
    // the (still cached) overlay selector.
    assert_regions_fit(visual, 900., 640.);
}
