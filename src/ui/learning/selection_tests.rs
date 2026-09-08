//! Exercise the production text element through GPUI layout and input dispatch.
//! The test platform uses deterministic font metrics and an in-memory clipboard;
//! these tests do not claim to exercise native HWND, DirectWrite or OS clipboard.

use super::scrollable_learning_text;
use gpui::{
    AppContext, Bounds, ClipboardItem, Context, Entity, InteractiveElement, IntoElement, Modifiers,
    MouseButton, ParentElement, Pixels, Point, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    SharedString, Styled, TestAppContext, VisualTestContext, Window, div, point, px,
};
use gpui_component::Root;

const TEXT_ID: &str = "learning-selection-regression";
const PROBE_SELECTOR: &str = "learning-selection-probe";
const VIEWPORT_HEIGHT: f32 = 180.;

struct SelectionProbe {
    markdown: SharedString,
    scroll_handle: Option<ScrollHandle>,
}

impl Render for SelectionProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = scrollable_learning_text(TEXT_ID, self.markdown.clone(), window, cx);
        // Observe the production helper's keyed handle. All scroll changes below
        // come from wheel events; the test never assigns an offset or selection.
        self.scroll_handle = Some(
            window
                .use_keyed_state(
                    SharedString::from(format!("{TEXT_ID}/scroll")),
                    cx,
                    |_, _| ScrollHandle::default(),
                )
                .read(cx)
                .clone(),
        );
        div()
            .id(PROBE_SELECTOR)
            .debug_selector(|| PROBE_SELECTOR.into())
            .w(px(360.))
            .h(px(VIEWPORT_HEIGHT))
            .text_size(px(16.))
            .line_height(px(24.))
            .child(text)
    }
}

fn fixture_text(prefix: &str) -> SharedString {
    let mut markdown = format!("{prefix}\n\n");
    for row in 0..40 {
        markdown.push_str(&format!("ROW_{row:02}_unique_text\n\n"));
    }
    markdown.into()
}

fn open_probe(
    cx: &mut TestAppContext,
    markdown: SharedString,
) -> (Entity<SelectionProbe>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut probe = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| SelectionProbe {
            markdown,
            scroll_handle: None,
        });
        probe = Some(view.clone());
        Root::new(view, window, cx)
    });
    redraw(visual);
    (
        probe.expect("the test root must contain a text probe"),
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
        .expect("the production text viewport must have been laid out")
}

fn select(cx: &mut VisualTestContext, start: Point<Pixels>, end: Point<Pixels>) {
    // TextView installs the move/up listeners only after its selecting state has
    // been painted. Keep real event boundaries rather than editing that state.
    cx.simulate_mouse_move(start, None, Modifiers::none());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    redraw(cx);
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
}

fn copy_selected(cx: &mut VisualTestContext) -> String {
    const SENTINEL: &str = "copy-action-was-not-dispatched";
    cx.write_to_clipboard(ClipboardItem::new_string(SENTINEL.into()));
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-c");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-c");
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("the copy action must write text to the test clipboard");
    assert_ne!(copied, SENTINEL, "the focused TextView must handle Copy");
    assert!(!copied.is_empty(), "selection must contain actual text");
    copied
}

fn wheel(cx: &mut VisualTestContext, delta_y: f32) {
    let bounds = probe_bounds(cx);
    let position = bounds.origin + point(px(160.), px(90.));
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.), px(delta_y))),
        ..Default::default()
    });
    redraw(cx);
    // A passive move after mouse-up must not continue the earlier selection.
    cx.simulate_mouse_move(position + point(px(1.), px(1.)), None, Modifiers::none());
    redraw(cx);
}

fn offset(probe: &Entity<SelectionProbe>, cx: &VisualTestContext) -> Point<Pixels> {
    probe.read_with(cx, |probe, _| {
        probe
            .scroll_handle
            .as_ref()
            .expect("production helper must retain its scroll handle")
            .offset()
    })
}

#[gpui::test]
fn selected_text_survives_wheel_out_of_view_and_reselection(cx: &mut TestAppContext) {
    let (probe, cx) = open_probe(cx, fixture_text("ORIGINAL_A\n\nORIGINAL_B"));
    let origin = probe_bounds(cx).origin;
    let start = origin + point(px(1.), px(12.));
    let end = origin + point(px(300.), px(12.));
    select(cx, start, end);
    assert_eq!(copy_selected(cx), "ORIGINAL_A");

    wheel(cx, -72.);
    assert_eq!(copy_selected(cx), "ORIGINAL_A");
    assert!(offset(&probe, cx).y < px(0.), "wheel must really scroll");

    wheel(cx, -360.);
    assert!(
        offset(&probe, cx).y < px(-VIEWPORT_HEIGHT),
        "the initial selection must now be outside the viewport"
    );
    assert_eq!(copy_selected(cx), "ORIGINAL_A");

    // Select the text now occupying the same screen coordinates. It must be a
    // different source paragraph, not a stale copy of the first selection.
    select(cx, start, end);
    let scrolled_selection = copy_selected(cx);
    assert!(
        scrolled_selection.starts_with("ROW_"),
        "expected a newly visible source row, got {scrolled_selection:?}"
    );
    assert!(!scrolled_selection.contains("ORIGINAL_"));

    wheel(cx, 10_000.);
    assert_eq!(offset(&probe, cx).y, px(0.));
    assert_eq!(copy_selected(cx), scrolled_selection);

    select(
        cx,
        origin + point(px(1.), px(36.)),
        origin + point(px(300.), px(36.)),
    );
    assert_eq!(copy_selected(cx), "ORIGINAL_B");
}

#[gpui::test]
fn multiline_selection_survives_scroll_and_reverse_drag(cx: &mut TestAppContext) {
    let (probe, cx) = open_probe(cx, fixture_text("FIRST_PARAGRAPH\n\nSECOND_PARAGRAPH"));
    let origin = probe_bounds(cx).origin;
    let start = origin + point(px(1.), px(12.));
    let end = origin + point(px(300.), px(36.));
    select(cx, start, end);
    let expected = "FIRST_PARAGRAPH\nSECOND_PARAGRAPH";
    assert_eq!(copy_selected(cx), expected);
    wheel(cx, -360.);
    assert!(offset(&probe, cx).y < px(-VIEWPORT_HEIGHT));
    assert_eq!(copy_selected(cx), expected);
    wheel(cx, 10_000.);
    assert_eq!(offset(&probe, cx).y, px(0.));
    select(cx, end, start);
    assert_eq!(copy_selected(cx), expected);
}

#[gpui::test]
fn code_block_selection_survives_scroll(cx: &mut TestAppContext) {
    // An unlabelled fence exercises the actual Markdown code-block element
    // without requiring a language registration or external syntax assets.
    let (probe, cx) = open_probe(
        cx,
        fixture_text("```\nFIRST_LINE = 1\nSECOND_LINE = 2\n```"),
    );
    let origin = probe_bounds(cx).origin;
    select(
        cx,
        origin + point(px(1.), px(1.)),
        origin + point(px(330.), px(65.)),
    );
    let expected = "FIRST_LINE = 1\nSECOND_LINE = 2";
    assert_eq!(copy_selected(cx), expected);
    wheel(cx, -360.);
    assert!(offset(&probe, cx).y < px(-VIEWPORT_HEIGHT));
    assert_eq!(copy_selected(cx), expected);
    wheel(cx, 10_000.);
    assert_eq!(offset(&probe, cx).y, px(0.));
    assert_eq!(copy_selected(cx), expected);
}
