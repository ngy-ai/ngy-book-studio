//! Tests for the system settings window.

use super::super::tests_support::*;
use super::*;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

fn open_window(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    Entity<SystemSettingsWindow>,
    &mut VisualTestContext,
) {
    open_settings_view::<SystemSettingsWindow>(cx, SystemSettingsWindow::new)
}

#[gpui_kit::test]
fn preferences_are_drafts_until_saved(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1600.)));
    redraw(visual);
    let compact = visual
        .debug_bounds("ai-pdf-compact-reading")
        .expect("compact reading checkbox must be rendered");
    visual.simulate_click(compact.center(), Modifiers::none());
    redraw(visual);
    settings.read_with(visual, |view, _| {
        assert!(view.pdf_compact_reading);
        assert!(view.entered_settings().unwrap().pdf_compact_reading);
        assert!(
            !view
                .services
                .provider_settings()
                .unwrap()
                .pdf_compact_reading,
            "changing the checkbox must not save the reading preference",
        );
    });

    let proxy = visual
        .debug_bounds("ai-use-system-proxy")
        .expect("system proxy checkbox must be rendered");
    settings.read_with(visual, |view, _| {
        assert!(
            view.use_proxy,
            "the system proxy stays on until the user opts out",
        );
    });
    visual.simulate_click(proxy.center(), Modifiers::none());
    redraw(visual);
    settings.read_with(visual, |view, _| {
        assert!(!view.use_proxy);
        assert!(!view.entered_settings().unwrap().use_proxy);
        assert!(
            view.services.provider_settings().unwrap().use_proxy,
            "changing the checkbox must not save the network preference",
        );
    });
    assert!(
        visual
            .debug_bounds("ai-background-job-scheduling")
            .is_none(),
        "调度卡片属于后台任务窗口，不应出现在系统配置窗口里"
    );
}

#[gpui_kit::test]
fn language_dropdown_updates_the_translation_draft(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    settings.read_with(visual, |view, _| {
        assert_eq!(
            view.default_language.as_deref(),
            Some("zh-Hans"),
            "系统配置窗口从简体中文开始",
        );
    });
    let button = visual
        .debug_bounds("ai-default-language")
        .expect("default-language dropdown must be rendered");
    assert!(button.size.width > px(0.) && button.size.height > px(0.));

    visual.update(|_, cx| {
        settings.update(cx, |view, cx| {
            view.set_default_language(Some("ja".to_string()), cx);
        });
    });
    redraw(visual);
    settings.read_with(visual, |view, _| {
        assert_eq!(view.default_language.as_deref(), Some("ja"));
        assert_eq!(
            view.entered_settings().unwrap().default_language.as_deref(),
            Some("ja")
        );
        assert_eq!(
            view.services
                .provider_settings()
                .unwrap()
                .default_language
                .as_deref(),
            Some("zh-Hans"),
            "changing the language must not save the draft",
        );
    });
}

#[gpui_kit::test]
fn storage_paths_are_shown_without_touching_other_settings(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    let change = visual
        .debug_bounds("ai-data-directory-change")
        .expect("data-directory button must be rendered");
    assert!(change.size.width > px(0.));
    settings.read_with(visual, |view, _| {
        let entered = view.entered_settings().unwrap();
        assert_eq!(
            entered.chat_model,
            view.services.provider_settings().unwrap().chat_model
        );
        assert!(view.state.notice().is_none());
    });
}
