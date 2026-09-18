//! The page text projection, zoom maths and the wheel listener.

use super::*;

/// Escapes a page's plain text into the HTML subset `TextView` renders.
pub(crate) fn page_text_html(text: &str, has_hidden_text: bool) -> String {
    if !has_hidden_text {
        return "<p>本页没有文字层；页面图片仅供阅读，不参与搜索或 AI 引用。</p>".to_string();
    }
    let mut html = String::with_capacity(text.len() + 16);
    for paragraph in text.split('\n') {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        html.push_str("<p>");
        for character in paragraph.chars() {
            match character {
                '&' => html.push_str("&amp;"),
                '<' => html.push_str("&lt;"),
                '>' => html.push_str("&gt;"),
                other => html.push(other),
            }
        }
        html.push_str("</p>");
    }
    html
}

pub(crate) fn page_text_preview(text: &str) -> String {
    let mut preview = String::new();
    for character in text.chars().take(MAX_PAGE_TEXT_CHARS) {
        preview.push(character);
    }
    preview
}

pub(crate) fn zoomed_size(width: u32, height: u32, zoom_milli: u32) -> (f32, f32) {
    let scale = zoom_milli as f32 / 1_000.0;
    (
        (width as f32 * scale).max(1.0),
        (height as f32 * scale).max(1.0),
    )
}

/// One Ctrl + wheel notch moves the page by one zoom step.
///
/// `ZOOM_FIT` is the reader's own "fit the window", not a size to step from: a
/// page smaller than the window would only add empty margin, so scrolling down
/// there stays put while scrolling up leaves it at 100%.
pub(crate) fn zoom_after_wheel(zoom_milli: u32, zoom_in: bool) -> u32 {
    if zoom_milli == ZOOM_FIT {
        return if zoom_in {
            ZOOM_MIN_MILLI * 4
        } else {
            ZOOM_FIT
        };
    }
    if zoom_in {
        zoom_milli
            .saturating_add(ZOOM_STEP_MILLI)
            .min(ZOOM_MAX_MILLI)
    } else {
        let next = zoom_milli.saturating_sub(ZOOM_STEP_MILLI);
        if next < ZOOM_MIN_MILLI {
            ZOOM_FIT
        } else {
            next
        }
    }
}

/// Ctrl + wheel over the page.
///
/// A plain notch is left to the reader so it keeps scrolling the page; with
/// Ctrl held the notch becomes one zoom step and is swallowed, so a scroller
/// under the pointer cannot also scroll by the same amount.
pub(crate) fn zoom_wheel_listener(
    app: Entity<PageImageApp>,
) -> impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if !event.modifiers.control {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height()).y;
        if f32::from(delta) == 0.0 {
            return;
        }
        cx.stop_propagation();
        app.update(cx, |this, cx| {
            this.zoom_from_wheel(f32::from(delta) > 0.0, cx);
        });
    }
}

/// A selectable, naturally sized page text view inside an outer scroller.
///
/// `TextView` 0.5.1 keeps selection endpoints in its own bounds, so its
/// internal virtual list must stay disabled and the whole view has to move
/// with the outer scroll container.
pub(crate) fn scrollable_page_text(
    id: SharedString,
    html: SharedString,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let scroll_handle = window
        .use_keyed_state(SharedString::from(format!("{id}/scroll")), cx, |_, _| {
            gpui::ScrollHandle::default()
        })
        .read(cx)
        .clone();
    div()
        .id(SharedString::from(format!("{id}/viewport")))
        .relative()
        .flex_1()
        .min_h(px(0.))
        .child(
            div()
                .id(SharedString::from(format!("{id}/body")))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .p_3()
                .child(
                    TextView::html(id, html, window, cx)
                        .style(TextViewStyle::default().paragraph_gap(rems(0.5)))
                        .selectable(true)
                        .scrollable(false)
                        .w(px(PAGE_TEXT_PANEL_WIDTH - 40.))
                        .h_auto()
                        .flex_shrink_0(),
                ),
        )
        .vertical_scrollbar(&scroll_handle)
        .into_any_element()
}
