//! 语速档位弹层的几何约束与交互。
//!
//! 阅读窗口的正文是原生子窗口 WebView，它画在 GPUI 画布之上：落进正文区域的浮层会被
//! 整块盖住。这条测试用与阅读窗口同尺寸的顶栏（工具栏 66px + 进度条 3px + 正文卡片
//! 上边距 12px）复现生产控件，断言弹层整行都留在正文之上。GPUI 测试平台画不出原生
//! WebView，真机上「正文是否遮住它」这一条只能靠人工冒烟补。

use super::*;
use crate::ui::init_ui_test;
use gpui_kit::component::Root;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

/// 阅读窗口正文之上的固定高度：工具栏 66 + 进度条 3 + 正文卡片上边距 12（`render.rs`
/// 与 `docs/agent/webview-and-window-constraints.md` 第 14 条是同一组数字）。
const TOOLBAR_HEIGHT: f32 = 66.;
const PROGRESS_HEIGHT: f32 = 3.;
const CONTENT_PADDING: f32 = 12.;
const WEBVIEW_TOP: f32 = TOOLBAR_HEIGHT + PROGRESS_HEIGHT + CONTENT_PADDING;

const TRIGGER: &str = "reader-read-aloud-rate-trigger";
const MENU: &str = "reader-read-aloud-rate-menu";
const BODY: &str = "rate-menu-test-body";
const OPTION_SELECTORS: [&str; SPEECH_RATE_PRESETS.len()] = [
    "reader-read-aloud-rate-option-0",
    "reader-read-aloud-rate-option-1",
    "reader-read-aloud-rate-option-2",
    "reader-read-aloud-rate-option-3",
    "reader-read-aloud-rate-option-4",
];

struct Harness {
    current: f32,
    open: bool,
    selected: Vec<f32>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open_view = cx.entity();
        let select_view = open_view.clone();
        div()
            .v_flex()
            .size_full()
            .child(
                div()
                    .h_flex()
                    .h(px(TOOLBAR_HEIGHT))
                    .items_center()
                    .px_5()
                    .child(speech_rate_menu(
                        self.current,
                        self.open,
                        move |open, cx| {
                            open_view.update(cx, |this, cx| {
                                this.open = open;
                                cx.notify();
                            });
                        },
                        move |preset, cx| {
                            select_view.update(cx, |this, cx| {
                                this.selected.push(preset);
                                this.current = preset;
                                this.open = false;
                                cx.notify();
                            });
                        },
                    )),
            )
            .child(div().h(px(PROGRESS_HEIGHT)))
            .child(div().h(px(CONTENT_PADDING)))
            .child(
                div()
                    .id(BODY)
                    .debug_selector(|| BODY.into())
                    .flex_1()
                    .bg(rgb(0xffffff)),
            )
    }
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    cx.run_until_parked();
}

fn open_harness(cx: &mut TestAppContext) -> (Entity<Harness>, &mut VisualTestContext) {
    init_ui_test(cx);
    let mut harness = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| Harness {
            current: 1.0,
            open: false,
            selected: Vec::new(),
        });
        harness = Some(view.clone());
        Root::new(view, window, cx)
    });
    redraw(visual);
    (
        harness.expect("the test root must contain the rate-menu harness"),
        visual,
    )
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} 必须渲染出来"));
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    cx.simulate_click(bounds.center(), Modifiers::none());
    redraw(cx);
}

/// 弹层必须整行留在工具栏里：正文区域在真机上是原生子窗口 WebView，伸进去的部分用户
/// 看不到，也就是这次回归的原始症状。
#[gpui_kit::test]
fn the_rate_menu_stays_above_the_webview_area(cx: &mut TestAppContext) {
    let (_harness, visual) = open_harness(cx);
    assert!(
        visual.debug_bounds(MENU).is_none(),
        "档位弹层初始必须是收起的"
    );
    assert!(visual.debug_bounds(TRIGGER).is_some(), "按钮要一直显示当前档位");

    click(visual, TRIGGER);
    let menu = visual
        .debug_bounds(MENU)
        .expect("点击档位按钮后必须展开一行档位");

    assert!(
        menu.bottom() <= px(WEBVIEW_TOP),
        "档位弹层不能伸进正文区域，正文 WebView 会把它盖住：{menu:?}"
    );
    assert!(menu.top() >= px(0.), "档位弹层不能跑到窗口外：{menu:?}");
    assert!(
        menu.size.height <= px(40.),
        "档位是横向一行，高度不能超过一行按钮：{menu:?}"
    );

    // 五个档位同排：行内每一项都落在弹层的纵向范围内，且都在窗口宽度以内。
    let viewport = visual.update(|window, _| window.viewport_size().width);
    for selector in OPTION_SELECTORS {
        let option = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} 必须渲染出来"));
        assert!(
            option.top() >= menu.top() && option.bottom() <= menu.bottom(),
            "档位 {selector} 必须和其余档位同一行：{option:?}"
        );
        assert!(option.right() <= viewport, "档位 {selector} 超出了窗口");
    }
}

#[gpui_kit::test]
fn choosing_a_preset_reports_it_and_closes_the_menu(cx: &mut TestAppContext) {
    let (harness, visual) = open_harness(cx);
    click(visual, TRIGGER);
    click(visual, OPTION_SELECTORS[3]);

    visual.update(|_, cx| {
        let harness = harness.read(cx);
        assert_eq!(harness.selected, vec![SPEECH_RATE_PRESETS[3]]);
        assert!(!harness.open, "选中档位后弹层必须收起");
    });
    assert!(visual.debug_bounds(MENU).is_none());
}

/// GPUI 画布内的「点外面」也要能关掉弹层。注意正文区域在真机上归 WebView，那里的点击
/// 到不了 GPUI，所以宿主还在 `on_select` 里显式收起。
#[gpui_kit::test]
fn clicking_outside_closes_the_menu(cx: &mut TestAppContext) {
    let (harness, visual) = open_harness(cx);
    click(visual, TRIGGER);
    assert!(visual.debug_bounds(MENU).is_some());

    let body = visual.debug_bounds(BODY).expect("正文占位必须渲染出来");
    visual.simulate_click(body.center(), Modifiers::none());
    redraw(visual);
    assert!(visual.debug_bounds(MENU).is_none());
    visual.update(|_, cx| assert!(!harness.read(cx).open));
}
