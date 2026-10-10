//! The page text projection, zoom maths and the wheel listener.

use super::*;

/// 一段文字最多多少字符：超过就按句末标点切成多段。
pub(crate) const MAX_PAGE_TEXT_PARAGRAPH_CHARS: usize = 480;

/// 把一页纯文本切成段落。显示与朗读共用这一份切分：段落索引就是朗读块 id，
/// 高亮按同一个索引对齐。DjVu 的隐藏文字层常把一整页塞成一行，所以超过一段
/// 上限的行再按句末标点切开——既不让一段朗读太久，也不越过共享层的单段上限。
pub(crate) fn page_text_paragraphs(text: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.chars().count() <= MAX_PAGE_TEXT_PARAGRAPH_CHARS {
            paragraphs.push(line.to_string());
            continue;
        }
        paragraphs.extend(split_long_paragraph(line));
    }
    paragraphs
}

/// 句子结束的标点。不放英文句点：小数与缩写会被切得支离破碎。
fn is_sentence_end(character: char) -> bool {
    matches!(character, '。' | '！' | '？' | '!' | '?' | ';' | '；' | '…')
}

fn split_long_paragraph(line: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut rest = line;
    while rest.chars().count() > MAX_PAGE_TEXT_PARAGRAPH_CHARS {
        let mut cut = 0usize;
        let mut sentence_end = 0usize;
        for (units, (offset, character)) in rest.char_indices().enumerate() {
            if units >= MAX_PAGE_TEXT_PARAGRAPH_CHARS {
                break;
            }
            cut = offset + character.len_utf8();
            if is_sentence_end(character) {
                sentence_end = cut;
            }
        }
        // 句末标点太靠前（不到一半）就不用它，避免切出很短的一段。
        if sentence_end < cut / 2 {
            sentence_end = cut;
        }
        paragraphs.push(rest[..sentence_end].trim().to_string());
        rest = rest[sentence_end..].trim_start();
    }
    if !rest.is_empty() {
        paragraphs.push(rest.to_string());
    }
    paragraphs.retain(|paragraph| !paragraph.is_empty());
    paragraphs
}

/// Escapes a page's plain text into the HTML subset `TextView` renders.
///
/// `highlight` 是当前朗读段的索引：给它包一个 `<mark>`（组件层把 `<mark>` 折成
/// 高亮标记），面板因此能跟着朗读走，不需要任何新 API。
pub(crate) fn page_text_html(
    text: &str,
    has_hidden_text: bool,
    highlight: Option<usize>,
) -> String {
    if !has_hidden_text {
        return "<p>本页没有文字层；页面图片仅供阅读，不参与搜索或 AI 引用。</p>".to_string();
    }
    let mut html = String::with_capacity(text.len() + 16);
    for (index, paragraph) in page_text_paragraphs(text).iter().enumerate() {
        html.push_str("<p>");
        let marked = highlight == Some(index);
        if marked {
            html.push_str("<mark>");
        }
        for character in paragraph.chars() {
            match character {
                '&' => html.push_str("&amp;"),
                '<' => html.push_str("&lt;"),
                '>' => html.push_str("&gt;"),
                other => html.push(other),
            }
        }
        if marked {
            html.push_str("</mark>");
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
/// The TextView's selection geometry must follow the text it paints, so its
/// internal virtual list stays disabled and the whole view moves with the
/// outer scroll container instead.
pub(crate) fn scrollable_page_text(
    id: SharedString,
    html: SharedString,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let scroll_handle = window
        .use_keyed_state(SharedString::from(format!("{id}/scroll")), cx, |_, _| {
            gpui_kit::ScrollHandle::default()
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
                    TextView::html(id, html)
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
