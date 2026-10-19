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

/// 第一行只有书名：状态胶囊、进度与元信息都在第二行。书名区因此一直
/// 用到行右边缘（只让展开箭头占位），不会被右侧任何一列挤窄；胶囊与
/// 书名区右边对齐。曾经胶囊单独占一列时会提前截断书名并留出空白（现场
/// 2026-10-19），这里用两个区域的水平边界守住。
#[gpui_kit::test]
pub(crate) fn job_row_title_owns_the_whole_first_line(cx: &mut TestAppContext) {
    let (_directory, _view, visual) = open_fixture(cx);
    visual.simulate_resize(size(px(1180.), px(820.)));
    redraw(visual);
    let list = bounds(visual, "jobs-layout-list");
    let title = bounds(visual, "jobs-row-title");
    let status = bounds(visual, "jobs-row-status");
    // 胶囊在第二行末尾，右边缘和书名区右边缘对齐。
    assert!(
        f32::from(status.right() - title.right()).abs() <= 0.5,
        "状态胶囊没有和书名区右边对齐：title={title:?}, status={status:?}"
    );
    // 书名区之外只剩行内边距、gap_3 与展开箭头，宽度远超半行。
    assert!(
        title.size.width > list.size.width / 2.,
        "书名区仍然被右侧列挤窄：list={list:?}, title={title:?}"
    );
    assert!(
        title.left() > list.left() && title.right() < list.right(),
        "书名区跑出了列表：list={list:?}, title={title:?}"
    );
}
