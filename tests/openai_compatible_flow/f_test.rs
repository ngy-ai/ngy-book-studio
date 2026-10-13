//! 端点额度：服务商退回（429）之后整个 Endpoint 一起退避。

use super::*;

use ngy_book_studio::ai::{EndpointBudget, EndpointRateLimits, RequestClass};

fn chat_request() -> ChatRequest {
    ChatRequest {
        model: "chat-model".to_string(),
        messages: vec![ChatMessage::text(ChatRole::User, "question")],
        tools: Vec::new(),
        temperature: None,
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        max_tokens: Some(64),
        reasoning_effort: None,
    }
}

/// `Retry-After` 说完之前不再打第二发：本地额度设为不限，所以测到的等待只可能来自冷却。
#[tokio::test]
pub(crate) async fn a_rate_limited_endpoint_holds_back_its_next_request() {
    let (base_url, arrivals, server) = throttled_chat_server(1);
    let limits = EndpointRateLimits::new();
    let limiter = limits.slot(&base_url, 0).unwrap();
    let provider = OpenAiHttpProvider::new(ProviderConfig {
        base_url,
        api_key: None,
        remote_content_confirmed: false,
        allow_insecure_remote_http: false,
        request_timeout_secs: 10,
    })
    .unwrap()
    .with_endpoint_budget(EndpointBudget::new(limiter, RequestClass::Interactive));

    let error = provider
        .chat_stream(chat_request())
        .await
        .err()
        .expect("429 必须是一次失败，而不是一段流");
    assert!(error.to_string().contains("429"), "{error:#}");

    let mut stream = provider
        .chat_stream(chat_request())
        .await
        .expect("冷却结束之后这一发必须照常发出");
    assert!(
        stream.next().await.is_some(),
        "冷却之后这一发必须真的拿到回答"
    );

    let arrivals = arrivals.lock().unwrap();
    assert_eq!(arrivals.len(), 2);
    let gap = arrivals[1].duration_since(arrivals[0]);
    assert!(
        gap >= Duration::from_millis(900),
        "冷却期间不该再打一次：{gap:?}"
    );
    drop(arrivals);
    server.join().unwrap();
}
