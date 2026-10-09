//! Tests for the AI settings window.

use super::models::ModelsSettingsWindow;
use super::tests_support::*;
use super::*;
use gpui_kit::TestAppContext;

#[test]
pub(crate) fn singleton_window_state_distinguishes_live_stale_and_reentrant_requests() {
    assert_eq!(
        existing_window_disposition(false, false, None::<u8>, false, |_| false),
        ExistingWindowDisposition::OpenNew
    );
    assert_eq!(
        existing_window_disposition(true, false, None::<u8>, false, |_| false),
        ExistingWindowDisposition::SuppressReentrantOpen
    );
    assert_eq!(
        existing_window_disposition(false, false, Some(7_u8), true, |handle| handle == 7),
        ExistingWindowDisposition::Activate(7)
    );
    assert_eq!(
        existing_window_disposition(false, false, Some(7_u8), false, |_| true),
        ExistingWindowDisposition::ClearStale(7)
    );
    assert_eq!(
        existing_window_disposition(false, false, Some(7_u8), true, |_| false),
        ExistingWindowDisposition::ClearStale(7)
    );
    assert_eq!(
        existing_window_disposition(false, true, Some(7_u8), true, |_| true),
        ExistingWindowDisposition::SuppressReentrantOpen
    );
}

#[test]
pub(crate) fn api_key_input_has_explicit_keep_set_and_delete_semantics() {
    assert_eq!(api_key_update_for_input("", false), ApiKeyUpdate::Keep);
    assert_eq!(api_key_update_for_input("   ", false), ApiKeyUpdate::Keep);
    assert_eq!(
        api_key_update_for_input("new-secret", false),
        ApiKeyUpdate::Set("new-secret".to_string())
    );
    assert_eq!(
        api_key_update_for_input("new-secret", true),
        ApiKeyUpdate::Delete
    );
}

/// 模型检测失败会带回整条请求地址。提示文字必须折行，而不是撑开提示条溢出窗口：
/// 文字容器的自动最小宽度按整段文字计算，`render_notice` 里显式 `min_w(px(0.))`
/// 之后它才能收缩到可用宽度并按宽度折行。
#[gpui_kit::test]
fn long_notice_text_wraps_inside_the_window(cx: &mut TestAppContext) {
    let (_directory, settings, visual) =
        open_settings_view::<ModelsSettingsWindow>(cx, ModelsSettingsWindow::new);
    visual.simulate_resize(size(px(560.), px(1200.)));
    redraw(visual);
    let url = "https://api.example.invalid/v1/models?endpoint=agnes&cursor=".repeat(8);
    settings.update(visual, |view, cx| {
        view.state_mut().report(
            format!(
                "无法读取 agnes 的模型：failed to query AI models: error sending request for url ({url})。\
                 若使用本地 Ollama，请确认服务已运行。"
            ),
            true,
        );
        cx.notify();
    });
    redraw(visual);
    let bounds = visual
        .debug_bounds("ai-models-notice-text")
        .expect("长提示必须渲染出文字区域");
    assert!(
        bounds.size.width < px(560.),
        "提示文字不能溢出窗口宽度：{bounds:?}"
    );
    assert!(
        bounds.size.height > px(30.),
        "长提示必须折成多行：{bounds:?}"
    );
}

#[test]
pub(crate) fn every_section_has_a_distinct_singleton_selector_and_app_id() {
    let mut slugs = std::collections::BTreeSet::new();
    for section in AiSettingsSection::ALL {
        assert!(slugs.insert(section.slug()));
        assert_eq!(
            section.selector("save-settings"),
            format!("ai-{}-save-settings", section.slug())
        );
        assert!(section.app_id().ends_with(section.slug()));
        assert!(!section.label().is_empty());
        assert!(section.window_min_size().width <= section.window_size().width);
        assert!(section.window_min_size().height <= section.window_size().height);
    }
}
