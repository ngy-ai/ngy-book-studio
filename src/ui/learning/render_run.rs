//! The run controls and the evidence tabs.

use super::*;

impl LearningWindow {
    #[inline(never)]
    pub(super) fn render_run_controls(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let scenario_view = cx.entity();
        let selected_scenario = self.scenario.clone();
        let chapter = self.service.chapter();
        let run_view = cx.entity();
        let cancel_view = cx.entity();
        let disabled = self.inputs_disabled();
        let can_run = !disabled && self.environment.as_ref().is_some_and(|env| env.ready);
        let scenario_button = Button::new("learning-scenario")
            .outline()
            .small()
            .label(scenario_label(chapter, &self.scenario).to_string())
            .icon(IconName::ChevronDown)
            .disabled(disabled)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for &(id, label) in chapter_scenarios(chapter) {
                    let view = scenario_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(selected_scenario == id)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if !this.inputs_disabled() {
                                        this.scenario = id.to_string();
                                        cx.notify();
                                    }
                                });
                            }),
                    );
                }
                menu
            });
        let mut implementations = div().h_flex().gap_1();
        for id in ["manual", "langgraph"] {
            let label = implementation_label(chapter, id);
            let view = cx.entity();
            implementations = implementations.child(
                Button::new(SharedString::from(format!("learning-implementation-{id}")))
                    .ghost()
                    .small()
                    .label(label)
                    .when(self.implementation == id, |button| {
                        button
                            .bg(rgb(palette(cx).accent_soft))
                            .text_color(rgb(palette(cx).accent_strong))
                    })
                    .disabled(disabled)
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if !this.inputs_disabled() {
                                this.implementation = id.to_string();
                                cx.notify();
                            }
                        });
                    }),
            );
        }
        let mut row = div()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(implementations)
            .child(scenario_button)
            .child(
                Button::new("learning-run")
                    .primary()
                    .small()
                    .icon(IconName::SquareTerminal)
                    .label(if self.running {
                        "运行中…"
                    } else {
                        "运行我的代码"
                    })
                    .disabled(!can_run)
                    .on_click(move |_, window, cx| {
                        run_view.update(cx, |this, cx| this.start_run(window, cx));
                    }),
            );
        if self.running {
            row = row.child(
                Button::new("learning-cancel")
                    .debug_selector(|| "learning-cancel".into())
                    .outline()
                    .small()
                    .label("取消")
                    .disabled(
                        self.cancellation
                            .as_ref()
                            .is_some_and(|token| token.load(Ordering::Acquire)),
                    )
                    .on_click(move |_, _, cx| {
                        cancel_view.update(cx, |this, cx| this.cancel_run(cx));
                    }),
            );
        }
        div()
            .v_flex()
            .flex_none()
            .gap_1p5()
            .px_3()
            .py_3()
            .border_t_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).surface))
            .child(row)
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(if chapter == 1 {
                        "本章使用固定模型响应与模拟只读工具，让两种实现面对相同任务。"
                    } else {
                        "使用运行时生成的虚构练习数据，未调用真实大模型。读取和模拟操作由宿主记录并检查。"
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_evidence_tabs(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let mut tabs = div().h_flex().gap_1().px_2().py_2();
        for (id, label, pane) in [
            ("learning-result-tab", "结果", EvidencePane::Result),
            ("learning-trace-tab", "逐步轨迹", EvidencePane::Trace),
            ("learning-attempt-tab", "本次作答", EvidencePane::Attempt),
            ("learning-history-tab", "历史", EvidencePane::History),
        ] {
            let view = cx.entity();
            tabs = tabs.child(
                Button::new(id)
                    .ghost()
                    .label(label)
                    .when(self.evidence_pane == pane, |button| {
                        button
                            .bg(rgb(palette(cx).accent_soft))
                            .text_color(rgb(palette(cx).accent_strong))
                    })
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.evidence_pane = pane;
                            cx.notify();
                        });
                    }),
            );
        }
        tabs.border_b_1()
            .border_color(rgb(palette(cx).border))
            .into_any_element()
    }
}
