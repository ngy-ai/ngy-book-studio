//! The rendered library window, the delete confirmation body and the window
//! removal helper.

use super::*;

impl Render for EpubReaderApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.library_mutations.close_requested() {
            return div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(rgb(palette(cx).paper))
                .text_color(rgb(palette(cx).ink))
                .child(div().font_semibold().child("正在关闭墨页…"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette(cx).muted))
                        .child("正在完成已提交的书库操作，完成后将自动关闭。"),
                )
                .into_any_element();
        }
        div()
            .relative()
            .size_full()
            .child(self.render_library(cx))
            .when_some(self.render_group_modal(cx), |this, modal| this.child(modal))
            .into_any_element()
    }
}

/// Shared confirmation body for the destructive "delete group" and
/// "delete book" modals.
pub(crate) fn render_delete_confirm_body(
    description: String,
    view: &Entity<EpubReaderApp>,
    theme: Palette,
) -> gpui::AnyElement {
    let cancel_view = view.clone();
    let confirm_view = view.clone();
    div()
        .v_flex()
        .gap_4()
        .child(
            div()
                .text_sm()
                .text_color(rgb(theme.ink))
                .line_height(gpui::relative(1.6))
                .child(description),
        )
        .child(
            div()
                .h_flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("modal-delete-cancel")
                        .label("取消")
                        .outline()
                        .on_click(move |_, _, cx| {
                            cancel_view.update(cx, |this, cx| this.close_group_modal(cx));
                        }),
                )
                .child(
                    Button::new("modal-delete-confirm")
                        .label("删除")
                        .danger()
                        .on_click(move |_, _, cx| {
                            confirm_view.update(cx, |this, cx| this.confirm_group_modal(cx));
                        }),
                ),
        )
        .into_any_element()
}

pub(crate) fn schedule_library_window_removal(
    window_handle: gpui::AnyWindowHandle,
    cx: &mut gpui::AsyncApp,
) {
    if let Err(error) = window_handle.update(cx, |_, window, cx| {
        finish_library_window_close(window, cx);
    }) {
        tracing::warn!(%error, "cannot schedule library window removal after mutation completion");
    }
}

pub fn wrap_root(app: Entity<EpubReaderApp>, window: &mut Window, cx: &mut Context<Root>) -> Root {
    // Confirm before changing any window state, then close children through
    // their ordinary save/progress/WebView barriers. The library goes last.
    let close_app = app.downgrade();
    let close_main: WindowCloseHandler = Rc::new(move |window, cx| {
        match close_app.update(cx, |app, cx| app.request_window_close(cx)) {
            Ok(true) => finish_library_window_close(window, cx),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "library state unavailable during close request");
                finish_library_window_close(window, cx);
            }
        }
    });
    window.on_window_should_close(cx, move |window, cx| {
        request_application_exit(window, cx, Rc::clone(&close_main));
        false
    });
    Root::new(app, window, cx)
}
