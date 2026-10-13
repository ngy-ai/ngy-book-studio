//! 阅读窗口工具栏里的朗读语速档位。
//!
//! 正文是原生子窗口 WebView，它画在 GPUI 画布之上：落进正文区域的 GPUI 浮层会被它整块
//! 盖住（`deferred` 层也一样，不会报错，只是看不见）。阅读窗口顶部只有「工具栏 66px +
//! 进度条 3px + 正文卡片上边距 12px」这条缝能完整显示浮层。所以档位是**一行**按钮，并用
//! `Anchor::BottomLeft` 把弹层放在按钮上沿——它被窗口边缘钳制后整个留在工具栏里，竖向下
//! 拉那样一路铺到正文上只看得到两三行。约束的完整说明见
//! `docs/agent/webview-and-window-constraints.md` 第 14 条。

use super::*;

/// 阅读窗口工具栏里的语速档位（受控弹层）。
///
/// 宿主自己持有展开状态与当前值：`on_open_change` 同步展开状态，`on_select` 在选中档位时
/// 被调用。正文区域的点击归 WebView，弹层收不到，所以「选完即收」必须由宿主在
/// `on_select` 里做。
pub(crate) fn speech_rate_menu(
    current_rate: f32,
    open: bool,
    on_open_change: impl Fn(bool, &mut App) + 'static,
    on_select: impl Fn(f32, &mut App) + 'static,
) -> Popover {
    let on_select: Rc<dyn Fn(f32, &mut App)> = Rc::new(on_select);
    Popover::new("reader-read-aloud-rate")
        .anchor(Anchor::BottomLeft)
        .p_1()
        .open(open)
        .on_open_change(move |open, _, cx| on_open_change(*open, cx))
        .trigger(
            Button::new("reader-read-aloud-rate-trigger")
                .debug_selector(|| "reader-read-aloud-rate-trigger".into())
                .ghost()
                .xsmall()
                .label(speech_rate_label(current_rate))
                .tooltip("朗读语速：改的是系统配置里的全局档位，对之后的朗读生效"),
        )
        .content(move |_, _, _| {
            let mut options = div()
                .debug_selector(|| "reader-read-aloud-rate-menu".into())
                .h_flex()
                .gap_1();
            for (index, preset) in SPEECH_RATE_PRESETS.into_iter().enumerate() {
                let on_select = on_select.clone();
                options = options.child(
                    Button::new(("reader-read-aloud-rate-option", index))
                        .debug_selector({
                            let selector = format!("reader-read-aloud-rate-option-{index}");
                            move || selector
                        })
                        .ghost()
                        .xsmall()
                        .selected((preset - current_rate).abs() < 0.01)
                        .label(speech_rate_label(preset))
                        .on_click(move |_, _, cx| on_select(preset, cx)),
                );
            }
            options
        })
}
