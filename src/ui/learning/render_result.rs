//! The run result and the attempt list.

use super::*;

impl LearningWindow {
    #[inline(never)]
    pub(super) fn render_result(
        &self,
        // 0.6 的 TextView 在构造时不再需要 window。
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(report) = &self.report else {
            return div()
                .v_flex()
                .gap_3()
                .p_4()
                .text_sm()
                .text_color(rgb(palette(cx).muted))
                .child(if self.running {
                    "正在执行你的代码…"
                } else {
                    "这里还没有运行结果。"
                })
                .child("填写预测并运行代码后，这里会展示实际结果、调用计数和停止原因。")
                .child("代码通过检查与学会了什么，将分别记录。")
                .into_any_element();
        };
        let color = if report.imported {
            palette(cx).muted
        } else if report.passed {
            0x376441
        } else {
            palette(cx).danger
        };
        let mut checks = div().v_flex().gap_1p5();
        if let Some(items) = report
            .raw
            .get("checks")
            .and_then(serde_json::Value::as_array)
        {
            for item in items.iter().take(24) {
                let passed = item.get("passed").and_then(serde_json::Value::as_bool) == Some(true);
                let detail = item
                    .get("detail")
                    .or_else(|| item.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("运行检查");
                let prefix = if report.imported {
                    "原记录"
                } else if passed {
                    "通过"
                } else {
                    "未通过"
                };
                checks = checks.child(
                    div()
                        .text_xs()
                        .text_color(rgb(if report.imported || passed {
                            palette(cx).muted
                        } else {
                            palette(cx).danger
                        }))
                        .child(format!("{prefix} · {detail}")),
                );
            }
        }
        let answer = if report.answer.trim().is_empty() {
            "本次没有可提交的最终答案。"
        } else {
            &report.answer
        };
        div()
            .id("learning-result-scroll")
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_3()
            .p_4()
            .overflow_y_scroll()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .text_color(rgb(color))
                    .child(div().font_semibold().child(run_label(&report.status, report.passed, report.imported)))
                    .child(div().text_xs().child(format!("停止原因：{}", stop_label(self.service.chapter(), &report.stop_reason)))),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!("记录 {}", short_text(&report.id, 14)))
                    .child(format!("记录时间：{}", record_time(&report.created_at))),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1p5()
                    .p_3()
                    .rounded(px(8.))
                    .bg(rgb(palette(cx).paper))
                    .text_sm()
                    .child(format!("{}：{}", if self.service.chapter() == 1 { "模型决策" } else { "练习输入获取" }, report.metrics.model_decisions))
                    .child(format!("实际工具执行：{}", report.metrics.actual_tool_executions))
                    .child(format!("框架节点标注（学员）：{}", report.metrics.framework_steps))
                    .child(format!("耗时：{:.2} 秒", report.metrics.elapsed_seconds)),
            )
            .child(div().text_sm().font_semibold().child("最终答案"))
            .child(TextView::markdown("learning-answer-text", fenced_text(answer, "json")).selectable(true))
            .when_some(report.raw.pointer("/error/message").and_then(serde_json::Value::as_str), |panel, error| {
                panel.child(div().text_sm().font_semibold().child("运行诊断"))
                    .child(TextView::markdown("learning-runtime-error", fenced_text(error, "text")).selectable(true))
            })
            .child(checks)
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .p_3()
                    .rounded(px(8.))
                    .bg(rgb(palette(cx).accent_soft))
                    .text_xs()
                    .text_color(rgb(palette(cx).accent_strong))
                    .child("学习证据：待评阅")
                    .child(if report.imported {
                        "这份报告来自导入备份，其历史结果和来源尚未重新核验。"
                    } else {
                        "运行结果只说明本次代码表现。理解、独立完成和迁移需要结合首次作答与复测判断。"
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_attempt(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(report) = &self.report else {
            return div()
                .p_4()
                .text_sm()
                .text_color(rgb(palette(cx).muted))
                .child("选择一条历史记录，或运行一次代码后，查看当时保存的代码、预测和学习笔记。")
                .into_any_element();
        };
        let workspace = &report.workspace;
        let help = HELP_LEVELS
            .iter()
            .find(|(id, _)| *id == workspace.help_level)
            .map_or("帮助级别待核验", |(_, label)| *label);
        let prediction = if workspace.prediction.trim().is_empty() {
            "本次未填写运行前预测。"
        } else {
            &workspace.prediction
        };
        let notes = if workspace.notes.trim().is_empty() {
            "本次未填写学习笔记。"
        } else {
            &workspace.notes
        };
        let code = if workspace.implementation == "langgraph" {
            &workspace.langgraph_code
        } else {
            &workspace.manual_code
        };
        let markdown = format!(
            "### 运行前预测\n\n{}\n\n### 当时的学习笔记\n\n{}\n\n### 本次运行代码\n\n{}",
            fenced_text(prediction, "text"),
            fenced_text(notes, "text"),
            fenced_text(code, CODE_LANGUAGE),
        );
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_3()
            .child(div().text_sm().font_semibold().child("本次作答快照 · 只读"))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!(
                        "{} · {} · {}",
                        implementation_label(self.service.chapter(), &workspace.implementation),
                        scenario_label(self.service.chapter(), &workspace.scenario),
                        help,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(if report.imported {
                        "导入记录 · 待核验。这里展示备份保存的作答，不能证明当时独立完成。"
                    } else {
                        "这里保留提交运行时的作答；当前编辑区的后续修改不会改写这份快照。"
                    }),
            )
            .child(div().flex_1().min_h(px(0.)).child(scrollable_learning_text(
                "learning-attempt-text",
                markdown,
                window,
                cx,
            )))
            .into_any_element()
    }
}
