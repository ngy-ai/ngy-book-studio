//! The rendered notes window.

use super::*;

impl Render for NotesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .text_color(rgb(palette(cx).ink))
            .text_sm()
            .bg(rgb(palette(cx).paper))
            .child(self.render_header(cx))
            .child(self.render_filters(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(palette(cx).danger))
                        .child(error),
                )
            })
            .child(self.render_list(window, cx))
            .child(self.render_footer(cx))
    }
}
