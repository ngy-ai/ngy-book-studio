//! The block probe body.

use super::*;

impl BackgroundJobsWindow {
    /// Panel of the model-response tab: one on-demand replay of the open failed
    /// block, its raw answer, and the worker's own verdict on that answer.
    ///
    /// This is the only place in the window that sends a model request, so it
    /// runs exclusively on the button below — never on a poll, never on open —
    /// and the button is disabled while a replay is in flight.
    pub(super) fn render_block_probe(
        &self,
        probe: &BlockProbeState,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut panel =
            div()
                .v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .child(div().text_xs().text_color(rgb(palette(cx).muted)).child(
                "失败块才有这一页：诊断日志只保存固定分类，所以这里按同一份冻结原文重新请求一次，\
                 把模型返回的原文原样显示出来。这次请求不写译文、不推进任务、不改任何记录。",
            ));
        panel = panel.child(
            div().h_flex().flex_none().items_center().gap_2().child(
                Button::new("jobs-block-probe-run")
                    .debug_selector(|| "jobs-block-probe-run".into())
                    .small()
                    .outline()
                    .icon(IconName::Redo)
                    .label(if probe.loading {
                        "正在请求模型…"
                    } else if probe.result.is_some() {
                        "再请求一次"
                    } else {
                        "重新请求一次"
                    })
                    .disabled(probe.loading)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.probe_block_response(window, cx);
                    })),
            ),
        );
        if probe.loading {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("已发出请求，正在等待模型返回；响应会先在这里显示，不会写进书库。"),
            );
        }
        if let Some(error) = probe.error.as_ref() {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).danger))
                    .child(error.clone()),
            );
        }
        let Some(result) = probe.result.as_ref() else {
            return panel.into_any_element();
        };
        panel = panel
            .child(
                div()
                    .flex_none()
                    .h(px(1.))
                    .w_full()
                    .bg(rgb(palette(cx).border)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!(
                        "调用模型 {} · 耗时 {:.1} 秒 · 收到 {} 字节 · {}",
                        result.model,
                        result.elapsed_ms as f64 / 1000.,
                        result.response_bytes,
                        probe_end_label(result)
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if result.decode_error_kind.is_some() {
                        rgb(palette(cx).danger)
                    } else {
                        rgb(palette(cx).success)
                    })
                    .child(probe_decode_label(result)),
            );
        let body = if result.response_text.is_empty() {
            div()
                .text_xs()
                .text_color(rgb(palette(cx).muted))
                .child("（模型没有返回任何内容）")
                .into_any_element()
        } else {
            div()
                .id("jobs-block-probe-text")
                .debug_selector(|| "jobs-block-probe-text".into())
                .w_full()
                .flex_none()
                .text_xs()
                .line_height(gpui::relative(1.5))
                .text_color(rgb(palette(cx).ink))
                .whitespace_normal()
                .child(prettify_json(&result.response_text))
                .into_any_element()
        };
        panel.child(body).into_any_element()
    }
}
