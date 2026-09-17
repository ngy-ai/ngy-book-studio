//! Unit tests: the pure request/response shapes around vision and translation.

use super::*;

use crate::{
    ai::job_diagnostics::{JobLogErrorKind, classify_error},
    ai::translation::{TranslationContext, TranslationSource},
    ai::{ChatMessage, MessageContent, ReasoningEffort},
    document::NormalizedRect,
};

#[test]
fn vision_request_disables_model_reasoning() {
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
fn vision_output_requires_bounded_non_empty_regions() {
    let valid = parse_vision_output(
            r#"{"regions":[{"ocr":"正文","description":"","region":{"left":10,"top":200,"right":900,"bottom":800}},{"ocr":"","description":"全页摘要","region":null},{"ocr":"标题","description":"标题区","region":{"left":10,"top":20,"right":900,"bottom":150}}]}"#,
        )
        .expect("bounded region response is valid");
    assert_eq!(valid.regions.len(), 3);
    assert_eq!(valid.regions[0].bounds(), None);
    assert_eq!(
        valid.regions[1].bounds(),
        Some(NormalizedRect::new(10, 20, 900, 150))
    );
    assert_eq!(
        valid.regions[2].bounds(),
        Some(NormalizedRect::new(10, 200, 900, 800))
    );

    for invalid in [
        r#"{"regions":[]}"#,
        r#"{"regions":[{"ocr":"","description":"","region":null}]}"#,
        r#"{"regions":[{"ocr":"missing region","description":""}]}"#,
        r#"{"regions":[{"ocr":"zero width","description":"","region":{"left":10,"top":10,"right":10,"bottom":20}}]}"#,
        r#"{"regions":[{"ocr":"outside","description":"","region":{"left":0,"top":0,"right":1001,"bottom":20}}]}"#,
        r#"{"regions":[{"ocr":"first","description":"","region":null},{"ocr":"second","description":"","region":null}]}"#,
    ] {
        assert!(
            parse_vision_output(invalid).is_err(),
            "invalid response unexpectedly accepted: {invalid}"
        );
    }

    let too_many = serde_json::json!({
        "regions": (0..=MAX_VISION_REGIONS_PER_PAGE)
            .map(|index| serde_json::json!({
                "ocr": format!("region {index}"),
                "description": "",
                "region": null
            }))
            .collect::<Vec<_>>()
    });
    assert!(parse_vision_output(&too_many.to_string()).is_err());

    let too_long = serde_json::json!({
        "regions": [{
            "ocr": "x".repeat(MAX_VISION_REGION_TEXT_CHARS + 1),
            "description": "",
            "region": null
        }]
    });
    assert!(parse_vision_output(&too_long.to_string()).is_err());
}

#[test]
fn translation_request_disables_model_reasoning() {
    // 思考模型会把整个输出预算和请求超时花在推理上，而 Ollama 的 OpenAI 兼容
    // 端点只回空 content 增量：本地 9B 模型实测在整段请求超时内产出 0 字节正文。
    // 翻译只要 content 里的严格 JSON，所以必须像 Agent 路径一样显式关闭推理。
    let source = TranslationSource {
        text: "第一章 起点".to_string(),
        segments: vec!["起点".to_string()],
    };
    let request = translation_request(
        "model:test",
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
    assert_eq!(request.temperature, Some(0.0));
    assert_eq!(request.max_tokens, Some(TRANSLATION_MAX_OUTPUT_TOKENS));

    // 格式纠正重试沿用同一组生成参数。
    let correction = translation_request(
        "model:test",
        "zh-Hans",
        None,
        &source,
        &TranslationContext::default(),
        Some("invalid_schema"),
    );
    assert_eq!(correction.reasoning_effort, Some(ReasoningEffort::None));
}

#[test]
fn translation_stream_shape_separates_missing_repeating_and_thinking_answers() {
    // 2026-09-11 现场：一条仍在稳定输出的流被整体请求超时切断，日志只能看到
    // 「1579 个正文块、7024 字节」；要判断模型是没写出答案、思考没结束还是把答案
    // 重复输出，必须同时记录完整容器数、未闭合容器、片段标记、思考块与重复标记。
    let empty = translation_response_shape("");
    assert_eq!(empty.parsed_as, "none");
    assert_eq!(empty.containers, 0);
    assert!(!empty.open_container);
    assert!(!empty.thinking_open);
    assert!(!empty.repeated_container);

    // 只打开了思考块：整个输出预算都花在推理上时就是这个形状。
    let thinking = translation_response_shape("<think>正在逐句推敲术语……");
    assert!(thinking.thinking_open);
    assert_eq!(thinking.containers, 0);

    // 关闭的思考块加完整答案仍然是合法响应，不能误报为未闭合。`parsed_as` 描述的
    // 是整段原始文本（带前缀时不是 JSON），答案本身由容器数与片段标记体现。
    let closed = translation_response_shape(
        "<think>想好了</think>{\"translations\":[{\"id\":0,\"text\":\"甲\"}]}",
    );
    assert!(!closed.thinking_open);
    assert_eq!(closed.containers, 1);
    assert_eq!(closed.segment_markers, 1);
    assert_eq!(closed.parsed_as, "none");

    // 答案重复输出：模型答完继续复读，诊断必须能指出来。
    let answer = "{\"translations\":[{\"id\":0,\"text\":\"甲\"}]}";
    let repeated = translation_response_shape(&format!("{answer}{answer}"));
    assert_eq!(repeated.containers, 2);
    assert!(repeated.repeated_container);
    assert_eq!(repeated.segment_markers, 2);

    // 截断在容器中间：没有任何完整答案，只能看到未闭合容器。
    let truncated = translation_response_shape(&format!("{answer}{{\"translations\":["));
    assert_eq!(truncated.containers, 1);
    assert!(truncated.open_container);
    assert!(!truncated.repeated_container);

    // 字符串里的括号不参与容器计数。
    let quoted =
        translation_response_shape("{\"translations\":[{\"id\":0,\"text\":\"} {[ } ]\"}]}");
    assert_eq!(quoted.containers, 1);
    assert!(!quoted.open_container);
}

/// 静默上限是「任务还能不能自己走出来」的那条线：差一点继续等，到达即按
/// provider 静默超时收尾，不再无限期占着 `transitions` 门闩——现场 2026-09-17
/// 的整本翻译与 AI 设置保存就是这样一起停住的。
#[test]
fn stream_silence_is_reported_exactly_at_the_limit() {
    let limit = TRANSLATION_STREAM_SILENCE_LIMIT.as_millis() as u64;
    assert!(!stream_silence_exceeded(0));
    assert!(!stream_silence_exceeded(limit - 1));
    assert!(stream_silence_exceeded(limit));

    // 兜底错误必须沿用 provider 超时的固定分类，否则后台任务日志与 UI 会因为
    // 「静默改由收流这一层负责」多出一个类别。
    let timeout = crate::ai::ProviderTimeout::stream(TRANSLATION_STREAM_SILENCE_LIMIT.as_secs());
    let error = anyhow::Error::new(timeout).context("翻译模型流式响应静默超时");
    assert_eq!(
        crate::ai::ai_diagnostics::error_kind(&error),
        "http_timeout"
    );
    assert_eq!(classify_error(&error), JobLogErrorKind::Timeout);
}

#[test]
fn an_unfinished_stream_is_only_salvaged_by_one_complete_valid_answer() {
    let source = TranslationSource {
        text: "Hello".to_string(),
        segments: vec!["Hello".to_string()],
    };
    let identity = "translation-v2:test";
    let answer = "{\"translations\":[{\"id\":0,\"text\":\"你好\"}]}";

    // 答完之后继续输出普通说明：整段仍能严格解码成一个答案，可以沿用。
    assert!(
        crate::ai::translation::parse_response(
            &source,
            &format!("{answer}\n希望这些译文对你有帮助。"),
            identity
        )
        .is_ok()
    );

    // 两个完整答案：协议拒绝在多个答案之间猜测，截断也不能放宽这条规则。
    let two = format!("{answer}{answer}");
    assert!(translation_response_shape(&two).repeated_container);
    assert!(crate::ai::translation::parse_response(&source, &two, identity).is_err());

    // 片段不完整或容器没写完整都不是答案。
    assert!(
        crate::ai::translation::parse_response(&source, r#"{"translations":[]}"#, identity)
            .is_err()
    );
    let truncated = format!("{answer}{{\"translations\":[");
    assert!(translation_response_shape(&truncated).open_container);
    assert!(crate::ai::translation::parse_response(&source, &truncated, identity).is_err());
}

#[test]
fn translation_format_instructions_forbid_full_width_structure_punctuation() {
    // 现场：中文模型把结构冒号写成全角标点（"text："），于是整段响应不是合法
    // JSON。格式说明必须显式约束结构字符，纠正提示还要按失败分类指出问题。
    let format = crate::ai::translation::FORMAT_INSTRUCTIONS;
    assert!(format.contains("半角 ASCII"));
    assert!(format.contains("全角标点"));
    assert!(format.contains("绝不能写在键名、冒号或逗号的位置"));

    for kind in [
        "invalid_json",
        "invalid_schema",
        "incomplete_json",
        "missing_json",
    ] {
        let reminder = translation_correction_reminder(kind, 4);
        assert!(reminder.contains(kind));
        assert!(reminder.contains("半角冒号"));
        assert!(reminder.contains("全部 4 个片段"));
        assert!(reminder.contains("id 为 0 到 3 的整数"));
    }
    let counts = translation_correction_reminder("segment_count_mismatch", 2);
    assert!(counts.contains("片段编号不完整或有重复"));
    let blanks = translation_correction_reminder("empty_segment_text", 1);
    assert!(blanks.contains("非空的译文"));
}

#[test]
fn translation_source_limits_count_raw_whitespace_and_encoded_slot_overhead() {
    let no_context = TranslationContext::default();
    let exact = "a".repeat(MAX_TRANSLATION_SOURCE_CHARS);
    assert!(
        validate_translation_source(
            &TranslationSource {
                text: exact.clone(),
                segments: vec![exact],
            },
            &no_context,
        )
        .is_ok()
    );

    let too_long = "a".repeat(MAX_TRANSLATION_SOURCE_CHARS + 1);
    assert!(
        validate_translation_source(
            &TranslationSource {
                text: too_long.clone(),
                segments: vec![too_long],
            },
            &no_context,
        )
        .is_err()
    );

    // Normalization collapses the visible matching key to three characters,
    // but the request must not retain an unlimited amount of source spacing.
    let whitespace = format!("a{}b", " ".repeat(MAX_TRANSLATION_SOURCE_CHARS));
    assert!(
        validate_translation_source(
            &TranslationSource {
                text: "a b".into(),
                segments: vec![whitespace],
            },
            &no_context,
        )
        .is_err()
    );

    // Each source character fits, yet thousands of tiny inline nodes cause
    // JSON identifiers and keys to exceed the independent request byte cap.
    let fragmented = TranslationSource {
        text: "a".repeat(MAX_TRANSLATION_SOURCE_CHARS),
        segments: vec!["a".to_string(); MAX_TRANSLATION_SOURCE_CHARS],
    };
    assert!(fragmented.request_input(&no_context).len() > MAX_TRANSLATION_REQUEST_BYTES);
    assert!(validate_translation_source(&fragmented, &no_context).is_err());

    // Code is not in the translated leaves but still counts as model context.
    assert!(
        validate_translation_source(
            &TranslationSource {
                text: "x".repeat(MAX_TRANSLATION_SOURCE_CHARS + 1),
                segments: vec!["caption".into()],
            },
            &no_context,
        )
        .is_err()
    );

    // 上下文计入请求上限：书名与章节标题再长也不会把请求体撑过上限，
    // 因为它们在进入请求体之前就被截断到固定长度。
    let huge_title = "标".repeat(MAX_TRANSLATION_SOURCE_CHARS * 2);
    assert!(
        validate_translation_source(
            &TranslationSource {
                text: "a".repeat(MAX_TRANSLATION_SOURCE_CHARS),
                segments: vec!["a".repeat(MAX_TRANSLATION_SOURCE_CHARS)],
            },
            &TranslationContext::new(&huge_title, &huge_title),
        )
        .is_ok()
    );
}

#[test]
fn translation_block_previews_collapse_whitespace_and_truncate() {
    assert_eq!(translation_block_preview("  a\n\tb  "), "a b");
    let exact = "字".repeat(TRANSLATION_BLOCK_PREVIEW_CHARS);
    assert_eq!(translation_block_preview(&exact), exact);
    let preview = translation_block_preview(&"字".repeat(TRANSLATION_BLOCK_PREVIEW_CHARS + 10));
    assert_eq!(preview.chars().count(), TRANSLATION_BLOCK_PREVIEW_CHARS + 1);
    assert!(preview.ends_with('…'));
}

#[test]
fn translation_cache_keys_separate_a_different_leaf_split() {
    assert_eq!(
        translation_cache_key("ab", ["a", "b"].into_iter()),
        translation_cache_key("ab", ["a", "b"].into_iter())
    );
    assert_ne!(
        translation_cache_key("ab", ["a", "b"].into_iter()),
        translation_cache_key("ab", ["ab"].into_iter()),
        "identical text with another leaf split is a different model input"
    );
    assert_ne!(
        translation_cache_key("ab", ["a", "b"].into_iter()),
        translation_cache_key("ba", ["b", "a"].into_iter())
    );
}

#[test]
fn translation_prompt_is_shared_by_the_worker_and_the_inspector() {
    // The task window shows the body rebuilt through `translation_prompt`,
    // while the worker sends the one inside `translation_request`. If these
    // ever diverge, the debug view silently stops describing the real call.
    let source = TranslationSource {
        text: "Hello world".to_string(),
        segments: vec!["Hello".to_string(), "world".to_string()],
    };
    let context = TranslationContext::new("A Book", "Chapter One");
    let request = translation_request("chat-test", "zh-Hans", Some("en"), &source, &context, None);
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
    let request = translation_request("chat-test", "zh-Hans", None, &source, &context, None);
    let system = match request.messages[0].content.as_ref().unwrap() {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(_) => panic!("the system prompt is plain text"),
    };
    assert!(!system.contains("原文可能的语言标记"));
}

#[test]
fn translation_request_body_serializes_the_exact_wire_shape() {
    let source = TranslationSource {
        text: "Body".to_string(),
        segments: vec!["Body".to_string()],
    };
    let request = translation_request(
        "chat-test",
        "zh-Hans",
        Some("en"),
        &source,
        &TranslationContext::default(),
        None,
    );
    let body = serde_json::to_string(&request).unwrap();
    let parsed = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(parsed["model"], "chat-test");
    assert_eq!(parsed["temperature"], 0.0);
    assert_eq!(
        parsed["max_tokens"],
        serde_json::json!(TRANSLATION_MAX_OUTPUT_TOKENS)
    );
    assert_eq!(parsed["reasoning_effort"], "none");
    assert_eq!(parsed["messages"][0]["role"], "system");
    assert_eq!(parsed["messages"][1]["role"], "user");
    // Tools are empty and must not be serialized into the body at all.
    assert!(parsed.get("tools").is_none());
}

#[test]
fn byte_limits_do_not_split_utf8() {
    assert_eq!(truncate_utf8_bytes("中文abcdef", 5), "中");
}

#[test]
fn translation_segment_fallback_covers_only_answered_multi_fragment_blocks() {
    let single = TranslationSource {
        text: "Hello".to_string(),
        segments: vec!["Hello".to_string()],
    };
    assert!(!fragment_fallback_applies(
        &single,
        &["segment_count_mismatch"]
    ));

    let multi = TranslationSource {
        text: "A B".to_string(),
        segments: vec!["A".to_string(), " B".to_string()],
    };
    for kind in [
        "segment_count_mismatch",
        "missing_segment_id",
        "unknown_segment_id",
        "duplicate_segment_id",
        "empty_segment_text",
        "invalid_segment_text",
        "positional_segments",
    ] {
        assert!(fragment_fallback_applies(&multi, &[kind]), "{kind}");
    }
    // 模型根本没写出 JSON 时，换请求形状解决不了问题，仍然按原来的规则跳过。
    for kind in [
        "missing_json",
        "incomplete_json",
        "ambiguous_json",
        "invalid_schema",
    ] {
        assert!(!fragment_fallback_applies(&multi, &[kind]), "{kind}");
    }

    // 现场（2026-09-15，block 1786/1787）：纠正提示把 attempt 1 的
    // `segment_count_mismatch` 换成了写不闭合的 JSON。整轮只要有一次是「答了但
    // 片段不完整」，回退就成立 —— 只读最后一次会让这两块白白跳过。
    assert!(fragment_fallback_applies(
        &multi,
        &["segment_count_mismatch", "invalid_json"]
    ));
    assert!(fragment_fallback_applies(
        &multi,
        &["segment_count_mismatch", "incomplete_json"]
    ));
    // 两次都是 JSON 层面没写出答案，仍然不回退。
    assert!(!fragment_fallback_applies(
        &multi,
        &["incomplete_json", "invalid_json"]
    ));
    // 空列表（理论上不会出现）不应回退。
    assert!(!fragment_fallback_applies(&multi, &[]));

    // 超过请求上限的文本块不再回退，避免一个病态段落耗光请求预算。
    let at_limit = TranslationSource {
        text: "x".repeat(64),
        segments: vec!["x".to_string(); MAX_TRANSLATION_FRAGMENT_FALLBACKS],
    };
    assert!(fragment_fallback_applies(
        &at_limit,
        &["segment_count_mismatch"]
    ));
    let over_limit = TranslationSource {
        text: "x".repeat(64),
        segments: vec!["x".to_string(); MAX_TRANSLATION_FRAGMENT_FALLBACKS + 1],
    };
    assert!(!fragment_fallback_applies(
        &over_limit,
        &["segment_count_mismatch"]
    ));
}

#[test]
fn translation_shape_labels_are_numbers_only() {
    // 现场（2026-09-13，block 79）：片段形状是「10 个字符的短名字 + 470 个字符的
    // 正文」，据此可看出文本块是按内联强调切开的，而日志里没有任何原文。
    let segments = vec!["Andrew Stellman".to_string(), " is a developer".to_string()];
    assert_eq!(translation_segment_chars(&segments), "15,15");
    assert_eq!(translation_expected_ids(2), "0..1");
    assert_eq!(translation_expected_ids(1), "0..0");
    assert_eq!(translation_expected_ids(0), "none");
    assert_eq!(decoded_segment_label(&[(0, 2), (1, 5)]), "0:2,1:5");

    let long = vec!["a".to_string(); TRANSLATION_SEGMENT_LABEL_LIMIT + 2];
    let label = translation_segment_chars(&long);
    assert!(label.ends_with(",+2"));
    assert_eq!(label.matches(',').count(), TRANSLATION_SEGMENT_LABEL_LIMIT);
}
