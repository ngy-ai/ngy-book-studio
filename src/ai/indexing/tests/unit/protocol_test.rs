use super::*;

#[test]
pub(crate) fn vision_output_requires_bounded_non_empty_regions() {
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

/// 一个块的失败怎么分类：协议失败与请求级失败都按块跳过（后者计入「连续失败就停」的
/// 计数），数据库/IO/取消类错误仍然让整次运行失败。
#[test]
pub(crate) fn block_failures_are_classified_into_skip_or_fatal() {
    // 协议失败：模型答了、形状不对。
    let protocol = anyhow::Error::new(crate::ai::translation::ResponseError::new("invalid_schema"));
    assert_eq!(
        block_skip_cause(&protocol),
        Some((SkipCause::Protocol, JobLogErrorKind::InvalidSchema))
    );

    // 请求级失败：端点 5xx、超时、以及没有更细分类的响应级错误（例如响应超过大小上限）。
    assert_eq!(
        block_skip_cause(&anyhow::Error::new(crate::ai::ProviderHttpError {
            status: 503,
            detail: "unavailable".to_string(),
        })),
        Some((SkipCause::Request, JobLogErrorKind::Provider))
    );
    for error in [
        anyhow::Error::new(crate::ai::ProviderTimeout::stream(60)),
        // 收流层报的「响应超过大小上限」没有更细的分类：它仍然是这一块自己的问题。
        anyhow::anyhow!("翻译模型响应超过大小上限"),
    ] {
        assert!(
            matches!(block_skip_cause(&error), Some((SkipCause::Request, _))),
            "{error:#} 必须按请求级失败跳过"
        );
    }

    // 不是某一块的毛病：数据库、IO 与取消类错误照旧让整次运行失败。
    for error in [
        anyhow::Error::new(std::io::Error::other("disk gone")),
        anyhow::Error::new(crate::ai::agent_runtime::AgentRequestCancelled),
    ] {
        assert_eq!(block_skip_cause(&error), None, "{error:#} 不该被跳过");
    }
}

#[test]
pub(crate) fn translation_stream_shape_separates_missing_repeating_and_thinking_answers() {
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
pub(crate) fn stream_silence_is_reported_exactly_at_the_limit() {
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
pub(crate) fn an_unfinished_stream_is_only_salvaged_by_one_complete_valid_answer() {
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
