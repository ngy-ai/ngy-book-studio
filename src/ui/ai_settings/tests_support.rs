//! 五个设置窗口共用的测试脚手架。
//!
//! 每个测试都拿一个真实的 `AppServices`（临时数据目录 + 内存凭据库）把窗口装进
//! `VisualTestContext`，因此可以验证「草稿 → 保存」的真实路径。

use super::*;

use gpui_kit::{Focusable as _, Modifiers, TestAppContext, VisualTestContext};
use ngy_book_studio::ai::credentials::MemoryCredentialStore;

pub(crate) fn open_settings_view<V: SettingsWindowView>(
    cx: &mut TestAppContext,
    build: impl FnOnce(Arc<AppServices>, ProviderSettings, &mut Window, &mut Context<V>) -> V + 'static,
) -> (tempfile::TempDir, Entity<V>, &mut VisualTestContext) {
    let directory = tempfile::tempdir().unwrap();
    let services = Arc::new(
        AppServices::open_with_credentials(
            directory.path(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap(),
    );
    let settings = services.provider_settings().unwrap();
    init_ui_test(cx);
    let mut settings_view = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| build(services, settings, window, cx));
        settings_view = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1080.), px(1400.)));
    redraw(visual);
    (directory, settings_view.unwrap(), visual)
}

pub(crate) fn redraw(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
}

pub(crate) fn edit_input(visual: &mut VisualTestContext, input: &Entity<InputState>, text: &str) {
    visual.update(|window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx));
    });
    redraw(visual);
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    visual.simulate_input(text);
    redraw(visual);
    input.read_with(visual, |input, _| assert_eq!(input.value().as_ref(), text));
    visual.update(|window, cx| assert!(input.read(cx).focus_handle(cx).is_focused(window)));
}

/// 不经过键盘、直接改写输入框内容，用于只需要一个新值的场景。
pub(crate) fn set_input_value(
    visual: &mut VisualTestContext,
    input: &Entity<InputState>,
    text: &str,
) {
    let input = input.clone();
    let text = text.to_string();
    visual.update(|window, cx| {
        input.update(cx, |input, cx| input.set_value(text.clone(), window, cx));
    });
    redraw(visual);
}

pub(crate) fn assert_hidden_input_does_not_receive_text(
    visual: &mut VisualTestContext,
    input: &Entity<InputState>,
    expected: &str,
) {
    visual.update(|window, cx| assert!(!input.read(cx).focus_handle(cx).is_focused(window)));
    visual.simulate_input("hidden-input-must-not-change");
    redraw(visual);
    input.read_with(visual, |input, _| {
        assert_eq!(input.value().as_ref(), expected);
    });
}

/// 一次点击后的确认：元素必须存在且可见。
pub(crate) fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} must be rendered"));
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    visual.simulate_click(bounds.center(), Modifiers::none());
    redraw(visual);
}
