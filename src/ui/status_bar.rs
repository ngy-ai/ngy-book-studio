//! The shared status bar.

use super::*;

pub(crate) fn render_status_bar(
    status_icon: IconName,
    status_text: String,
    status_color: u32,
    summary: String,
    context: String,
    palette: Palette,
) -> gpui::AnyElement {
    div()
        .h_flex()
        .h(px(STATUS_BAR_HEIGHT))
        .flex_none()
        .items_center()
        .justify_between()
        .gap_4()
        .px_5()
        .border_t_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_xs()
        .text_color(rgb(palette.muted))
        .child(
            div()
                .h_flex()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .gap_3()
                .child(
                    div()
                        .h_flex()
                        .flex_none()
                        .gap_1p5()
                        .text_color(rgb(status_color))
                        .child(Icon::new(status_icon).small())
                        .child(status_text),
                )
                .child(
                    div()
                        .w(px(1.))
                        .h(px(12.))
                        .flex_none()
                        .bg(rgb(palette.border)),
                )
                .child(div().min_w(px(0.)).truncate().child(summary)),
        )
        .child(
            div()
                .h_flex()
                .max_w(px(460.))
                .min_w(px(0.))
                .flex_none()
                .gap_3()
                .child(div().min_w(px(0.)).truncate().child(context))
                .child(
                    div()
                        .w(px(1.))
                        .h(px(12.))
                        .flex_none()
                        .bg(rgb(palette.border)),
                )
                .child(
                    div()
                        .flex_none()
                        .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                ),
        )
        .into_any_element()
}
