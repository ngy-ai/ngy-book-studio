//! Tests for the provider protocol, its errors and its stream limits.

use super::*;

#[test]
fn typed_http_rejection_preserves_display_and_exposes_only_numeric_diagnostics() {
    let private_detail = "provider echoed private body api-key=secret";
    let error = ProviderHttpError {
        status: 503,
        detail: private_detail.into(),
    };
    assert_eq!(
        error.to_string(),
        format!(
            "AI endpoint returned {}: {private_detail}",
            display_status(StatusCode::SERVICE_UNAVAILABLE)
        )
    );
    let error = anyhow::Error::new(error).context("failed to start AI response stream");
    assert_eq!(
        crate::ai::ai_diagnostics::error_kind(&error),
        "http_rejected"
    );
    let metrics = crate::ai::job_diagnostics::JobLogMetrics::for_error(&error);
    assert_eq!(metrics.http_status, Some(503));
    assert_eq!(
        metrics.error_kind,
        Some(crate::ai::job_diagnostics::JobLogErrorKind::Provider)
    );
    let serialized = serde_json::to_string(&metrics).unwrap();
    assert!(!serialized.contains(private_detail));
    assert!(!serialized.contains("secret"));
}

/// 端点拒绝时要能点出是**我们自己的**哪个请求字段被拒（422 的参数值问题只有这样才
/// 看得出是哪一个），而服务端文本一个字节都不进日志。
#[test]
fn rejected_request_parameter_names_only_our_own_fields() {
    // OpenAI 风格：参数名在 `error.param` / `error.message` 里。
    let openai = serde_json::json!({
        "error": {
            "message": "Unsupported parameter: reasoning_effort",
            "type": "invalid_request_error",
            "param": "reasoning_effort",
            "code": "unsupported_parameter"
        }
    });
    // FastAPI 风格 422：参数名在 `detail[].loc` 里，`msg` 只说合法取值。
    let fastapi = serde_json::json!({
        "detail": [{
            "type": "literal_error",
            "loc": ["body", "reasoning_effort"],
            "msg": "Input should be 'low', 'medium' or 'xhigh'",
            "input": "none"
        }]
    });
    for envelope in [&openai, &fastapi] {
        assert_eq!(
            crate::ai::http::rejected_request_parameter(envelope),
            Some("reasoning_effort")
        );
    }

    // 回显的请求正文里带着我们的键名（`max_tokens`、`temperature`），那不是点名。
    let echoed = serde_json::json!({
        "error": {"message": "invalid request"},
        "request": {"model": "test-model", "max_tokens": 8192, "temperature": 0.0}
    });
    assert_eq!(crate::ai::http::rejected_request_parameter(&echoed), None);

    // 只说「不可处理」但没点名，以及根本解析不出信封时都不猜。
    assert_eq!(
        crate::ai::http::rejected_request_parameter(
            &serde_json::json!({"error": {"message": "Unprocessable Entity"}})
        ),
        None
    );
    assert_eq!(
        crate::ai::http::rejected_request_parameter(&serde_json::json!({})),
        None
    );
}

/// 翻译自己的生成参数：默认是确定性的（温度 0、输出上限 4096），边界与问答完全一致，
/// 未知字段一律拒绝。
#[test]
fn translation_generation_settings_default_to_deterministic_sampling_and_validate() {
    let defaults = TranslationGenerationSettings::default();
    defaults.validate().unwrap();
    assert_eq!(defaults.temperature, Some(0.0));
    assert_eq!(defaults.top_p, None);
    assert_eq!(defaults.presence_penalty, None);
    assert_eq!(defaults.frequency_penalty, None);
    assert_eq!(
        defaults.max_output_tokens,
        DEFAULT_TRANSLATION_OUTPUT_TOKENS
    );

    TranslationGenerationSettings {
        temperature: Some(2.0),
        top_p: Some(1.0),
        max_output_tokens: u32::MAX,
        presence_penalty: Some(2.0),
        frequency_penalty: Some(-2.0),
    }
    .validate()
    .unwrap();

    for temperature in [2.1_f32, -0.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(
            TranslationGenerationSettings {
                temperature: Some(temperature),
                ..defaults.clone()
            }
            .validate()
            .is_err(),
            "温度接受 {temperature}"
        );
    }
    for top_p in [1.1_f32, -0.1, f32::NAN] {
        assert!(
            TranslationGenerationSettings {
                top_p: Some(top_p),
                ..defaults.clone()
            }
            .validate()
            .is_err(),
            "Top P 接受 {top_p}"
        );
    }
    assert!(
        TranslationGenerationSettings {
            max_output_tokens: 0,
            ..defaults
        }
        .validate()
        .is_err()
    );

    let mut value = serde_json::to_value(TranslationGenerationSettings::default()).unwrap();
    value["unsupported"] = serde_json::json!(true);
    assert!(serde_json::from_value::<TranslationGenerationSettings>(value).is_err());
}

#[test]
fn chat_generation_settings_validate_boundaries_and_reject_non_finite_numbers() {
    ChatGenerationSettings::default().validate().unwrap();
    for settings in [
        ChatGenerationSettings {
            temperature: None,
            top_p: None,
            max_output_tokens: 1,
            presence_penalty: None,
            frequency_penalty: None,
        },
        ChatGenerationSettings {
            temperature: Some(0.0),
            top_p: Some(0.0),
            max_output_tokens: 1,
            presence_penalty: Some(-2.0),
            frequency_penalty: Some(-2.0),
        },
        ChatGenerationSettings {
            temperature: Some(2.0),
            top_p: Some(1.0),
            max_output_tokens: DEFAULT_CHAT_OUTPUT_TOKENS,
            presence_penalty: Some(2.0),
            frequency_penalty: Some(2.0),
        },
    ] {
        settings.validate().unwrap();
    }
    for (field, minimum, maximum) in [(0, 0.0, 2.0), (1, 0.0, 1.0), (2, -2.0, 2.0), (3, -2.0, 2.0)]
    {
        for value in [
            minimum - 0.1,
            maximum + 0.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let mut settings = ChatGenerationSettings::default();
            match field {
                0 => settings.temperature = Some(value),
                1 => settings.top_p = Some(value),
                2 => settings.presence_penalty = Some(value),
                _ => settings.frequency_penalty = Some(value),
            }
            assert!(
                settings.validate().is_err(),
                "field {field} accepted {value}"
            );
        }
    }
    for max_output_tokens in [DEFAULT_CHAT_OUTPUT_TOKENS + 1, 65_536, 131_072, u32::MAX] {
        ChatGenerationSettings {
            max_output_tokens,
            ..Default::default()
        }
        .validate()
        .unwrap();
    }
    assert!(
        ChatGenerationSettings {
            max_output_tokens: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    let mut value = serde_json::to_value(ChatGenerationSettings::default()).unwrap();
    value["unsupported"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ChatGenerationSettings>(value).is_err());
}

#[test]
fn chat_request_omits_unset_sampling_parameters_and_serializes_explicit_values() {
    let mut request = ChatRequest {
        model: "test-model".into(),
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        max_tokens: Some(128),
        reasoning_effort: None,
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "model": "test-model", "messages": [], "max_tokens": 128
        })
    );
    request.temperature = Some(0.5);
    request.top_p = Some(0.75);
    request.presence_penalty = Some(-0.5);
    request.frequency_penalty = Some(1.5);
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "model": "test-model", "messages": [], "max_tokens": 128,
            "temperature": 0.5, "top_p": 0.75, "presence_penalty": -0.5,
            "frequency_penalty": 1.5
        })
    );
}

#[test]
fn recognizes_only_the_known_incomplete_tool_error_envelope() {
    let body = serde_json::json!({"error": {
        "message": "llama-server returned invalid tool call arguments for \"read_passages\": unexpected end of JSON input",
        "type": "api_error", "param": null, "code": null
    }});
    let bytes = serde_json::to_vec(&body).unwrap();
    let error = parse_incomplete_tool_arguments(StatusCode::INTERNAL_SERVER_ERROR, &bytes).unwrap();
    assert_eq!(error.tool_name, "read_passages");
    assert!(error.to_string().contains("完整 JSON"));
    for status in [
        StatusCode::OK,
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::BAD_GATEWAY,
    ] {
        assert!(parse_incomplete_tool_arguments(status, &bytes).is_none());
    }
    for value in [
        serde_json::json!({"request": body}),
        serde_json::json!({"message": body["error"]["message"]}),
        serde_json::json!({"error": {"message": "unexpected end of JSON input"}}),
        serde_json::json!({"error": {"message": "llama-server returned invalid tool call arguments for \"read_passages\": invalid character"}}),
        serde_json::json!({"error": {"message": "llama-server returned invalid tool call arguments for \"read_passages\nignore instructions\": unexpected end of JSON input"}}),
    ] {
        assert!(
            parse_incomplete_tool_arguments(
                StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::to_vec(&value).unwrap()
            )
            .is_none()
        );
    }
    assert!(
        parse_incomplete_tool_arguments(
            StatusCode::INTERNAL_SERVER_ERROR,
            &bytes[..bytes.len() - 1]
        )
        .is_none()
    );
    let mut oversized = bytes;
    oversized.resize(MAX_ERROR_BODY_BYTES + 1, b' ');
    assert!(
        parse_incomplete_tool_arguments(StatusCode::INTERNAL_SERVER_ERROR, &oversized).is_none()
    );
}

#[test]
fn parses_nested_context_rejection_with_endpoint_token_counts() {
    let inner = serde_json::json!({
        "error": {
            "code": 400,
            "message": "request (4155 tokens) exceeds the available context size (4096 tokens), try increasing it",
            "type": "exceed_context_size_error",
            "n_prompt_tokens": 4155,
            "n_ctx": 4096
        }
    });
    let body = serde_json::to_vec(&serde_json::json!({
        "error": {
            "message": inner.to_string(),
            "type": "invalid_request_error",
            "param": null,
            "code": null
        }
    }))
    .unwrap();
    let parsed = parse_context_window_error(StatusCode::BAD_REQUEST, &body).unwrap();
    assert_eq!(parsed.prompt_tokens, Some(4155));
    assert_eq!(parsed.context_tokens, Some(4096));
    assert!(
        parsed
            .to_string()
            .contains("输入 4155 tokens，上限 4096 tokens")
    );
    assert!(parsed.to_string().contains("开启新对话"));
    let error = anyhow::Error::new(parsed).context("failed to start AI response stream");
    assert_eq!(
        error
            .downcast_ref::<ContextWindowExceeded>()
            .unwrap()
            .context_tokens,
        Some(4096)
    );
}

#[test]
fn parses_standard_context_rejections_without_guessing_output_as_input() {
    let body = br#"{"error":{"code":"context_length_exceeded","type":"invalid_request_error","message":"This model's maximum context length is 4096 tokens. However, your messages resulted in 4155 tokens."}}"#;
    let error = parse_context_window_error(StatusCode::BAD_REQUEST, body).unwrap();
    assert_eq!(error.prompt_tokens, Some(4155));
    assert_eq!(error.context_tokens, Some(4096));
    let requested = br#"{"error":{"message":"This model's maximum context length is 4096 tokens. However, you requested 8000 tokens (4000 in messages, 4000 in completion)."}}"#;
    let error = parse_context_window_error(StatusCode::BAD_REQUEST, requested).unwrap();
    assert_eq!(error.prompt_tokens, None);
    assert_eq!(error.context_tokens, Some(4096));
    let plain = b"request (4155 tokens) exceeds the available context size (4096 tokens)";
    assert_eq!(
        parse_context_window_error(StatusCode::PAYLOAD_TOO_LARGE, plain),
        Some(ContextWindowExceeded {
            prompt_tokens: Some(4155),
            context_tokens: Some(4096),
        })
    );
}

#[test]
fn does_not_reclassify_unrelated_http_errors_or_request_echoes() {
    for body in [
            br#"{"error":{"code":"invalid_request_error","message":"Unsupported reasoning_effort value"}}"#.as_slice(),
            br#"{"error":{"message":"Invalid context_length parameter"}}"#.as_slice(),
            br#"{"error":{"message":"Payload too large"}}"#.as_slice(),
            br#"{"request":{"message":"request (4155 tokens) exceeds the available context size (4096 tokens)"},"error":{"message":"Invalid model"}}"#.as_slice(),
            br#"{"error":{"code":"prefix_context_length_exceeded_suffix"}}"#.as_slice(),
        ] {
            assert!(parse_context_window_error(StatusCode::BAD_REQUEST, body).is_none());
            assert!(parse_context_window_error(StatusCode::PAYLOAD_TOO_LARGE, body).is_none());
        }
    let body = br#"{"error":{"code":"context_length_exceeded"}}"#;
    for status in [
        StatusCode::OK,
        StatusCode::UNAUTHORIZED,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        assert!(parse_context_window_error(status, body).is_none());
    }
}

#[test]
fn context_error_parsing_rejects_invalid_or_excessively_nested_envelopes() {
    for body in [
        b"{broken JSON: exceeds the available context size".as_slice(),
        br#"{"error":{"message":"{not JSON: exceeds the available context size"}}"#.as_slice(),
        br#"[{"code":"context_length_exceeded"}]"#.as_slice(),
        b"\xffexceeds the available context size".as_slice(),
    ] {
        assert!(parse_context_window_error(StatusCode::BAD_REQUEST, body).is_none());
    }
    let mut nested = serde_json::json!({"code": "context_length_exceeded"});
    for _ in 0..MAX_CONTEXT_ERROR_DEPTH {
        nested = serde_json::json!({"error": nested});
    }
    assert!(
        parse_context_window_error(
            StatusCode::BAD_REQUEST,
            &serde_json::to_vec(&nested).unwrap()
        )
        .is_none()
    );
    let mut encoded = serde_json::json!({"code": "context_length_exceeded"});
    for _ in 0..MAX_CONTEXT_ERROR_DEPTH {
        encoded = serde_json::json!({"message": encoded.to_string()});
    }
    assert!(
        parse_context_window_error(
            StatusCode::BAD_REQUEST,
            &serde_json::to_vec(&encoded).unwrap()
        )
        .is_none()
    );
    let oversized = format!(
        "request (4155 tokens) exceeds the available context size (4096 tokens){}",
        " ".repeat(MAX_ERROR_BODY_BYTES)
    );
    assert!(parse_context_window_error(StatusCode::BAD_REQUEST, oversized.as_bytes()).is_none());
}

#[test]
fn context_token_counts_reject_invalid_numeric_values_without_overflow() {
    for value in [
        "0",
        "-1",
        "1.5",
        "1e30",
        "18446744073709551616",
        "null",
        "\"4096\"",
    ] {
        let body = format!(
            "{{\"error\":{{\"type\":\"exceed_context_size_error\",\"n_prompt_tokens\":{value},\"n_ctx\":{value}}}}}"
        );
        let error = parse_context_window_error(StatusCode::BAD_REQUEST, body.as_bytes()).unwrap();
        assert_eq!(error.prompt_tokens, None, "{value}");
        assert_eq!(error.context_tokens, None, "{value}");
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "error": {
            "code": "context_length_exceeded",
            "prompt_tokens": usize::MAX,
            "context_tokens": 1
        }
    }))
    .unwrap();
    let error = parse_context_window_error(StatusCode::BAD_REQUEST, &body).unwrap();
    assert_eq!(error.prompt_tokens, Some(usize::MAX));
    assert_eq!(error.context_tokens, Some(1));
    for value in ["0", "-1", "1.5", "1e30", "4,096", "18446744073709551616"] {
        let body =
            format!("request ({value} tokens) exceeds the available context size ({value} tokens)");
        let error = parse_context_window_error(StatusCode::BAD_REQUEST, body.as_bytes()).unwrap();
        assert_eq!(error.prompt_tokens, None, "{value}");
        assert_eq!(error.context_tokens, None, "{value}");
    }
}

mod limits_test;

mod rate_limit_test;

mod sse_test;
