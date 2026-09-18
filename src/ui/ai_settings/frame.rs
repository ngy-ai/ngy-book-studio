//! The shared settings frame.

use super::*;

/// 五个设置窗口共用的外框：标题栏、可滚动正文、提示条与底部保存栏。
pub(crate) fn settings_frame<V: SettingsWindowView>(
    view: &V,
    body: gpui::AnyElement,
    footer_extra: Option<gpui::AnyElement>,
    cx: &mut Context<V>,
) -> gpui::AnyElement {
    let section = V::section();
    let state = view.state();
    let busy = state.busy();
    let scroll_handle = state.scroll_handle.clone();
    let notice = state.notice().cloned();
    let save_label = if state.operation == PendingOperation::Saving {
        "正在保存…"
    } else {
        "保存设置"
    };

    div()
        .v_flex()
        .size_full()
        .bg(rgb(palette(cx).paper))
        .child(
            div()
                .h_flex()
                .flex_none()
                .justify_between()
                .gap_3()
                .px_6()
                .py_4()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
                .child(
                    div()
                        .h_flex()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size_9()
                                .rounded(px(10.))
                                .bg(rgb(palette(cx).accent_soft))
                                .text_color(rgb(palette(cx).accent))
                                .child(Icon::new(section.icon())),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_semibold()
                                        .text_color(rgb(palette(cx).ink))
                                        .child(format!("AI 设置 · {}", section.label())),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(palette(cx).muted))
                                        .child(section.subtitle()),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child(section.header_note()),
                ),
        )
        .child(
            div()
                .id(SharedString::from(section.selector("scroll")))
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .child(div().p_6().child(body)),
        )
        .when_some(notice, |this, notice| {
            this.child(
                div()
                    .flex_none()
                    .px_6()
                    .py_2()
                    .child(render_notice(&notice, palette(cx))),
            )
        })
        .child(
            div()
                .h_flex()
                .flex_none()
                .justify_between()
                .gap_3()
                .px_6()
                .py_4()
                .border_t_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
                .child(
                    div()
                        .max_w(px(440.))
                        .text_xs()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(palette(cx).muted))
                        .child("保存只写入本窗口的设置；其它设置窗口里未保存的输入不会被覆盖。"),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .when_some(footer_extra, |this, extra| this.child(extra))
                        .child(
                            Button::new(SharedString::from(section.selector("save-settings")))
                                .debug_selector(section.debug_selector("save-settings"))
                                .primary()
                                .icon(IconName::Check)
                                .label(save_label)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                        ),
                ),
        )
        .into_any_element()
}
