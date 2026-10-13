//! Tests for the models settings window.

use super::super::tests_support::*;
use super::*;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};
use ngy_book_studio::ai::DEFAULT_AI_REQUEST_TIMEOUT_SECS;
use ngy_book_studio::services::{DEFAULT_ENDPOINT_ID, DEFAULT_TRANSLATION_MODEL, EndpointSettings};

fn open_window(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    Entity<ModelsSettingsWindow>,
    &mut VisualTestContext,
) {
    open_settings_view::<ModelsSettingsWindow>(cx, ModelsSettingsWindow::new)
}

fn endpoint_fixture(name: &str, port: u16) -> EndpointSettings {
    EndpointSettings {
        id: format!("endpoint-{name}"),
        name: name.to_string(),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        remote_content_confirmed: false,
        allow_insecure_remote_http: false,
        confirmed_remote_endpoint: String::new(),
        request_timeout_secs: DEFAULT_AI_REQUEST_TIMEOUT_SECS,
        requests_per_minute: 0,
    }
}

/// 走服务层保存一份新的 Endpoint 列表，等价于在「Endpoint」窗口里保存。
fn save_endpoints(services: &Arc<AppServices>, endpoints: Vec<EndpointSettings>) {
    services
        .runtime()
        .block_on(async {
            let mut saved = services.provider_settings().unwrap();
            saved.endpoint_routing.additional_endpoints = endpoints;
            services.configure_providers(saved, BTreeMap::new()).await
        })
        .unwrap();
}

/// 模拟用户离开本窗口又回到本窗口。
fn refocus(visual: &mut VisualTestContext) {
    visual.deactivate_window();
    visual.update(|window, _| window.activate_window());
    redraw(visual);
}

#[gpui_kit::test]
fn role_bindings_change_without_saving_and_block_missing_endpoints(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1600.)));
    redraw(visual);
    // 一个 Tab 只显示一个模型：先切到 Embedding 才看得到它的 Endpoint 选择。
    click(visual, "ai-models-tab-embedding");
    let choice = visual
        .debug_bounds("ai-embedding-endpoint-0")
        .expect("the embedding tab must render its endpoint choices");
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

#[gpui_kit::test]
fn model_and_generation_drafts_survive_invalid_save_then_reset(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(2200.)));
    redraw(visual);
    let inputs = settings.read_with(visual, |view, _| {
        [
            view.chat_generation.temperature.clone(),
            view.chat_generation.top_p.clone(),
            view.chat_generation.max_output_tokens.clone(),
            view.chat_generation.presence_penalty.clone(),
            view.chat_generation.frequency_penalty.clone(),
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
        assert_eq!(
            view.chat_generation
                .max_output_tokens
                .read(cx)
                .value()
                .as_ref(),
            "0"
        );
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

#[gpui_kit::test]
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

/// 翻译 Tab 带自己的一组生成参数：默认温度 0（确定性，与问答的 0.1 不同），两组草稿
/// 分别解析进 `chat_generation` / `translation_generation`，互不影响，「恢复默认」也
/// 只动对应那一组。
#[gpui_kit::test]
fn translation_generation_is_configured_separately_from_chat(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(2600.)));
    redraw(visual);
    click(visual, "ai-models-tab-translation");
    let inputs = settings.read_with(visual, |view, _| {
        [
            view.translation_generation.temperature.clone(),
            view.translation_generation.max_output_tokens.clone(),
        ]
    });
    settings.read_with(visual, |view, cx| {
        let entered = view.entered_settings(cx).unwrap();
        assert_eq!(entered.translation_generation.temperature, Some(0.0));
        assert_eq!(
            entered.translation_generation.max_output_tokens,
            DEFAULT_TRANSLATION_OUTPUT_TOKENS
        );
        assert_eq!(entered.chat_generation.temperature, Some(0.1));
    });

    edit_input(visual, &inputs[0], "0.6");
    edit_input(visual, &inputs[1], "8192");
    settings.read_with(visual, |view, cx| {
        let entered = view.entered_settings(cx).unwrap();
        assert_eq!(entered.translation_generation.temperature, Some(0.6));
        assert_eq!(entered.translation_generation.max_output_tokens, 8192);
        // 只改草稿：已存设置不动，问答那组也没被动过。
        assert_eq!(
            view.services
                .provider_settings()
                .unwrap()
                .translation_generation,
            TranslationGenerationSettings::default()
        );
        assert_eq!(entered.chat_generation, ChatGenerationSettings::default());
    });

    click(visual, "ai-reset-translation-generation");
    settings.read_with(visual, |view, cx| {
        assert_eq!(
            view.entered_settings(cx).unwrap().translation_generation,
            TranslationGenerationSettings::default()
        );
        assert_eq!(
            view.translation_generation.temperature.read(cx).value(),
            "0"
        );
        assert_eq!(view.chat_generation.temperature.read(cx).value(), "0.1");
        assert!(!view.state.notice().unwrap().error);
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

/// 模型设置用 Tab 切换四个配置：一次只渲染一个模型的区域，问答生成参数只跟着问答
/// 模型；翻译模型与问答模型是两个独立字段，草稿在同一个实体里因此切 Tab 不会丢。
#[gpui_kit::test]
fn model_tabs_switch_the_visible_role_and_keep_translation_independent(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1600.)));
    redraw(visual);

    // 默认停在问答模型：四个 Tab 与它的检测按钮都在。
    for selector in [
        "ai-models-tab-chat",
        "ai-models-tab-translation",
        "ai-models-tab-embedding",
        "ai-models-tab-vision",
        "ai-detect-chat-models",
        "ai-reset-chat-generation",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "{selector} 应该在问答 Tab 上渲染"
        );
    }
    settings.read_with(visual, |view, _| {
        assert_eq!(view.active_model_tab, ModelRole::Chat);
    });

    // 切到翻译 Tab：它的区域出现。
    // 注意 `debug_bounds` 不清理旧帧的选择器，所以这里只断言新区域出现、不靠它断言
    // 旧区域消失；「一次只渲染一个模型」由 `active_model_tab` 与渲染分派保证。
    click(visual, "ai-models-tab-translation");
    settings.read_with(visual, |view, _| {
        assert_eq!(view.active_model_tab, ModelRole::Translation);
    });
    assert!(
        visual
            .debug_bounds("ai-detect-translation-models")
            .is_some()
    );

    let translation_input =
        settings.read_with(visual, |view, _| view.translation_model_input.clone());
    edit_input(visual, &translation_input, "translation-only-model");
    settings.read_with(visual, |view, cx| {
        // 保存前只改草稿：已存设置不动。
        assert_eq!(
            view.entered_settings(cx).unwrap().translation_model,
            "translation-only-model"
        );
        assert_eq!(
            view.services.provider_settings().unwrap().translation_model,
            DEFAULT_TRANSLATION_MODEL
        );
    });

    click(visual, "ai-models-tab-chat");
    let chat_input = settings.read_with(visual, |view, _| view.chat_model_input.clone());
    edit_input(visual, &chat_input, "chat-only-model");
    settings.read_with(visual, |view, cx| {
        let entered = view.entered_settings(cx).unwrap();
        assert_eq!(entered.chat_model, "chat-only-model");
        assert_eq!(entered.translation_model, "translation-only-model");
        assert_eq!(view.active_model_tab, ModelRole::Chat);
        // 切走再切回，翻译草稿仍在。
        assert_eq!(
            view.translation_model_input.read(cx).value().as_ref(),
            "translation-only-model"
        );
    });
}

/// Endpoint 列表由「Endpoint」窗口维护：那边保存之后，模型设置窗口重新获得焦点时必须
/// 跟上。否则新加的 Endpoint 在这里根本选不到（此前只能关掉本窗口再打开）。
#[gpui_kit::test]
fn endpoint_choices_follow_the_saved_list_when_the_window_regains_focus(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1600.)));
    redraw(visual);
    assert!(visual.debug_bounds("ai-chat-endpoint-0").is_some());
    assert!(
        visual.debug_bounds("ai-chat-endpoint-1").is_none(),
        "只有一个 Endpoint 时不该有第二个选择项"
    );

    let services = settings.read_with(visual, |view, _| Arc::clone(&view.services));
    save_endpoints(
        &services,
        vec![
            endpoint_fixture("second", 21441),
            endpoint_fixture("third", 21442),
        ],
    );

    // 窗口一直没关，只是切走又切回来：列表按已保存的 Endpoint 刷新。
    refocus(visual);
    settings.read_with(visual, |view, _| {
        let ids: Vec<&str> = view
            .endpoints
            .iter()
            .map(|endpoint| endpoint.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![DEFAULT_ENDPOINT_ID, "endpoint-second", "endpoint-third"]
        );
    });
    click(visual, "ai-chat-endpoint-2");
    settings.read_with(visual, |view, _| {
        assert_eq!(view.chat_endpoint_id, "endpoint-third");
    });

    // 那边改名、改地址后同样跟上：草稿按新的已保存值重建。
    let mut renamed = endpoint_fixture("second", 21443);
    renamed.name = "second-renamed".into();
    save_endpoints(&services, vec![renamed, endpoint_fixture("third", 21442)]);
    refocus(visual);
    settings.read_with(visual, |view, cx| {
        let draft = view
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == "endpoint-second")
            .expect("改名后的 Endpoint 仍在列表里");
        assert_eq!(draft.name_input.read(cx).value().as_ref(), "second-renamed");
        assert_eq!(
            draft.base_url_input.read(cx).value().as_ref(),
            "http://127.0.0.1:21443/v1"
        );
        // 其它模型的草稿不受影响。
        assert_eq!(view.chat_endpoint_id, "endpoint-third");
    });

    // 那边删掉本窗口里选中、但还没保存的 Endpoint（删除只按已保存绑定拦截）：选择退回
    // 默认 Endpoint，保存不再报「绑定的 Endpoint 不存在」。
    save_endpoints(&services, vec![endpoint_fixture("second", 21443)]);
    refocus(visual);
    settings.read_with(visual, |view, cx| {
        assert_eq!(view.endpoints.len(), 2);
        assert_eq!(view.chat_endpoint_id, DEFAULT_ENDPOINT_ID);
        assert!(!view.state.notice().unwrap().error);
        assert!(view.entered_settings(cx).is_ok());
    });
}
