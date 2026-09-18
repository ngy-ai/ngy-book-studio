//! Exercise the production message renderer through layout and mouse/clipboard
//! events. The GPUI test platform does not cover native fonts or OS clipboard.

use super::{AiMessageRole, message_text_width, selectable_message_text};
use gpui::{
    AppContext, Bounds, ClipboardItem, Context, Entity, InteractiveElement, IntoElement, Modifiers,
    MouseButton, ParentElement, Pixels, Point, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext, Window, div, point, px,
};
use gpui_component::{Root, StyledExt, clipboard::Clipboard};
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
        // Keep the production message card and clipboard sibling: testing an
        // unconstrained TextView alone misses nested flex measurement defects.
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
                        .gap_1()
                        .line_height(gpui::relative(1.5))
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
                        )
                        .child(
                            div().w(px(24.)).flex_shrink_0().child(
                                Clipboard::new("ai-markdown-bubble-copy").value(LIST_MESSAGE),
                            ),
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
    cx.update(gpui_component::init);
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
    cx.update(|window, app| window.draw(app).clear());
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
    const SENTINEL: &str = "message-copy-action-was-not-dispatched";
    cx.write_to_clipboard(ClipboardItem::new_string(SENTINEL.into()));
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-c");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-c");
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("the copy action must write text to the test clipboard");
    assert_ne!(copied, SENTINEL, "the focused message must handle Copy");
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

#[gpui::test]
fn assistant_markdown_selection_renders_syntax_while_user_selection_keeps_it(
    cx: &mut TestAppContext,
) {
    let content = "# TITLE\n**BOLD**";
    let (_, assistant) = open_probe(cx, AiMessageRole::Assistant, content);
    assert_eq!(select_short_message(assistant), "TITLE\nBOLD");

    let (_, user) = open_probe(cx, AiMessageRole::User, content);
    // The existing HTML path represents each line as a <p>. Its parsed
    // Root/Paragraph boundaries add a blank line during selection copy; verify
    // that exact existing output, including the literal Markdown punctuation.
    assert_eq!(select_short_message(user), "# TITLE\n\n**BOLD**");
}

#[gpui::test]
fn streaming_message_updates_same_id_from_open_fence_to_complete_markdown(cx: &mut TestAppContext) {
    let (probe, cx) = open_probe(cx, AiMessageRole::Assistant, "```\nFIRST = 1");
    assert_eq!(select_short_message(cx), "FIRST = 1");

    probe.update(cx, |probe, cx| {
        probe.content = "```\nFIRST = 1\nSECOND = 2\n```\n\n**COMPLETE**".into();
        cx.notify();
    });
    redraw(cx);
    // TextView 0.5.1 debounces changes with a real 200 ms Timer; advancing the
    // GPUI deterministic clock cannot trigger it. First drain the update queue,
    // then allow that real timer to fire before checking the displayed content.
    std::thread::sleep(Duration::from_millis(250));
    redraw(cx);

    let copied = select_short_message(cx);
    assert!(copied.contains("FIRST = 1\nSECOND = 2"), "{copied:?}");
    assert!(copied.contains("COMPLETE"), "{copied:?}");
    assert!(!copied.contains("```"), "{copied:?}");
    assert!(!copied.contains("**"), "{copied:?}");
}

#[gpui::test]
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
    assert_eq!(copy_selected(cx), "ORIGINAL");

    wheel(cx, -384.);
    assert!(probe.read_with(cx, |probe, _| probe.scroll_handle.offset().y) < px(-VIEWPORT_HEIGHT));
    assert_eq!(copy_selected(cx), "ORIGINAL");

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
    assert_eq!(copy_selected(cx), "ORIGINAL");
}

#[gpui::test]
fn narrow_answer_bubble_keeps_short_and_wrapped_list_item_text_selectable(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
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

        // Drag only in the message's text column, never over Clipboard. The
        // clipboard payload must come from painted, selectable list body text,
        // rather than the separate full-message copy button's stored source.
        select(
            visual,
            bounds.origin + point(px(1.), px(1.)),
            point(bounds.right() - px(1.), bounds.bottom() - px(1.)),
        );
        let copied = copy_selected(visual);
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
