//! The trace table, the history list and the environment pane.

use super::*;

impl LearningWindow {
    #[inline(never)]
    pub(super) fn render_trace(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.trace.is_empty() {
            return div()
                .p_4()
                .text_sm()
                .text_color(rgb(palette(cx).muted))
                .child(if self.running {
                    "正在等待执行器的实际事件…"
                } else {
                    "当前没有可展示的执行轨迹。"
                })
                .into_any_element();
        }
        let selected = self
            .trace_selected
            .unwrap_or(self.trace.len() - 1)
            .min(self.trace.len() - 1);
        let mut events = div()
            .id("learning-trace-event-list")
            .v_flex()
            .h(px(170.))
            .min_h(px(0.))
            .gap_1()
            .overflow_y_scroll();
        for (index, event) in self.trace.iter().enumerate() {
            let view = cx.entity();
            events = events.child(
                Button::new(SharedString::from(format!("learning-trace-event-{index}")))
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .label(format!(
                        "{} · {}",
                        self.omitted_traces + index + 1,
                        short_text(&event.label, 36)
                    ))
                    .when(index == selected, |button| {
                        button.bg(rgb(palette(cx).accent_soft))
                    })
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.trace_selected = Some(index);
                            cx.notify();
                        });
                    }),
            );
        }
        let previous_view = cx.entity();
        let next_view = cx.entity();
        let event = &self.trace[selected];
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_3()
            .when(self.omitted_traces > 0, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child(format!(
                            "当前显示最近 {MAX_VISIBLE_TRACES} 条；完整报告保留更早事件。"
                        )),
                )
            })
            .child(events)
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("learning-trace-previous")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label("上一步")
                            .disabled(selected == 0)
                            .on_click(move |_, _, cx| {
                                previous_view.update(cx, |this, cx| {
                                    this.trace_selected = Some(selected.saturating_sub(1));
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("learning-trace-next")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronRight)
                            .label("下一步")
                            .disabled(selected + 1 >= self.trace.len())
                            .on_click(move |_, _, cx| {
                                next_view.update(cx, |this, cx| {
                                    this.trace_selected = Some(
                                        (selected + 1).min(this.trace.len().saturating_sub(1)),
                                    );
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(div().text_sm().font_semibold().child(event.label.clone()))
            .child(div().flex_1().min_h(px(0.)).child(scrollable_learning_text(
                "learning-trace-detail",
                fenced_text(&event.detail, "text"),
                window,
                cx,
            )))
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_history(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("learning-history-scroll")
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_3()
            .overflow_y_scroll();
        if self.history.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(rgb(palette(cx).muted))
                    .child("尚无运行历史。第一次运行后会保留当时的代码、预测与结果。"),
            );
        }
        for (index, entry) in self.history.iter().enumerate() {
            let view = cx.entity();
            let id = entry.id.clone();
            let selected = self.report.as_ref().is_some_and(|report| report.id == id);
            list = list.child(
                div()
                    .id(SharedString::from(format!("learning-history-{}", entry.id)))
                    .v_flex()
                    .gap_1p5()
                    .p_3()
                    .border_1()
                    .border_color(rgb(if selected {
                        palette(cx).accent
                    } else {
                        palette(cx).border
                    }))
                    .rounded(px(8.))
                    .bg(rgb(palette(cx).surface))
                    .child(div().text_sm().font_semibold().child(format!(
                        "{} · {}",
                        implementation_label(self.service.chapter(), &entry.implementation),
                        scenario_label(self.service.chapter(), &entry.scenario)
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child(run_label(&entry.status, entry.passed, entry.imported)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child(record_time(&entry.created_at)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("learning-history-open-{index}")))
                            .outline()
                            .small()
                            .label("查看结果与轨迹")
                            .disabled(self.busy())
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.load_history(id.clone(), window, cx)
                                });
                            }),
                    ),
            );
        }
        list.into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_environment(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(environment) = &self.environment else {
            return div().into_any_element();
        };
        let url = environment.install_url.clone();
        let refresh_view = cx.entity();
        div()
            .v_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(rgb(palette(cx).border))
            .text_xs()
            .text_color(rgb(palette(cx).muted))
            .child(div().font_semibold().child(if environment.ready {
                "运行环境"
            } else {
                "运行环境尚未就绪"
            }))
            .child(environment.message.clone())
            .when(!environment.ready, |panel| {
                panel.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .when_some(url, |row, url| {
                            row.child(
                                Button::new("learning-install-help")
                                    .outline()
                                    .small()
                                    .icon(IconName::ExternalLink)
                                    .label("官方安装说明")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                        })
                        .child(
                            Button::new("learning-env-recheck")
                                .ghost()
                                .small()
                                .label("重新检测")
                                .disabled(self.busy())
                                .on_click(move |_, window, cx| {
                                    refresh_view.update(cx, |this, cx| this.load(window, cx));
                                }),
                        ),
                )
            })
            .into_any_element()
    }
}
