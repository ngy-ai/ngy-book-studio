//! Tests for stream limits and streamed reply assembly.

use super::super::*;
use super::*;

fn sse_state(
    bytes: Vec<u8>,
    limits: SseLimits,
) -> BoundedSseState<
    impl Stream<Item = std::result::Result<Vec<u8>, reqwest::Error>> + Send + 'static,
    Vec<u8>,
> {
    BoundedSseState::new(
        stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(bytes)]),
        limits,
        Duration::from_secs(30),
    )
}

#[test]
fn defaults_to_local_ollama_without_remote_confirmation() {
    let config = ProviderConfig::default();
    let url = config.validated_base_url().unwrap();
    assert_eq!(url.as_str(), DEFAULT_OLLAMA_OPENAI_BASE_URL);
    assert!(!config.is_remote().unwrap());
    assert_eq!(config.request_timeout_secs, DEFAULT_AI_REQUEST_TIMEOUT_SECS);
}

#[test]
fn request_timeout_accepts_only_the_published_bounds() {
    for timeout in [MIN_AI_REQUEST_TIMEOUT_SECS, MAX_AI_REQUEST_TIMEOUT_SECS] {
        let config = ProviderConfig {
            request_timeout_secs: timeout,
            ..Default::default()
        };
        assert!(config.validated_base_url().is_ok());
    }
    for timeout in [
        MIN_AI_REQUEST_TIMEOUT_SECS - 1,
        MAX_AI_REQUEST_TIMEOUT_SECS + 1,
    ] {
        let config = ProviderConfig {
            request_timeout_secs: timeout,
            ..Default::default()
        };
        assert!(config.validated_base_url().is_err());
    }
}

#[test]
fn remote_endpoint_requires_confirmation_and_https() {
    let mut config = ProviderConfig {
        base_url: "https://models.example.test/v1".to_string(),
        ..Default::default()
    };
    assert!(config.validated_base_url().is_err());
    config.remote_content_confirmed = true;
    assert!(config.validated_base_url().is_ok());

    config.base_url = "http://models.example.test/v1".to_string();
    assert!(config.validated_base_url().is_err());
    config.allow_insecure_remote_http = true;
    assert!(config.validated_base_url().is_ok());
}

#[test]
fn parses_streamed_content_tools_and_usage() {
    let event = parse_chat_stream_event(
            r#"{"choices":[{"delta":{"content":"answer","tool_calls":[{"index":0,"id":"call-1","function":{"name":"search_books","arguments":"{\"query\":"}}]},"finish_reason":null}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
        )
        .unwrap();
    assert_eq!(event.content_delta.as_deref(), Some("answer"));
    assert_eq!(
        event.tool_call_deltas[0].name.as_deref(),
        Some("search_books")
    );
    assert_eq!(event.usage.unwrap().total_tokens, 3);
}

#[test]
fn separates_empty_and_unrecognized_stream_chunks() {
    // Ollama 的 OpenAI 兼容端点对思考模型只回 `content: ""` 的空块。如果这些块
    // 与正文块混在一起计数，一次 120 秒超时就会表现为「2886 个内容事件、0 字节
    // 正文」，看不出模型根本没有开始回答。
    let mut diagnostics = SseDiagnostics::new();
    for _ in 0..2 {
        diagnostics.observe(
            &ChatStreamEvent {
                content_delta: Some(String::new()),
                ..ChatStreamEvent::default()
            },
            0,
            1,
        );
    }
    diagnostics.observe(
        &ChatStreamEvent {
            content_delta: Some("答".to_string()),
            ..ChatStreamEvent::default()
        },
        0,
        2,
    );
    // 流的首块通常只带 role；provider 私有的推理字段同样落在这里：本客户端
    // 不消费的字段不会伪装成正文。
    diagnostics.observe(&ChatStreamEvent::default(), 0, 3);
    // `[DONE]` 本身没有载荷，不能计入「无法识别」。
    diagnostics.observe(
        &ChatStreamEvent {
            done: true,
            ..ChatStreamEvent::default()
        },
        0,
        4,
    );
    assert_eq!(diagnostics.content_events, 1);
    assert_eq!(diagnostics.content_bytes, "答".len());
    assert_eq!(diagnostics.empty_content_events, 2);
    assert_eq!(diagnostics.unrecognized_events, 1);
}

#[test]
fn rejects_credentials_in_endpoint_url() {
    let config = ProviderConfig {
        base_url: "https://secret@example.test/v1".to_string(),
        remote_content_confirmed: true,
        ..Default::default()
    };
    assert!(config.validated_base_url().is_err());
}

/// 诊断用的端点来源只到 `scheme://host:port`：路径可能带租户名，不能进日志；端口必须
/// 保留，否则同一台机器上的多个 OpenAI-compatible 服务无法区分（翻译转储行靠它说明
/// 这一发发给了谁）。
#[test]
fn endpoint_origin_label_drops_the_path_and_keeps_the_port() {
    let tenant = normalize_provider_base_url("https://tenant.example.test/v1/").unwrap();
    assert_eq!(
        endpoint_origin_label(&tenant),
        "https://tenant.example.test"
    );

    let local = normalize_provider_base_url("http://127.0.0.1:11434/v1/").unwrap();
    assert_eq!(endpoint_origin_label(&local), "http://127.0.0.1:11434");
}

/// HTTP provider 报出的就是它自己的 `base_url` 来源；翻译转储行的 `endpoint=` 取自这里，
/// 因此它必须与真正发请求的端点同源，而不是另一份可能过期的副本。
#[test]
fn http_provider_reports_the_origin_of_the_endpoint_it_sends_to() {
    let provider = OpenAiHttpProvider::new(ProviderConfig {
        base_url: "https://tenant.example.test/v1/".to_string(),
        remote_content_confirmed: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        provider.endpoint_origin().as_deref(),
        Some("https://tenant.example.test")
    );
}

#[tokio::test]
async fn successful_body_limit_accepts_exact_size_and_rejects_one_more_byte() {
    let exact = stream::iter(vec![
        Ok::<Vec<u8>, reqwest::Error>(b"12".to_vec()),
        Ok::<Vec<u8>, reqwest::Error>(b"34".to_vec()),
    ]);
    assert_eq!(
        collect_limited(exact, 4, "test response").await.unwrap(),
        b"1234"
    );

    let over = stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(b"12345".to_vec())]);
    let error = collect_limited(over, 4, "test response")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("4-byte limit"));
}

#[tokio::test]
async fn sse_limits_apply_before_event_json_is_parsed() {
    let bytes = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},",
        "\"finish_reason\":null}]}\n\n"
    )
    .as_bytes()
    .to_vec();
    let event_wire_bytes = bytes.len() - 1;
    let exact_limits = SseLimits {
        total_bytes: bytes.len(),
        event_bytes: event_wire_bytes,
        events: 1,
    };
    let mut exact = sse_state(bytes.clone(), exact_limits);
    assert_eq!(
        exact
            .next_event()
            .await
            .unwrap()
            .unwrap()
            .content_delta
            .as_deref(),
        Some("x")
    );

    let mut event_over = sse_state(
        bytes.clone(),
        SseLimits {
            event_bytes: event_wire_bytes - 1,
            ..exact_limits
        },
    );
    assert!(
        event_over
            .next_event()
            .await
            .unwrap_err()
            .to_string()
            .contains("chat event exceeds")
    );

    let mut total_over = sse_state(
        bytes.clone(),
        SseLimits {
            total_bytes: bytes.len() - 1,
            ..exact_limits
        },
    );
    assert!(
        total_over
            .next_event()
            .await
            .unwrap_err()
            .to_string()
            .contains("chat stream exceeds")
    );
}

#[tokio::test]
async fn sse_event_count_rejects_the_first_event_above_the_limit() {
    let event = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},",
        "\"finish_reason\":null}]}\n\n"
    );
    let bytes = format!("{event}{event}").into_bytes();
    let mut state = sse_state(
        bytes.clone(),
        SseLimits {
            total_bytes: bytes.len(),
            event_bytes: event.len() - 1,
            events: 1,
        },
    );
    assert!(state.next_event().await.unwrap().is_some());
    assert!(
        state
            .next_event()
            .await
            .unwrap_err()
            .to_string()
            .contains("1-event limit")
    );
}

/// 2026-09-11 现场：reqwest 的整体请求超时（120 秒）切断了一条仍在稳定输出的流
/// （1579 个正文块、7024 字节正文、没有 `[DONE]`）。流会话只能按「静默」失败，
/// 慢但持续有数据的回答必须被允许继续，直到输出上限或服务端结束。
#[tokio::test]
async fn streaming_reply_survives_a_total_duration_beyond_one_chunk_gap() {
    let chunks = (0..10)
            .map(|index| {
                let event = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{index}\"}},\"finish_reason\":null}}]}}\n\n"
                );
                (
                    Duration::from_millis(20),
                    Ok::<Vec<u8>, reqwest::Error>(event.into_bytes()),
                )
            })
            .collect::<Vec<_>>();
    // 累计耗时 200 毫秒，是单块静默上限（50 毫秒）的四倍。
    let input = stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let (delay, chunk) = chunks.next()?;
        tokio::time::sleep(delay).await;
        Some((chunk, chunks))
    });
    let mut state = BoundedSseState::new(input, SseLimits::CHAT, Duration::from_millis(50));
    for _ in 0..10 {
        assert!(state.next_event().await.unwrap().is_some());
    }
}

#[tokio::test]
async fn streaming_reply_reports_a_silent_gap_as_http_timeout() {
    let event = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n";
    let input =
        stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(event.to_vec())]).chain(stream::pending::<
            std::result::Result<Vec<u8>, reqwest::Error>,
        >());
    let mut state = BoundedSseState::new(input, SseLimits::CHAT, Duration::from_secs(1));
    assert!(state.next_event().await.unwrap().is_some());
    let error = state.next_event().await.unwrap_err();
    assert_eq!(error_kind(&error), "http_timeout");
    let timeout = error
        .downcast_ref::<ProviderTimeout>()
        .expect("a silent stream must stay a typed provider timeout");
    assert_eq!(timeout.scope(), "stream");
    assert_eq!(timeout.secs(), 1);
}

#[test]
fn model_entry_limit_is_checked_before_deduplication() {
    let model = ModelInfo {
        id: "model".to_string(),
        owned_by: None,
    };
    assert_eq!(
        validate_and_normalize_models(vec![model.clone(); MAX_MODEL_ENTRIES])
            .unwrap()
            .len(),
        1
    );
    let error = validate_and_normalize_models(vec![model; MAX_MODEL_ENTRIES + 1])
        .unwrap_err()
        .to_string();
    assert!(error.contains("entry limit"));
}
