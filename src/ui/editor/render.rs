//! The document shell the WebView starts from and the small render helpers.

use super::*;

pub(crate) const DEFAULT_CHAPTER_HTML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>新章节</title></head>
  <body>
    <h2>新章节</h2>
    <p>在这里输入正文内容…</p>
  </body>
</html>"#;

/// Keeps each sizeable GPUI builder on its own call frame. In unoptimized
/// Windows builds a single monolithic editor render previously reserved almost
/// the entire 1 MiB main-thread stack before GPUI started layout.
#[inline(never)]
pub(crate) fn render_editor_section(render: impl FnOnce() -> gpui::AnyElement) -> gpui::AnyElement {
    render()
}

#[inline(never)]
pub(crate) fn render_editor_resize_handle(
    id: &'static str,
    pane: EditorResizablePane,
    view: Entity<EditorApp>,
    theme: Palette,
) -> gpui::AnyElement {
    div()
        .id(id)
        .h_full()
        .w(px(EDITOR_RESIZE_HANDLE_WIDTH))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_col_resize()
        .hover(|this| this.bg(rgb(theme.accent_soft)))
        .on_drag(EditorPaneResizeDrag(pane), move |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| EditorPaneResizePreview)
        })
        .on_drag_move::<EditorPaneResizeDrag>(move |event, window, cx| {
            if event.drag(cx).0 != pane {
                return;
            }
            let pointer_x = event.event.position.x;
            let viewport_width = window.viewport_size().width;
            view.update(cx, |this, cx| {
                this.resize_pane_from_pointer(pane, pointer_x, viewport_width, cx);
            });
        })
        .child(div().h_full().w(px(1.)).bg(rgb(theme.border)))
        .into_any_element()
}
