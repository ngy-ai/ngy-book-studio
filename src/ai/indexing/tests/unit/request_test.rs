use super::*;

#[test]
pub(crate) fn vision_request_disables_model_reasoning() {
    // 视觉识别同样只消费 `content` 里的严格 JSON，必须与翻译、Agent 路径一致地
    // 关闭推理，否则思考模型会在整段请求超时内只回空增量。
    let request = vision_request("vision:test", "data:image/png;base64,AAAA".to_string());
    assert_eq!(request.reasoning_effort, Some(ReasoningEffort::None));
    assert_eq!(
        serde_json::to_value(&request).unwrap()["reasoning_effort"],
        serde_json::json!("none")
    );
    assert_eq!(request.temperature, Some(0.0));
    assert_eq!(request.max_tokens, Some(1_500));
}

#[test]
pub(crate) fn translation_request_disables_model_reasoning() {
    // 思考模型会把整个输出预算和请求超时花在推理上，而 Ollama 的 OpenAI 兼容
    // 端点只回空 content 增量：本地 9B 模型实测在整段请求超时内产出 0 字节正文。
    // 翻译只要 content 里的严格 JSON，所以必须像 Agent 路径一样显式关闭推理。
    let source = TranslationSource {
        text: "第一章 起点".to_string(),
        segments: vec!["起点".to_string()],
    };
    let generation = TranslationGenerationSettings::default();
    let request = translation_request(
        "model:test",
        &generation,
        "zh-Hans",
        None,
        &source,
        &TranslationContext::default(),
        None,
    );
    assert_eq!(request.reasoning_effort, Some(ReasoningEffort::None));
    assert_eq!(
        serde_json::to_value(&request).unwrap()["reasoning_effort"],
        serde_json::json!("none"),
        "关闭推理只有出现在请求体里才对服务端生效"
    );
    // 翻译的默认生成参数：温度 0（确定性）与固定的输出上限。
    assert_eq!(request.temperature, Some(0.0));
    assert_eq!(request.max_tokens, Some(DEFAULT_TRANSLATION_OUTPUT_TOKENS));

    // 格式纠正重试沿用同一组生成参数。
    let correction = translation_request(
        "model:test",
        &generation,
        "zh-Hans",
        None,
        &source,
        &TranslationContext::default(),
        Some("invalid_schema"),
    );
    assert_eq!(correction.reasoning_effort, Some(ReasoningEffort::None));
}

#[test]
pub(crate) fn translation_request_carries_the_configured_generation_parameters() {
    let source = TranslationSource {
        text: "Hello world".to_string(),
        segments: vec!["Hello".to_string(), "world".to_string()],
    };
    let generation = TranslationGenerationSettings {
        temperature: Some(0.6),
        top_p: Some(0.9),
        max_output_tokens: 8192,
        presence_penalty: Some(0.5),
        frequency_penalty: Some(-0.5),
    };
    // 每一发都带同一组参数：两次严格尝试、协议纠正与逐片段回退都走这条构造。
    for correction in [None, Some("invalid_schema")] {
        let request = translation_request(
            "model:test",
            &generation,
            "zh-Hans",
            Some("en"),
            &source,
            &TranslationContext::default(),
            correction,
        );
        assert_eq!(request.temperature, Some(0.6));
        assert_eq!(request.top_p, Some(0.9));
        assert_eq!(request.presence_penalty, Some(0.5));
        assert_eq!(request.frequency_penalty, Some(-0.5));
        assert_eq!(request.max_tokens, Some(8192));
        // 推理开关不受生成参数影响：严格 JSON 的输出预算不能被思考吃掉。
        assert_eq!(request.reasoning_effort, Some(ReasoningEffort::None));
    }

    // 清空温度 = 请求体里根本没有这个键（服务端默认值），而不是温度 0。
    let cleared = TranslationGenerationSettings {
        temperature: None,
        ..generation
    };
    let request = translation_request(
        "model:test",
        &cleared,
        "zh-Hans",
        Some("en"),
        &source,
        &TranslationContext::default(),
        None,
    );
    assert_eq!(request.temperature, None);
    assert!(
        serde_json::to_value(&request)
            .unwrap()
            .get("temperature")
            .is_none()
    );
}

/// 端点拒绝这一发时的错误封套是失败日志里唯一的「端点答了什么」。它必须能从
/// 错误链里取出来（外层的分类包装不能挡住它），而连接、超时类错误没有响应可看。
#[test]
pub(crate) fn provider_error_body_only_comes_from_a_rejected_endpoint() {
    let envelope = "{\"error\":{\"message\":\"model crashed\"}}";
    let rejected = anyhow::Error::new(crate::ai::ProviderHttpError {
        status: 500,
        detail: envelope.to_string(),
    })
    .context("failed to start streaming chat completion");
    assert_eq!(provider_error_body(&rejected), Some(envelope));

    // 传输层错误（连接被拒、超时）没有响应封套。
    let refused = anyhow::anyhow!("failed to start streaming chat completion");
    assert_eq!(provider_error_body(&refused), None);

    // 状态被拒但正文为空：不产生预览行。
    let empty = anyhow::Error::new(crate::ai::ProviderHttpError {
        status: 502,
        detail: String::new(),
    });
    assert_eq!(provider_error_body(&empty), None);
}

#[test]
pub(crate) fn translation_prompt_is_shared_by_the_worker_and_the_inspector() {
    // The task window shows the body rebuilt through `translation_prompt`,
    // while the worker sends the one inside `translation_request`. If these
    // ever diverge, the debug view silently stops describing the real call.
    let source = TranslationSource {
        text: "Hello world".to_string(),
        segments: vec!["Hello".to_string(), "world".to_string()],
    };
    let context = TranslationContext::new("A Book", "Chapter One");
    let generation = TranslationGenerationSettings::default();
    let request = translation_request(
        "chat-test",
        &generation,
        "zh-Hans",
        Some("en"),
        &source,
        &context,
        None,
    );
    let ChatMessage { content, .. } = &request.messages[0];
    let system = match content.as_ref().unwrap() {
        MessageContent::Text(text) => text.as_str(),
        MessageContent::Parts(_) => panic!("the system prompt is plain text"),
    };
    assert_eq!(
        system,
        translation_prompt("zh-Hans", Some("en"), &source, None)
    );
    assert!(system.contains("中文（简体）"));
    // 语言提示是前置的肯定句：语境内联在提示词开头，而不是收尾处的一句猜测。
    assert!(system.starts_with("原文语言是 en。"));
    // Segments are only allowed to be translated, so the prompt names them.
    assert!(system.contains("segments"));

    let ChatMessage { content, .. } = &request.messages[1];
    let user = match content.as_ref().unwrap() {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(_) => panic!("the user message is plain text"),
    };
    assert_eq!(user, source.request_input(&context));
    let parsed = serde_json::from_str::<serde_json::Value>(&user).unwrap();
    assert_eq!(parsed["source"], "Hello world");
    assert_eq!(parsed["segments"][1]["text"], "world");
    // 书名与章节随正文一起进入同一条用户消息：模型看到语境，而系统提示保持不变。
    assert_eq!(parsed["book"], "A Book");
    assert_eq!(parsed["chapter"], "Chapter One");

    // A request without a source-language hint must not mention one.
    let request = translation_request(
        "chat-test",
        &TranslationGenerationSettings::default(),
        "zh-Hans",
        None,
        &source,
        &context,
        None,
    );
    let system = match request.messages[0].content.as_ref().unwrap() {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(_) => panic!("the system prompt is plain text"),
    };
    assert!(!system.contains("原文可能的语言标记"));
    assert!(!system.contains("原文语言是"));
}
