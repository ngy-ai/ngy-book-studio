//! The whole background-task layout.

use super::*;

#[gpui_kit::test]
pub(crate) fn complete_background_task_layout_keeps_all_controls_inside_both_window_sizes(
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
