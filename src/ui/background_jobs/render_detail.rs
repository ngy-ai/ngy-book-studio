//! The detail pane.

use super::*;

impl BackgroundJobsWindow {
    pub(super) fn render_detail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(job) = self
            .jobs
            .iter()
            .find(|job| Some(&job.id) == self.selected_job_id.as_ref())
        else {
            return div()
                .debug_selector(|| "jobs-layout-detail".into())
                .v_flex()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .justify_center()
                .p_5()
                .text_sm()
                .text_color(rgb(palette(cx).muted))
                .child("选择任务查看运行情况。")
                .into_any_element();
        };
        // Built once per frame: the counters, the header's repair button, the copy
        // payload and the block rows all read the same view of the task.
        let inspector = self
            .blocks
            .as_ref()
            .map(|blocks| BlockInspector::new(job, blocks.total, self.logs.as_ref()));
        // A finished translation that skipped blocks offers a second repair route
        // in front of 重新翻译: re-send only the blocks that never got a
        // translation, after the user switched to a stronger model. The ordinals
        // are resolved here because a click handler must not borrow the per-frame
        // inspector, and the button is limited to the block tab because the failed
        // set is derived from that list.
        let failed_ordinals: Option<Vec<usize>> = if self.tab == DetailTab::Blocks {
            inspector.as_ref().and_then(|inspector| {
                let ordinals: Vec<usize> = (0..inspector.progress.total)
                    .filter(|index| inspector.state(*index) == BlockState::Failed)
                    .collect();
                (!ordinals.is_empty()).then_some(ordinals)
            })
        } else {
            None
        };
        let mut actions = div().h_flex().gap_1().flex_wrap();
        if let Some(ordinals) = failed_ordinals {
            actions = actions.child(
                Button::new("jobs-blocks-retry-failed")
                    .debug_selector(|| "jobs-blocks-retry-failed".into())
                    .small()
                    .outline()
                    .label(format!("重试失败块（{}）", ordinals.len()))
                    .disabled(self.pending_job_id.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.retry_translation_blocks(ordinals.clone(), window, cx);
                    })),
            );
        }
        for action in available_actions(job) {
            let id = job.id.clone();
            // The debug selector drops the job id so the layout test can name it
            // as a literal while it asserts where the repair button sits relative
            // to 重新翻译.
            let selector = format!("background-job-action-{}", action_id(action));
            actions = actions.child(
                Button::new(SharedString::from(format!(
                    "background-job-{}-{}",
                    job.id,
                    action_id(action)
                )))
                .debug_selector(move || selector)
                .small()
                .outline()
                .label(action_label(action))
                .disabled(self.pending_job_id.is_some())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_action(id.clone(), action, window, cx)
                })),
            );
        }
        let mut tabs = div().h_flex().gap_1();
        let mut tab_items = vec![
            (DetailTab::Summary, "任务详情"),
            (DetailTab::Logs, "运行日志"),
        ];
        if job.kind == "translation" {
            tab_items.push((DetailTab::Blocks, "文本块明细"));
        }
        for (tab, label) in tab_items {
            tabs = tabs.child(
                Button::new(SharedString::from(format!("jobs-tab-{}", tab_id(tab))))
                    .small()
                    .ghost()
                    .label(label)
                    .when(self.tab == tab, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(palette(cx).accent_soft).into())
                                .foreground(rgb(palette(cx).accent_strong).into()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.tab = tab;
                        this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                        this.refresh_logs(window, cx);
                        this.refresh_blocks(window, cx);
                        cx.notify();
                    })),
            );
        }
        let copy_text = match self.tab {
            DetailTab::Logs => self
                .logs
                .as_ref()
                .map(|logs| diagnostic_log_copy(job, logs)),
            DetailTab::Blocks => self.blocks.as_ref().and_then(|blocks| {
                inspector
                    .as_ref()
                    .map(|inspector| translation_blocks_copy(job, blocks, inspector))
            }),
            DetailTab::Summary => Some(diagnostic_summary(job)),
        };
        let copy_disabled = copy_text.is_none()
            || match self.tab {
                DetailTab::Logs => self.logs_error.is_some() || self.logs_loading,
                DetailTab::Blocks => self.blocks_loading,
                DetailTab::Summary => false,
            };
        let copy_view = cx.entity().clone();
        let header = div()
            .flex_none()
            .h_flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(tabs)
            .child(actions)
            .child(
                Button::new("jobs-copy-diagnostics")
                    .debug_selector(|| "jobs-copy-diagnostics".into())
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .label(match self.tab {
                        DetailTab::Logs => "复制日志",
                        DetailTab::Blocks => "复制概览",
                        DetailTab::Summary => "复制详情",
                    })
                    .disabled(copy_disabled)
                    .on_click(move |_, _, cx| {
                        if let Some(text) = &copy_text {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                            copy_view.update(cx, |this, cx| {
                                this.notice = Some(JobsNotice {
                                    text: "已复制所选任务的诊断信息。".into(),
                                    error: false,
                                });
                                cx.notify();
                            });
                        }
                    }),
            );
        let mut content = div()
            .id(SharedString::from(format!(
                "job-detail-{}-{}",
                job.id,
                tab_id(self.tab)
            )))
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .p_4()
            .gap_2()
            .text_xs();
        if self.tab == DetailTab::Blocks {
            for row in self.translation_block_rows(job, inspector.as_ref(), cx) {
                content = content.child(row);
            }
        } else if self.tab == DetailTab::Logs {
            content = content.child(
                div()
                    .text_color(rgb(palette(cx).muted))
                    .child("最新记录在前 · 时间为 UTC · ordinal 从 0 开始 · 复制按时间正序排列。"),
            );
            if let Some(error) = &self.logs_error {
                content = content.child(
                    div()
                        .text_color(rgb(palette(cx).danger))
                        .child(error.clone()),
                );
            }
            if let Some(logs) = &self.logs {
                if logs.truncated {
                    content = content.child(
                        div()
                            .text_color(rgb(palette(cx).muted))
                            .child("较早的记录已按保留上限清理，以下为最近的运行日志。"),
                    );
                }
                if logs.entries.is_empty() {
                    content = content.child(div().py_3().text_color(rgb(palette(cx).muted)).child(
                        "暂无保留的运行日志。任务可能尚未开始、在升级前执行，或记录已清理；后续运行会自动记录。",
                    ));
                }
                for entry in logs.entries.iter().rev() {
                    content = content.child(
                        div()
                            .flex_none()
                            .line_height(gpui::relative(1.5))
                            .border_b_1()
                            .border_color(rgb(palette(cx).border))
                            .py_1()
                            .child(entry.format_line()),
                    );
                }
            } else if self.logs_loading {
                content = content.child("正在读取日志…");
            }
        } else {
            for (label, value) in detail_rows(job) {
                content = content.child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .text_color(rgb(palette(cx).muted))
                                .child(label),
                        )
                        .child(div().flex_1().min_w(px(0.)).child(value)),
                );
            }
            if let Some(error) = &job.error {
                content = content.child(
                    div()
                        .p_2()
                        .rounded(px(6.))
                        .bg(rgb(palette(cx).danger_soft))
                        .text_color(rgb(palette(cx).danger))
                        .child(error.clone()),
                );
            }
            if let Some(cursor) = &job.cursor_json {
                content = content
                    .child(div().text_color(rgb(palette(cx).muted)).child("执行游标"))
                    .child(
                        div()
                            .p_2()
                            .bg(rgb(palette(cx).sidebar))
                            .child(prettify_cursor(cursor)),
                    );
            }
        }
        div()
            .debug_selector(|| "jobs-layout-detail".into())
            .v_flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .bg(rgb(palette(cx).surface))
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(content.size_full())
                    .vertical_scrollbar(&self.detail_scroll),
            )
            .into_any_element()
    }
}
