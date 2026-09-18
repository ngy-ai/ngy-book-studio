//! Tests for response decoding, wrapping and repair.

mod repair_test;

pub(crate) use super::*;

pub(crate) fn source() -> TranslationSource {
    TranslationSource {
        text: "Read important code() text".to_string(),
        segments: vec!["Read ".into(), "important".into(), " text\n".into()],
    }
}

pub(crate) fn valid_response() -> &'static str {
    r#"{"translations":[{"id":0,"text":"阅读"},{"id":1,"text":"重要"},{"id":2,"text":"文本"}]}"#
}

#[test]
pub(crate) fn response_accepts_one_answer_inside_common_model_wrappers() {
    let expected = parse_response(&source(), valid_response(), "v2").unwrap();
    let wrapped = [
        format!("```json\n{}\n```", valid_response()),
        format!("```\n{}\n```", valid_response()),
        format!("~~~json\r\n{}\r\n~~~", valid_response()),
        format!("以下是翻译结果：\n{}\n翻译完成。", valid_response()),
        format!(
            "这里是\"翻译结果\"：\n```json\n{}\n```\n完成。",
            valid_response()
        ),
        format!("\u{feff}\r\n{}\r\n", valid_response()),
        format!(
            "<think>先考虑术语，再逐片段翻译。</think>\n{}",
            valid_response()
        ),
        format!(
            "<think>示例：{{\"translations\":[]}}。仅用于推理。</think>\n```json\n{}\n```",
            valid_response()
        ),
    ];
    for response in wrapped {
        assert_eq!(
            parse_response(&source(), &response, "v2").unwrap(),
            expected
        );
    }
}

#[test]
pub(crate) fn wrapper_extraction_does_not_unwrap_json_envelopes_or_choose_between_answers() {
    let answer = valid_response();
    let json_string = serde_json::to_string(answer).unwrap();
    let rejected = [
        format!("[{answer}]"),
        format!("结果：[{answer}]"),
        format!("{{\"answer\":{answer}}}"),
        format!("结果：{{\"answer\":{answer}}}"),
        json_string.clone(),
        format!("结果：{json_string}"),
        format!("{answer}\n{answer}"),
        format!("```json\n{answer}\n```\n```json\n{answer}\n```"),
        format!("参考示例：{{\"translations\":[]}}\n实际结果：{answer}"),
        format!("<think>仍在思考，不能把示例当结果：{answer}"),
        format!("{answer}\n另一答案：{{\"translations\":"),
    ];
    for response in rejected {
        let error = parse_response(&source(), &response, "v2").unwrap_err();
        assert!(error.is::<ResponseError>());
    }
    let error = parse_response(&source(), &format!("{answer}\n{answer}"), "v2").unwrap_err();
    assert_eq!(
        error.downcast_ref::<ResponseError>().unwrap().kind(),
        "ambiguous_json"
    );
}

#[test]
pub(crate) fn wrapper_scanner_ignores_braces_and_escaped_quotes_inside_translation_text() {
    let source = TranslationSource {
        text: "Example".into(),
        segments: vec!["Example".into()],
    };
    let text = "内容 {x} [y] \\\"引号\\\" <think>文字</think>";
    let json = serde_json::json!({"translations": [{"id": 0, "text": text}]}).to_string();
    let result = parse_response(&source, &format!("结果：\n```json\n{json}\n```"), "v2").unwrap();
    assert_eq!(result.segments[0].translated, text);
}

#[test]
pub(crate) fn model_response_errors_are_typed_and_do_not_disclose_provider_content() {
    let secret = "PRIVATE_PROVIDER_FIELD_AND_BOOK_TEXT";
    let response =
        format!("前言\n```json\n{{\"{secret}\":\"{secret}\",\"translations\":[]}}\n```\n后记");
    let error = parse_response(&source(), &response, "v2").unwrap_err();
    let typed = error.downcast_ref::<ResponseError>().unwrap();
    assert_eq!(typed.kind(), "invalid_schema");
    assert!(typed.json_line().is_some());
    assert!(typed.json_column().is_some());
    for display in [
        format!("{error:#}"),
        format!("{error:?}"),
        format!("{typed:?}"),
    ] {
        assert!(!display.contains(secret));
        assert!(!display.contains("前言"));
    }

    let error = parse_response(&source(), r#"{"translations":[]}"#, "v2").unwrap_err();
    let typed = error.downcast_ref::<ResponseError>().unwrap();
    assert_eq!(typed.kind(), "segment_count_mismatch");
    assert_eq!(typed.expected_segments(), Some(3));
    assert_eq!(typed.actual_segments(), Some(0));
    assert!(typed.json_line().is_none());

    let invariant = parse_response(&source(), valid_response(), "").unwrap_err();
    assert!(!invariant.is::<ResponseError>());
}

#[test]
pub(crate) fn wrapped_answers_retain_strict_ids_fields_and_text_validation() {
    let cases = [
        (
            r#"{"translations":[{"id":0,"text":"甲"},{"id":0,"text":"乙"},{"id":2,"text":"丙"}]}"#,
            "duplicate_segment_id",
        ),
        (
            r#"{"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"},{"id":3,"text":"丙"}]}"#,
            "unknown_segment_id",
        ),
        (
            r#"{"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"},{"id":2,"text":"\uFEFF"}]}"#,
            "empty_segment_text",
        ),
        (
            r#"{"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"},{"id":2,"text":"\u0000"}]}"#,
            "invalid_segment_text",
        ),
        (
            r#"{"translations":[{"id":0,"text":"甲","extra":true}]}"#,
            "invalid_schema",
        ),
        (r#"{"translations":[],"extra":true}"#, "invalid_schema"),
        (r#"{"translations":[],"translations":[]}"#, "invalid_schema"),
    ];
    for (json, kind) in cases {
        let response = format!("译文如下：\n```json\n{json}\n```\n完成。");
        let error = parse_response(&source(), &response, "v2").unwrap_err();
        assert_eq!(error.downcast_ref::<ResponseError>().unwrap().kind(), kind);
    }
}

#[test]
pub(crate) fn response_requires_object_segments_without_losing_duplicate_field_rejection() {
    let source = TranslationSource {
        text: "Hello".into(),
        segments: vec!["Hello".into()],
    };
    // 位置数组（serde 可接受的位置序列）单独归类：模型其实按段答了，只是没写
    // 字段名，多片段块可以靠逐片段回退救回，因此不能和「结构无效」混为一谈。
    for response in [
        r#"{"translations":[[0,"你好"]]}"#.to_string(),
        format!(
            "结果：\n```json\n{}\n```",
            r#"{"translations":[[0,"你好"]]}"#
        ),
    ] {
        let error = parse_response(&source, &response, "v2").unwrap_err();
        assert_eq!(
            error.downcast_ref::<ResponseError>().unwrap().kind(),
            "positional_segments"
        );
    }
    // 重复字段仍然只是结构无效：这不是「答了但形状不对」，而是答案本身有歧义。
    let invalid = [
        r#"{"translations":[{"id":0,"id":0,"text":"你好"}]}"#,
        r#"{"translations":[{"id":0,"text":"你好","text":"另一个译文"}]}"#,
    ];
    for json in invalid {
        for response in [json.to_string(), format!("结果：\n```json\n{json}\n```")] {
            let error = parse_response(&source, &response, "v2").unwrap_err();
            assert_eq!(
                error.downcast_ref::<ResponseError>().unwrap().kind(),
                "invalid_schema"
            );
        }
    }
}

#[test]
pub(crate) fn bare_single_segment_object_is_wrapped_for_one_segment_source() {
    let source = TranslationSource {
        text: "Hello".into(),
        segments: vec!["Hello".into()],
    };
    let result = parse_response(&source, r#"{"id":0,"text":"你好"}"#, "v2").unwrap();
    assert_eq!(result.segments.len(), 1);
    assert_eq!(result.segments[0].translated, "你好");
}

#[test]
pub(crate) fn bare_single_segment_object_fails_segment_count_for_multi_segment_source() {
    let source = TranslationSource {
        text: "A B".into(),
        segments: vec!["A".into(), "B".into()],
    };
    let error = parse_response(&source, r#"{"id":0,"text":"甲"}"#, "v2").unwrap_err();
    assert_eq!(
        error.downcast_ref::<ResponseError>().unwrap().kind(),
        "segment_count_mismatch"
    );
}

#[test]
pub(crate) fn bare_segment_array_is_accepted_as_the_translation_list() {
    // 现场（2026-09-12，qwen3.5:0.8b）：小模型丢掉了 translations 外壳，逐块返回
    // 顶层数组 [{"id":0,"text":"…"}]，同一本书的文本块全部被判 invalid_schema。
    let answer = r#"[{"id":0,"text":"阅读"},{"id":1,"text":"重要"},{"id":2,"text":"文本"}]"#;
    let expected = parse_response(&source(), valid_response(), "v2").unwrap();
    for response in [
        answer.to_string(),
        format!("```json\n{answer}\n```"),
        format!("结果：\n```json\n{answer}\n```\n完成。"),
        format!("<think>先确认术语，再逐片段翻译。</think>\n{answer}"),
    ] {
        assert_eq!(
            parse_response(&source(), &response, "v2").unwrap(),
            expected,
            "{response}"
        );
    }
}

#[test]
pub(crate) fn bare_segment_array_keeps_every_element_level_rule() {
    let single = TranslationSource {
        text: "Hello".into(),
        segments: vec!["Hello".into()],
    };
    let rejected = [
        // 位置化片段、非对象元素和包裹另一个答案的数组都不是片段列表。
        r#"[[0,"你好"]]"#,
        r#"["你好"]"#,
        r#"[{"translations":[{"id":0,"text":"你好"}]}]"#,
        // 元素本身仍受 deny_unknown_fields / 重复字段 / JSON 语法约束。
        r#"[{"id":0,"text":"你好","html":"<b>"}]"#,
        r#"[{"id":0,"id":0,"text":"你好"}]"#,
        r#"[{"id":0,"text":"你好"},]"#,
        r#"[{"id":0,"text":"你好"}{"id":0,"text":"你好"}]"#,
        r#"[]"#,
    ];
    for json in rejected {
        for response in [json.to_string(), format!("结果：\n```json\n{json}\n```")] {
            let error = parse_response(&single, &response, "v2").unwrap_err();
            assert_eq!(
                error.downcast_ref::<ResponseError>().unwrap().kind(),
                "invalid_schema",
                "{response}"
            );
        }
    }

    // 结构可接受、片段数量不符时必须落到数量检查，而不是格式错误。
    let two = TranslationSource {
        text: "A B".into(),
        segments: vec!["A".into(), "B".into()],
    };
    let error = parse_response(&two, r#"[{"id":0,"text":"甲"}]"#, "v2").unwrap_err();
    assert_eq!(
        error.downcast_ref::<ResponseError>().unwrap().kind(),
        "segment_count_mismatch"
    );

    // 两个完整的顶层容器仍然无法确认唯一答案。
    let error = parse_response(
        &single,
        r#"[{"id":0,"text":"甲"}] [{"id":0,"text":"乙"}]"#,
        "v2",
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ResponseError>().unwrap().kind(),
        "ambiguous_json"
    );
}
