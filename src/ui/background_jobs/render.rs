//! The rendered jobs window.

use super::*;

impl Render for BackgroundJobsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_width = window.viewport_size().width;
        let detail_modal = self.render_block_detail_modal(window, cx);
        div()
            .debug_selector(|| "jobs-layout-root".into())
            .v_flex()
            .size_full()
            .overflow_hidden()
            .bg(rgb(palette(cx).paper))
            .text_color(rgb(palette(cx).ink))
            .child(self.render_header(cx))
            .when_some(self.refresh_error.as_ref(), |this, error| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .text_xs()
                        .bg(rgb(palette(cx).danger_soft))
                        .text_color(rgb(palette(cx).danger))
                        .child(error.clone()),
                )
            })
            .when_some(self.notice.as_ref(), |this, notice| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .text_xs()
                        .bg(rgb(if notice.error {
                            0xf8e2de
                        } else {
                            palette(cx).accent_soft
                        }))
                        .text_color(rgb(if notice.error {
                            palette(cx).danger
                        } else {
                            palette(cx).accent_strong
                        }))
                        .child(notice.text.clone()),
                )
            })
            .child(self.render_kinds_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(self.render_list_column(viewport_width, cx))
                    .child(self.render_resize_handle(cx))
                    .child(self.render_detail(cx)),
            )
            .children(detail_modal)
    }
}
