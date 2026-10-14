//! Exercise the production message renderer through layout and mouse/clipboard
//! events. The GPUI test platform does not cover native fonts or OS clipboard.

use super::{AiMessageRole, message_text_width, selectable_message_text};
use crate::ui::init_ui_test;
use gpui_kit::component::{Root, StyledExt};
use gpui_kit::{
    AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement, Modifiers, MouseButton,
    ParentElement, Pixels, Point, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext, Window, div, point, px,
};
use std::time::Duration;

const TEXT_ID: &str = "ai-markdown-message-regression";
const PROBE_SELECTOR: &str = "ai-markdown-message-probe";
const VIEWPORT_HEIGHT: f32 = 180.;
const BUBBLE_TEXT_SELECTOR: &str = "ai-markdown-bubble-text-probe";

const LIST_MESSAGE: &str = "- FIRST_ITEM\n- Second longer item includes enough words to wrap within the narrow answer bubble.\n\n1. READ_SOURCES\n2. CHECK_SOURCES";

struct BubbleProbe {
    width: Pixels,
}

impl Render for BubbleProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the production message card: testing an unconstrained TextView
        // alone misses nested flex measurement defects. The action row (copy,
        // edit, regenerate) lives below the bubble in production, outside the
        // text column, so the probe must not reserve a button column either.
        div().w(self.width).h(px(400.)).child(
            div()
                .v_flex()
                .w_full()
                .min_w(px(0.))
                .max_w((self.width - px(24.)).max(px(0.)))
                .gap_2()
                .px_3()
                .py_2()
                .border_1()
                .text_sm()
                .child(
                    div()
                        .h_flex()
                        .min_w(px(0.))
                        .items_start()
                        .line_height(gpui_kit::relative(1.5))
                        .child(
                            div()
                                .id(BUBBLE_TEXT_SELECTOR)
                                .debug_selector(|| BUBBLE_TEXT_SELECTOR.into())
                                .min_w(px(0.))
                                .flex_1()
                                .child(selectable_message_text(
                                    "ai-markdown-bubble-list-message",
                                    AiMessageRole::Assistant,
                                    LIST_MESSAGE,
                                    message_text_width(self.width, window.rem_size()),
                                    window,
                                    cx,
                                )),
                        ),
                ),
        )
    }
}

struct MessageProbe {
    role: AiMessageRole,
    content: String,
    scroll_handle: ScrollHandle,
}

impl Render for MessageProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = selectable_message_text(TEXT_ID, self.role, &self.content, px(360.), window, cx);
        // Match the conversation's outer scroller: the production TextView must
        // retain its natural height so selection moves with the actual text.
        div()
            .id(PROBE_SELECTOR)
            .debug_selector(|| PROBE_SELECTOR.into())
            .w(px(360.))
            .h(px(VIEWPORT_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .text_size(px(16.))
            .line_height(px(24.))
            .child(text)
    }
}

fn open_probe<'a>(
    cx: &'a mut TestAppContext,
    role: AiMessageRole,
    content: &str,
) -> (Entity<MessageProbe>, &'a mut VisualTestContext) {
    init_ui_test(cx);
    let mut probe = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| MessageProbe {
            role,
            content: content.to_owned(),
            scroll_handle: ScrollHandle::default(),
        });
        probe = Some(view.clone());
        Root::new(view, window, cx)
    });
    redraw(visual);
    (
        probe.expect("the root must contain a message probe"),
        visual,
    )
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    cx.run_until_parked();
}

fn probe_bounds(cx: &mut VisualTestContext) -> Bounds<Pixels> {
    cx.debug_bounds(PROBE_SELECTOR)
        .expect("the message viewport must have been laid out")
}

fn select(cx: &mut VisualTestContext, start: Point<Pixels>, end: Point<Pixels>) {
    cx.simulate_mouse_move(start, None, Modifiers::none());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    // TextView installs drag/up listeners after its selecting state is painted.
    redraw(cx);
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
}

fn copy_selected(cx: &mut VisualTestContext) -> String {
    // 0.6 的 TextView 把拖动选择放在窗口级的 `TextSelection` 里，键盘 Copy 的
    // 分发需要原生键盘输入路径（无头测试窗口没有），所以这里直接读窗口选择：
    // 断言的内容仍是「拖动选中的正文」，与生产里的复制内容同源。
    let copied = cx.update(gpui_kit::base::TextSelection::selected_text);
    assert!(!copied.is_empty(), "selection must contain message text");
    copied
}

fn select_short_message(cx: &mut VisualTestContext) -> String {
    let origin = probe_bounds(cx).origin;
    select(
        cx,
        origin + point(px(1.), px(1.)),
        origin + point(px(350.), px(170.)),
    );
    copy_selected(cx)
}

/// 全选消息正文并返回选中文本。
///
/// 0.6 的窗口选择只在 pointer down 命中文字参与者时才开始，代码块与列表的
/// 缩进/内边距让固定像素坐标很脆弱（落在字符内部会丢掉半个词）；`ctrl-a`
/// 走 TextView 自己的 `input::SelectAll` 动作，验证「内容进入窗口选择」这一
/// 同一不变量。拖动的命中与跟随滚动仍由 `assistant_markdown_selection_
/// survives_outer_scroll_and_reselection` 覆盖。
fn select_all_message(cx: &mut VisualTestContext, bounds: Bounds<Pixels>) -> String {
    // 先点一下正文内缩的一点：0.6 的 `ctrl-a` 绑定在 TextView 的 key context
    // 上，需要焦点；内缩坐标避开列表与代码块的内边距/缩进，落在文字上。
    let target = bounds.origin + point(px(24.), px(14.));
    cx.simulate_mouse_down(target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    redraw(cx);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    redraw(cx);
    copy_selected(cx)
}

fn wheel(cx: &mut VisualTestContext, delta_y: f32) {
    let position = probe_bounds(cx).origin + point(px(160.), px(90.));
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.), px(delta_y))),
        ..Default::default()
    });
    redraw(cx);
    cx.simulate_mouse_move(position + point(px(1.), px(1.)), None, Modifiers::none());
    redraw(cx);
}

#[gpui_kit::test]
fn assistant_markdown_selection_renders_syntax_while_user_selection_keeps_it(
    cx: &mut TestAppContext,
) {
    let content = "# TITLE\n**BOLD**";
    let (_, assistant) = open_probe(cx, AiMessageRole::Assistant, content);
    // 0.6 的窗口选择按块拼接，每个选中的块以换行结尾（旧版剪贴板会去掉
    // 最后一个换行），正文本身与语法渲染规则不变。
    assert_eq!(select_short_message(assistant), "TITLE\nBOLD\n");

    let (_, user) = open_probe(cx, AiMessageRole::User, content);
    // The existing HTML path represents each line as a <p>. Its parsed
    // Root/Paragraph boundaries add a blank line during selection copy; verify
    // that exact existing output, including the literal Markdown punctuation.
    // 0.6 的窗口选择还会为每个选中的块补一个结尾换行，所以末尾多出两行。
    assert_eq!(select_short_message(user), "# TITLE\n\n**BOLD**\n\n\n");
}

#[gpui_kit::test]
fn streaming_message_updates_same_id_from_open_fence_to_complete_markdown(cx: &mut TestAppContext) {
    let (probe, cx) = open_probe(cx, AiMessageRole::Assistant, "```\nFIRST = 1");
    let bounds = probe_bounds(cx);
    let single = select_all_message(cx, bounds);
    assert!(single.contains("FIRST = 1"), "{single:?}");

    probe.update(cx, |probe, cx| {
        probe.content = "```\nFIRST = 1\nSECOND = 2\n```\n\n**COMPLETE**".into();
        cx.notify();
    });
    redraw(cx);
    // The TextView parses changes on a real background task; advancing the GPUI
    // deterministic clock cannot trigger it. First drain the update queue, then
    // let that real task finish before checking the displayed content.
    std::thread::sleep(Duration::from_millis(250));
    redraw(cx);

    let bounds = probe_bounds(cx);
    let copied = select_all_message(cx, bounds);
    assert!(copied.contains("FIRST = 1\nSECOND = 2"), "{copied:?}");
    assert!(copied.contains("COMPLETE"), "{copied:?}");
    assert!(!copied.contains("```"), "{copied:?}");
    assert!(!copied.contains("**"), "{copied:?}");
}

#[gpui_kit::test]
fn assistant_markdown_selection_survives_outer_scroll_and_reselection(cx: &mut TestAppContext) {
    let mut content = "**ORIGINAL**\n\n".to_owned();
    for row in 0..40 {
        content.push_str(&format!("**ROW_{row:02}_unique_text**\n\n"));
    }
    let (probe, cx) = open_probe(cx, AiMessageRole::Assistant, &content);
    let origin = probe_bounds(cx).origin;
    let start = origin + point(px(1.), px(12.));
    let end = origin + point(px(330.), px(12.));
    select(cx, start, end);
    assert_eq!(copy_selected(cx), "ORIGINAL\n");

    wheel(cx, -384.);
    assert!(probe.read_with(cx, |probe, _| probe.scroll_handle.offset().y) < px(-VIEWPORT_HEIGHT));
    assert_eq!(copy_selected(cx), "ORIGINAL\n");

    select(cx, start, end);
    let scrolled = copy_selected(cx);
    assert!(scrolled.starts_with("ROW_"), "{scrolled:?}");
    assert!(!scrolled.contains("ORIGINAL"), "{scrolled:?}");
    assert!(!scrolled.contains("**"), "{scrolled:?}");

    wheel(cx, 10_000.);
    assert_eq!(
        probe.read_with(cx, |probe, _| probe.scroll_handle.offset().y),
        px(0.)
    );
    assert_eq!(copy_selected(cx), scrolled);
    select(cx, start, end);
    assert_eq!(copy_selected(cx), "ORIGINAL\n");
}

#[gpui_kit::test]
fn narrow_answer_bubble_keeps_short_and_wrapped_list_item_text_selectable(cx: &mut TestAppContext) {
    init_ui_test(cx);
    let mut probe = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| BubbleProbe { width: px(280.) });
        probe = Some(view.clone());
        Root::new(view, window, cx)
    });
    let probe = probe.expect("the root must contain a message bubble");
    // Preserve the entity and message ID while widening and narrowing the
    // sidebar, so the same cached TextView must reflow and remain selectable.
    for width in [280., 360., 280.] {
        probe.update(visual, |probe, cx| {
            probe.width = px(width);
            cx.notify();
        });
        redraw(visual);
        let bounds = visual
            .debug_bounds(BUBBLE_TEXT_SELECTOR)
            .expect("the message text column must have been laid out");
        assert!(bounds.size.width > px(150.), "{width}: {bounds:?}");
        assert!(bounds.size.height > px(60.), "{width}: {bounds:?}");
        assert!(bounds.bottom() < px(400.), "{width}: {bounds:?}");

        // Drag only inside the message's text column. The clipboard payload
        // must come from painted, selectable list body text, never from a
        // message action button's stored source.
        let copied = select_all_message(visual, bounds);
        for expected in [
            "FIRST_ITEM",
            "Second longer item",
            "narrow answer bubble.",
            "READ_SOURCES",
            "CHECK_SOURCES",
        ] {
            assert!(
                copied.contains(expected),
                "answer width {width} lost list text {expected:?}: {copied:?}"
            );
        }
    }
}
