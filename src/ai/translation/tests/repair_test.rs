//! Tests for punctuation repair and accepted-answer invariants.

use super::super::*;
use super::*;

#[test]
pub(crate) fn structural_full_width_punctuation_is_repaired_without_touching_content() {
    let single = TranslationSource {
        text: "Hello".into(),
        segments: vec!["Hello".into()],
    };
    // 现场成因：中文模型把结构冒号、逗号写成了全角标点。
    let repaired = parse_response(
        &single,
        "{\"translations\"：[{\"id\"：0，\"text\"：\"你好\"}]}",
        "v2",
    )
    .unwrap();
    assert_eq!(repaired.segments[0].translated, "你好");

    // 结构引号写成全角引号同样可修复，值内容逐字保留。
    let quoted = parse_response(
        &single,
        "{\"translations\":[{\"id\":0,\"text\":“嗯：好，”}]}",
        "v2",
    )
    .unwrap();
    assert_eq!(quoted.segments[0].translated, "嗯：好，");
}

#[test]
pub(crate) fn accepted_answers_are_never_rewritten_by_the_punctuation_repair() {
    let single = TranslationSource {
        text: "Hello".into(),
        segments: vec!["Hello".into()],
    };
    let content = "他说：“好”：没问题，";
    let json = serde_json::json!({"translations": [{"id": 0, "text": content}]}).to_string();
    let parsed = parse_response(&single, &json, "v2").unwrap();
    assert_eq!(parsed.segments[0].translated, content);

    let (repaired, replaced) = repair_structural_punctuation(&json);
    assert_eq!(replaced, 0, "合法响应不进入修复路径");
    assert_eq!(repaired.as_ref(), json);
}

#[test]
pub(crate) fn punctuation_repair_never_invents_missing_content() {
    // 截断的响应即使结构标点是全角也仍然被拒绝。
    let error = parse_response(
        &source(),
        "{\"translations\"：[{\"id\"：0，\"text\"：\"甲\"}",
        "v2",
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ResponseError>().unwrap().kind(),
        "incomplete_json"
    );

    // 266 号现场：键名吞掉了分隔符（"text："），要恢复它只能猜模型意图，
    // 因此必须继续拒绝，而不是把全角标点当成译文或补一个空值。
    let field = r#"{"translations":[{"id":0,"text":"1 + 1"},{"id":1,"text":" = "},{"id":2,"text":"2"},{"id":3,"text：","as you'd expect."}]}"#;
    let error = parse_response(&source(), field, "v2").unwrap_err();
    let detail = error.downcast_ref::<ResponseError>().unwrap();
    assert!(detail.json_line().is_some() && detail.json_column().is_some());
}

#[test]
pub(crate) fn response_reorders_ids_and_preserves_source_boundary_spacing() {
    let result = parse_response(
            &source(),
            r#"{"translations":[{"id":2,"text":"文本"},{"id":0,"text":" 阅读 "},{"id":1,"text":"重要"}]}"#,
            "translation-v2:test",
        )
        .unwrap();
    assert_eq!(result.execution_identity, "translation-v2:test");
    assert_eq!(
        result
            .segments
            .iter()
            .map(|segment| segment.translated.as_str())
            .collect::<Vec<_>>(),
        ["阅读 ", "重要", " 文本\n"]
    );
    assert_eq!(result.segments[0].source, "Read ");
    let stored = serde_json::to_string(&result).unwrap();
    assert_eq!(
        serde_json::from_str::<StoredTranslation>(&stored).unwrap(),
        result
    );
}

#[test]
pub(crate) fn response_rejects_missing_duplicate_unknown_ids_and_invalid_shapes() {
    let invalid = [
        r#"{"translations":[{"id":0,"text":"甲"}]}"#,
        r#"{"translations":[{"id":0,"text":"甲"},{"id":0,"text":"乙"},{"id":2,"text":"丙"}]}"#,
        r#"{"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"},{"id":3,"text":"丙"}]}"#,
        r#"{"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"},{"id":2,"text":" \n\uFEFF"}]}"#,
        r#"{"translations":[{"id":0,"text":null}]}"#,
        r#"{"translations":[{"id":"0","text":"甲"}]}"#,
        r#"{"translations":[{"id":-1,"text":"甲"}]}"#,
        r#"{"translations":[{"id":0.5,"text":"甲"}]}"#,
        r#"{"translations":[{"id":0,"text":"甲","html":"<b>"}]}"#,
        r#"{"translations":[],"html":"<script>"}"#,
        r#"{"translations":[],"translations":[]}"#,
        "```json\n{\"translations\":[]}\n```",
        "直接输出纯文本",
    ];
    for response in invalid {
        assert!(
            parse_response(&source(), response, "v2").is_err(),
            "{response}"
        );
    }
}

#[test]
pub(crate) fn model_markup_is_only_a_text_value() {
    let source = TranslationSource {
        text: "Hello".into(),
        segments: vec!["Hello".into()],
    };
    let result = parse_response(
        &source,
        r#"{"translations":[{"id":0,"text":"<img src=x onerror=alert(1)> **文字**"}]}"#,
        "v2",
    )
    .unwrap();
    assert_eq!(
        result.segments[0].translated,
        "<img src=x onerror=alert(1)> **文字**"
    );
}

#[test]
pub(crate) fn request_provides_whole_block_context_and_only_identified_text_leaves() {
    let value: serde_json::Value =
        serde_json::from_str(&source().request_input(&TranslationContext::default())).unwrap();
    assert_eq!(value["source"], "Read important code() text");
    assert_eq!(value["segments"].as_array().unwrap().len(), 3);
    assert_eq!(
        value["segments"][0],
        serde_json::json!({"id": 0, "text": "Read "})
    );
    assert_eq!(
        value["segments"][2],
        serde_json::json!({"id": 2, "text": " text\n"})
    );
}

#[test]
pub(crate) fn request_carries_book_and_chapter_as_context_inside_the_untrusted_payload() {
    let context = TranslationContext::new("  墨页  手册  ", "第一章\n起点");
    let input = source().request_input(&context);
    let value: serde_json::Value = serde_json::from_str(&input).unwrap();
    // 标题先折叠空白，再作为语境字段进入同一个 JSON：正文与片段不受影响。
    assert_eq!(value["book"], "墨页 手册");
    assert_eq!(value["chapter"], "第一章 起点");
    assert_eq!(value["source"], "Read important code() text");
    assert_eq!(value["segments"].as_array().unwrap().len(), 3);
    // 没有上下文的调用产出的请求体与上下文为空时逐字节相同：既不加空字段，
    // 也不改变字段顺序（合成片段仍复用这条路径）。
    assert_eq!(
        source().request_input(&TranslationContext::default()),
        serde_json::json!({
            "source": "Read important code() text",
            "segments": [
                {"id": 0, "text": "Read "},
                {"id": 1, "text": "important"},
                {"id": 2, "text": " text\n"},
            ],
        })
        .to_string()
    );
    assert_eq!(
        source().request_input(&TranslationContext::new("  ", "\n")),
        source().request_input(&TranslationContext::default())
    );
}

#[test]
pub(crate) fn context_labels_are_truncated_so_one_title_cannot_bloat_every_request() {
    let long = "标".repeat(MAX_CONTEXT_LABEL_CHARS + 500);
    let value: serde_json::Value =
        serde_json::from_str(&source().request_input(&TranslationContext::new(&long, &long)))
            .unwrap();
    for field in ["book", "chapter"] {
        assert_eq!(
            value[field].as_str().unwrap().chars().count(),
            MAX_CONTEXT_LABEL_CHARS,
            "{field} 必须被截断到固定上限"
        );
    }
}

#[test]
pub(crate) fn decoded_segment_shapes_report_ids_and_lengths_without_content() {
    assert_eq!(
        decoded_segment_shapes(valid_response()).unwrap(),
        vec![(0, 2), (1, 2), (2, 2)]
    );

    // 现场（2026-09-13，qwen3.5:0.8b）：模型把整段译文塞进 id=0 一条。诊断必须能
    // 指出「期望 2 条、实际 1 条」而不带出译文本身。
    let merged = r#"[{"id":0,"text":"整段合并的译文"}]"#;
    assert_eq!(decoded_segment_shapes(merged).unwrap(), vec![(0, 7)]);

    // 解码失败的响应与非 JSON 响应都只给出固定分类，不返回片段。
    let error = decoded_segment_shapes("不是 JSON").unwrap_err();
    assert_eq!(error.kind(), "missing_json");
    let error = decoded_segment_shapes("<think>没写完").unwrap_err();
    assert_eq!(error.kind(), "incomplete_reasoning");
}

#[test]
pub(crate) fn matching_whitespace_agrees_with_javascript_including_bom() {
    assert_eq!(
        normalize_source_text("\u{feff}a\u{a0}\t b\u{2028}c\u{feff}"),
        "a b c"
    );
    assert_eq!(normalize_source_text("a\u{85}b"), "a\u{85}b");
}

#[test]
pub(crate) fn only_leaves_with_visible_text_own_a_translation_slot() {
    // 空白与不可见格式字符都没有可翻译内容；模型对它们只会回空白，而空片段
    // 会让整个文本块被拒绝，因此两端都必须把它们排除在片段列表之外。
    for invisible in [
        "   ",
        "\t\n",
        "\u{a0}\u{3000}",
        "\u{feff}",
        "\u{200b}",
        "\u{200b}\u{200c}",
        "\u{00ad}",
        "\u{0600}",
        "\u{0605}",
        "\u{06dd}",
        "\u{070f}",
        "\u{0890}\u{0891}",
        "\u{08e2}",
        "\u{2060}",
        "\u{202a}\u{200b}",
        "\u{110bd}\u{110cd}",
        "\u{13430}\u{1343f}",
        "\u{1bca0}\u{1bca3}",
        "\u{1d173}\u{1d17a}",
        "\u{e0001}\u{e0020}\u{e007f}",
    ] {
        assert!(!has_visible_text(invisible), "{invisible:?} 不应成为片段");
    }
    for visible in ["a", "1.", "—", "\u{200b}字", "\u{a0}x", "x\u{00ad}"] {
        assert!(has_visible_text(visible), "{visible:?} 应当成为片段");
    }
}
