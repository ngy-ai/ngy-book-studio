//! Pagination, dividers and the bulk retry affordance.

use super::*;

#[gpui::test]
pub(crate) fn pagination_clicks_select_the_current_page_after_filter_changes(
    cx: &mut TestAppContext,
) {
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
pub(crate) fn divider_drag_clamps_the_list_column_and_keeps_the_detail_readable(
    cx: &mut TestAppContext,
) {
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
pub(crate) fn failed_blocks_offer_a_bulk_retry_before_retranslate_and_a_row_retry(
    cx: &mut TestAppContext,
) {
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
