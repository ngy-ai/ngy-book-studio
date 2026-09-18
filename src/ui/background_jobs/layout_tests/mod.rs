//! Render the complete production window through GPUI layout and mouse input.
//! Synthetic task snapshots isolate layout from provider and worker timing;
//! services use a fresh local directory and in-memory credentials only.

use super::*;
use gpui::{
    Modifiers, Pixels, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext, point,
};
use ngy_book_studio::{
    ai::credentials::MemoryCredentialStore,
    ai::job_diagnostics::{BackgroundJobLogEntry, JobLogLevel, JobLogMetrics},
    services::{BackgroundJobProgress, TranslationBlockRequest},
};

mod blocks_test;

mod detail_test;

mod interaction_test;

mod layout_test;

pub(crate) fn task_id(index: usize) -> String {
    format!("translation:layout-source-{index:04}-0123456789abcdef:zh-Hans")
}

pub(crate) fn snapshots() -> Vec<BackgroundJobSnapshot> {
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
pub(crate) fn block_list(total: usize) -> TranslationBlockList {
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

pub(crate) fn open_fixture(
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

pub(crate) fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

pub(crate) fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing rendered region {selector}"))
}

pub(crate) fn assert_regions_fit(cx: &mut VisualTestContext, width: f32, height: f32) {
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

/// One persisted diagnostic line, reduced to what the inspector reads.
pub(crate) fn log_entry(
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

pub(crate) fn log_snapshot(entries: Vec<BackgroundJobLogEntry>) -> BackgroundJobLogSnapshot {
    BackgroundJobLogSnapshot {
        entries,
        truncated: false,
    }
}
