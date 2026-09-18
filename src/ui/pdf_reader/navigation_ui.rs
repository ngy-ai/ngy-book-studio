//! The collapsed navigation rail and the pane resize handle.

use super::*;

impl PdfReaderApp {
    pub(crate) fn toggle_navigation_collapsed(&mut self, cx: &mut Context<Self>) {
        if self.pane_layout.toggle_navigation_collapsed() {
            cx.notify();
        }
    }

    /// Narrow rail shown while the page list is collapsed. The reader keeps the
    /// resized width so expanding restores the previous layout.
    pub(crate) fn render_collapsed_navigation(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        div()
            .v_flex()
            .w(px(READER_NAVIGATION_COLLAPSED_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .border_r_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).sidebar))
            .py_3()
            .child(
                Button::new("pdf-navigation-expand")
                    .ghost()
                    .icon(IconName::PanelLeftOpen)
                    .tooltip("展开页面列表")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle_navigation_collapsed(cx));
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .text_color(rgb(palette(cx).accent))
                    .child(Icon::new(IconName::Menu).small()),
            )
            .into_any_element()
    }

    pub(crate) fn render_pane_resize_handle(
        &self,
        pane: ReaderResizablePane,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let drag_view = view.clone();
        let handle_id = match pane {
            ReaderResizablePane::Navigation => "pdf-navigation-resize-handle",
            ReaderResizablePane::Ai => "pdf-ai-resize-handle",
        };
        let active = self.pane_layout.is_resizing(pane);

        div()
            .id(handle_id)
            .h_full()
            .w(px(READER_PANE_RESIZE_HANDLE_WIDTH))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .bg(rgb(palette(cx).surface))
            .child(
                div()
                    .h_full()
                    .w(px(if active { 2. } else { 1. }))
                    .bg(rgb(if active {
                        palette(cx).accent
                    } else {
                        palette(cx).border
                    })),
            )
            .hover(|this| this.bg(rgb(palette(cx).accent_soft)))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<PdfPaneResizeDrag>, window, cx| {
                    if event.drag(cx).0 != pane {
                        return;
                    }
                    if this.pane_layout.resize_from_pointer(
                        pane,
                        event.event.position.x,
                        window.viewport_size().width,
                        this.ai_sidebar_collapsed,
                    ) {
                        if pane == ReaderResizablePane::Ai {
                            let width = this.pane_layout.ai_width();
                            this.ai_sidebar
                                .update(cx, |sidebar, cx| sidebar.set_expanded_width(width, cx));
                        }
                        cx.notify();
                    }
                },
            ))
            .on_drag(
                PdfPaneResizeDrag(pane),
                move |_: &PdfPaneResizeDrag, _, _, cx| {
                    cx.stop_propagation();
                    drag_view.update(cx, |this, cx| {
                        if this.pane_layout.begin_resize(pane) {
                            cx.notify();
                        }
                    });
                    cx.new(|_| EmptyView)
                },
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.pane_layout.finish_resize() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.pane_layout.finish_resize() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }
}
