use super::*;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

#[gpui_kit::test]
fn endpoint_switches_preserve_drafts_and_role_bindings_block_deletion(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1400.)));
    redraw(visual);
    let default_key = settings.read_with(visual, |view, _| {
        view.selected_endpoint().api_key_input.clone()
    });
    edit_input(visual, &default_key, "fixture-default-key");
    let add = visual.debug_bounds("ai-add-endpoint").unwrap();
    visual.simulate_click(add.center(), Modifiers::none());
    redraw(visual);
    assert_hidden_input_does_not_receive_text(visual, &default_key, "fixture-default-key");
    let (extra_id, extra_url) = settings.read_with(visual, |view, cx| {
        let endpoint = view.selected_endpoint();
        assert_ne!(endpoint.id, DEFAULT_ENDPOINT_ID);
        assert!(!endpoint.remote_content_confirmed);
        assert!(!endpoint.allow_insecure_remote_http);
        assert_eq!(endpoint.api_key_update(cx), ApiKeyUpdate::Keep);
        (endpoint.id.clone(), endpoint.base_url_input.clone())
    });
    edit_input(visual, &extra_url, "http://127.0.0.1:18082/v1/");
    let extra_key = settings.read_with(visual, |view, _| {
        view.selected_endpoint().api_key_input.clone()
    });
    edit_input(visual, &extra_key, "fixture-extra-key");

    // 模型仍绑定这个 Endpoint 时删除应被拒绝，草稿保持原样。
    settings.update(visual, |view, cx| {
        view.embedding_endpoint_id = extra_id.clone();
        cx.notify();
    });
    redraw(visual);

    let delete = visual.debug_bounds("ai-delete-endpoint").unwrap();
    visual.simulate_click(delete.center(), Modifiers::none());
    redraw(visual);
    settings.read_with(visual, |view, cx| {
        assert_eq!(view.endpoints.len(), 2);
        assert!(view.state.notice().unwrap().error);
        assert_eq!(
            view.selected_endpoint().api_key_update(cx),
            ApiKeyUpdate::Set("fixture-extra-key".into())
        );
    });

    let default_choice = visual.debug_bounds("ai-endpoint-choice-0").unwrap();
    visual.simulate_click(default_choice.center(), Modifiers::none());
    redraw(visual);
    settings.read_with(visual, |view, cx| {
        assert_eq!(
            view.selected_endpoint().api_key_input.entity_id(),
            default_key.entity_id()
        );
        assert_eq!(
            view.selected_endpoint().api_key_update(cx),
            ApiKeyUpdate::Set("fixture-default-key".into())
        );
        assert_eq!(
            view.endpoints[1].api_key_input.entity_id(),
            extra_key.entity_id()
        );
    });

    // 解除绑定后可以删除，选中项回到默认 Endpoint。
    visual.update(|window, cx| {
        settings.update(cx, |view, cx| {
            view.embedding_endpoint_id = DEFAULT_ENDPOINT_ID.to_string();
            view.select_endpoint(&extra_id, window, cx);
            view.delete_selected_endpoint(window, cx);
            assert_eq!(view.endpoints.len(), 1);
            assert_eq!(view.selected_endpoint_id, DEFAULT_ENDPOINT_ID);
        });
    });
}

/// 「每分钟请求数上限」进出草稿：没填过就是「自动」写成 0（输入框显示为留空），
/// 超上限的值在保存前就被挡住。
#[gpui_kit::test]
fn the_request_limit_round_trips_and_rejects_values_over_the_cap(cx: &mut TestAppContext) {
    use ngy_book_studio::ai::MAX_ENDPOINT_RPM;

    let (_directory, settings, visual) = open_window(cx);
    let input = settings.read_with(visual, |view, cx| {
        let endpoint = view.selected_endpoint();
        assert_eq!(endpoint.entered(cx).unwrap().requests_per_minute, 0);
        assert_eq!(
            endpoint.requests_per_minute_input.read(cx).value().as_str(),
            "",
            "「自动」在输入框里显示成留空"
        );
        view.selected_endpoint().requests_per_minute_input.clone()
    });

    edit_input(visual, &input, "12");
    settings.read_with(visual, |view, cx| {
        assert_eq!(
            view.selected_endpoint()
                .entered(cx)
                .unwrap()
                .requests_per_minute,
            12
        );
    });

    edit_input(visual, &input, &(MAX_ENDPOINT_RPM + 1).to_string());
    settings.read_with(visual, |view, cx| {
        assert!(
            view.selected_endpoint().entered(cx).is_err(),
            "超上限的数字必须在保存前被拒绝"
        );
    });
}

#[gpui_kit::test]
fn endpoint_url_change_resets_only_its_authorization_key_and_detection(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    let (url_input, original_key, original_endpoint) = settings.read_with(visual, |view, cx| {
        let endpoint = view.selected_endpoint();
        (
            endpoint.base_url_input.clone(),
            endpoint.api_key_input.clone(),
            endpoint.entered(cx).unwrap(),
        )
    });
    visual.update(|_, cx| {
        settings.update(cx, |view, cx| {
            let endpoint = &mut view.endpoints[0];
            endpoint.remote_content_confirmed = true;
            endpoint.allow_insecure_remote_http = true;
            endpoint.confirmed_remote_endpoint =
                canonical_endpoint(&original_endpoint.base_url).unwrap();
            endpoint.delete_api_key = true;
            endpoint.detected_models = Some(vec!["old-server-model".into()]);
            cx.notify();
        });
    });
    edit_input(visual, &url_input, "http://127.0.0.1:18083/v1/");
    settings.read_with(visual, |view, cx| {
        let endpoint = view.selected_endpoint();
        assert!(!endpoint.remote_content_confirmed);
        assert!(!endpoint.allow_insecure_remote_http);
        assert!(endpoint.confirmed_remote_endpoint.is_empty());
        assert!(!endpoint.delete_api_key);
        assert!(endpoint.detected_models.is_none());
        assert_ne!(endpoint.api_key_input.entity_id(), original_key.entity_id());
        assert_eq!(endpoint.api_key_update(cx), ApiKeyUpdate::Keep);
        assert!(!detection_matches(view, &original_endpoint, 0, cx));
    });
    visual.update(|_, cx| {
        settings.update(cx, |view, cx| {
            let endpoint = view.selected_endpoint().entered(cx).unwrap();
            assert!(detection_matches(
                view,
                &endpoint,
                view.selected_endpoint().discovery_generation,
                cx
            ));
            apply_detected_models(view, &endpoint, vec![], false);
            assert_eq!(view.selected_endpoint().detected_models, Some(vec![]));
        });
    });
}

#[gpui_kit::test]
fn key_edits_and_key_deletion_invalidate_only_the_matching_endpoint(cx: &mut TestAppContext) {
    let (_directory, settings, visual) = open_window(cx);
    visual.simulate_resize(size(px(1080.), px(1600.)));
    let (key_input, endpoint, generation) = visual.update(|window, cx| {
        settings.update(cx, |view, cx| {
            let endpoint = view.selected_endpoint().entered(cx).unwrap();
            let other = EndpointSettings {
                id: "second-endpoint".into(),
                name: "另一个服务".into(),
                base_url: "http://127.0.0.1:18084/v1/".into(),
                ..endpoint.clone()
            };
            view.endpoints.push(new_endpoint_draft(other, window, cx));
            view.endpoints[0].detected_models = Some(vec!["account-a-model".into()]);
            view.endpoints[1].detected_models = Some(vec!["other-server-model".into()]);
            cx.notify();
            (
                view.endpoints[0].api_key_input.clone(),
                endpoint,
                view.endpoints[0].discovery_generation,
            )
        })
    });
    redraw(visual);
    edit_input(visual, &key_input, "fixture-account-b-key");
    settings.read_with(visual, |view, cx| {
        assert!(view.endpoints[0].detected_models.is_none());
        assert_eq!(
            view.endpoints[0].api_key_input.entity_id(),
            key_input.entity_id()
        );
        assert!(!detection_matches(view, &endpoint, generation, cx));
        assert_eq!(
            view.endpoints[1].detected_models,
            Some(vec!["other-server-model".into()])
        );
    });
    let replacement_key = visual.update(|window, cx| {
        settings.update(cx, |view, cx| {
            view.endpoints[0].detected_models = Some(vec!["account-b-model".into()]);
            set_delete_endpoint_key(view, DEFAULT_ENDPOINT_ID, true, window, cx);
            assert!(view.endpoints[0].detected_models.is_none());
            view.endpoints[0].detected_models = Some(vec!["anonymous-model".into()]);
            set_delete_endpoint_key(view, DEFAULT_ENDPOINT_ID, false, window, cx);
            assert!(view.endpoints[0].detected_models.is_none());
            view.endpoints[0].detected_models = Some(vec!["cached-after-reset".into()]);
            view.endpoints[0].api_key_input.clone()
        })
    });
    redraw(visual);
    edit_input(visual, &replacement_key, "fixture-account-c-key");
    settings.read_with(visual, |view, _| {
        assert!(view.endpoints[0].detected_models.is_none())
    });
    // 检测前的校验失败不应清掉别的 Endpoint 的检测结果。
    visual.update(|window, cx| {
        settings.update(cx, |view, cx| {
            view.endpoints[0].detected_models = Some(vec!["cached-before-failure".into()]);
            view.endpoints[0]
                .request_timeout_input
                .update(cx, |input, cx| input.set_value("0", window, cx));
            detect_models(view, DEFAULT_ENDPOINT_ID, window, cx);
            assert_eq!(view.state.operation, PendingOperation::Idle);
            assert!(view.state.notice().unwrap().error);
            assert!(view.endpoints[0].detected_models.is_none());
            assert_eq!(
                view.endpoints[1].detected_models,
                Some(vec!["other-server-model".into()])
            );
        });
    });
}
