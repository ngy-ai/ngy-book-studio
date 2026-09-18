//! Tests for the models settings window.

use super::super::tests_support::*;
use super::*;
use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use ngy_book_studio::services::DEFAULT_ENDPOINT_ID;

fn open_window(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    Entity<ModelsSettingsWindow>,
    &mut VisualTestContext,
) {
    open_settings_view::<ModelsSettingsWindow>(cx, ModelsSettingsWindow::new)
}

#[gpui::test]
fn role_bindings_change_without_saving_and_block_missing_endpoints(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1600.)));
    redraw(visual);
    let choice = visual.debug_bounds("ai-embedding-endpoint-0").unwrap();
    visual.simulate_click(choice.center(), Modifiers::none());
    redraw(visual);
    settings.read_with(visual, |view, cx| {
        let entered = view.entered_settings(cx).unwrap();
        assert_eq!(
            entered.endpoint_routing.embedding_endpoint_id,
            DEFAULT_ENDPOINT_ID
        );
        assert_eq!(
            entered.endpoint_routing.chat_endpoint_id,
            DEFAULT_ENDPOINT_ID
        );
        assert_eq!(
            view.services
                .provider_settings()
                .unwrap()
                .endpoint_routing
                .embedding_endpoint_id,
            DEFAULT_ENDPOINT_ID,
        );
    });
    // 不存在的 Endpoint 不会被采纳。
    visual.update(|_, cx| {
        settings.update(cx, |view, cx| {
            view.assign_endpoint(ModelRole::Vision, "missing-endpoint", cx);
            assert_eq!(view.vision_endpoint_id, DEFAULT_ENDPOINT_ID);
        });
    });
}

#[gpui::test]
fn model_and_generation_drafts_survive_invalid_save_then_reset(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(2200.)));
    redraw(visual);
    let inputs = settings.read_with(visual, |view, _| {
        [
            view.temperature_input.clone(),
            view.top_p_input.clone(),
            view.max_output_tokens_input.clone(),
            view.presence_penalty_input.clone(),
            view.frequency_penalty_input.clone(),
        ]
    });
    for (input, value) in inputs.iter().zip(["0.7", "0.8", "131072", "0.3", "-0.4"]) {
        edit_input(visual, input, value);
    }
    settings.read_with(visual, |view, cx| {
        let parameters = view.entered_settings(cx).unwrap().chat_generation;
        assert_eq!(parameters.temperature, Some(0.7));
        assert_eq!(parameters.top_p, Some(0.8));
        assert_eq!(parameters.max_output_tokens, 131_072);
        assert_eq!(parameters.presence_penalty, Some(0.3));
        assert_eq!(parameters.frequency_penalty, Some(-0.4));
        assert_eq!(
            view.services.provider_settings().unwrap().chat_generation,
            ChatGenerationSettings::default()
        );
    });

    edit_input(visual, &inputs[2], "0");
    click(visual, "ai-models-save-settings");
    settings.read_with(visual, |view, cx| {
        assert_eq!(view.state.operation, PendingOperation::Idle);
        assert!(view.state.notice().is_some_and(|notice| notice.error));
        assert_eq!(view.max_output_tokens_input.read(cx).value().as_ref(), "0");
        assert_eq!(
            view.services.provider_settings().unwrap().chat_generation,
            ChatGenerationSettings::default()
        );
    });

    let model = settings.read_with(visual, |view, _| view.chat_model_input.clone());
    edit_input(visual, &model, "preserved-model-draft");
    let reset = visual.debug_bounds("ai-reset-chat-generation").unwrap();
    visual.simulate_click(reset.center(), Modifiers::none());
    redraw(visual);
    settings.read_with(visual, |view, cx| {
        let entered = view.entered_settings(cx).unwrap();
        assert_eq!(entered.chat_generation, ChatGenerationSettings::default());
        assert_eq!(entered.chat_model, "preserved-model-draft");
        assert_eq!(
            view.services.provider_settings().unwrap().chat_model,
            DEFAULT_CHAT_MODEL
        );
        assert!(!view.state.notice().unwrap().error);
    });
}

#[gpui::test]
fn detected_models_fill_the_role_input(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(2200.)));
    redraw(visual);
    settings.update(visual, |view, cx| {
        view.endpoints[0].detected_models = Some(vec!["qwen3.5:0.8b".into()]);
        cx.notify();
    });
    redraw(visual);
    settings.read_with(visual, |view, cx| {
        assert_eq!(
            view.role_model(ModelRole::Chat, cx).as_deref(),
            Some(DEFAULT_CHAT_MODEL)
        );
    });
    visual.update(|window, cx| {
        settings.update(cx, |view, cx| {
            view.adopt_detected_model(ModelRole::Chat, "qwen3.5:0.8b", window, cx);
        });
    });
    redraw(visual);
    settings.read_with(visual, |view, cx| {
        assert_eq!(
            view.role_model(ModelRole::Chat, cx).as_deref(),
            Some("qwen3.5:0.8b")
        );
        assert_eq!(
            view.chat_model_input.read(cx).value().as_ref(),
            "qwen3.5:0.8b"
        );
    });
}

#[test]
fn generation_text_parsing_supports_blank_options_and_rejects_invalid_drafts() {
    let empty_options = parse_chat_generation(" ", "", " 1 ", "", " ").unwrap();
    assert_eq!(empty_options.temperature, None);
    assert_eq!(empty_options.top_p, None);
    assert_eq!(empty_options.presence_penalty, None);
    assert_eq!(empty_options.frequency_penalty, None);
    assert_eq!(empty_options.max_output_tokens, 1);
    for value in ["4097", "65536", "131072", "4294967295"] {
        assert_eq!(
            parse_chat_generation("", "", value, "", "")
                .unwrap()
                .max_output_tokens,
            value.parse::<u32>().unwrap(),
        );
    }
    for invalid in ["", "0", "1.5", "-1", "NaN", "4294967296"] {
        assert!(parse_chat_generation("", "", invalid, "", "").is_err());
    }
    for invalid in ["NaN", "inf", "-inf", "0,7", "abc", "2.1", "-0.1"] {
        assert!(parse_chat_generation(invalid, "", "4096", "", "").is_err());
    }
    assert!(parse_chat_generation("", "1.1", "4096", "", "").is_err());
    assert!(parse_chat_generation("", "", "4096", "-2.1", "").is_err());
    assert!(parse_chat_generation("", "", "4096", "", "2.1").is_err());
}
