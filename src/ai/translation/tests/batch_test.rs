//! Tests for the batch envelope: packing budget, request shape and per-block restore.

use super::*;

fn item<'a>(
    source: &'a TranslationSource,
    context: TranslationContext<'a>,
) -> TranslationBatchItem<'a> {
    TranslationBatchItem { source, context }
}

fn two_segment_source() -> TranslationSource {
    TranslationSource {
        text: "Hello world".to_string(),
        segments: vec!["Hello ".into(), "world".into()],
    }
}

fn one_segment_source() -> TranslationSource {
    TranslationSource {
        text: "Second".to_string(),
        segments: vec!["Second".into()],
    }
}

/// 单个块的负载在批里与单块请求逐字节相同（只少书/章语境、多一个批内编号）：
/// 「同一个块单独发」与「放在批里发」因此只有信封不同。
#[test]
pub(crate) fn batch_input_keeps_each_block_payload_and_carries_the_shared_context() {
    let first = two_segment_source();
    let second = one_segment_source();
    let context = TranslationContext::new("Book", "Chapter");
    let items = [item(&first, context), item(&second, context)];
    let payload: serde_json::Value = serde_json::from_str(&batch_request_input(&items)).unwrap();
    assert_eq!(payload["book"], serde_json::json!("Book"));
    assert_eq!(payload["chapter"], serde_json::json!("Chapter"));
    let blocks = payload["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["id"], serde_json::json!(0));
    assert_eq!(blocks[1]["id"], serde_json::json!(1));
    assert_eq!(blocks[1]["source"], serde_json::json!(second.text));
    assert_eq!(
        blocks[0]["segments"],
        serde_json::json!([{"id":0,"text":"Hello "},{"id":1,"text":"world"}])
    );

    // 单块请求体里那一份负载：去掉书/章语境与批内编号之后必须完全相同。
    let single: serde_json::Value = serde_json::from_str(&first.request_input(&context)).unwrap();
    let mut single = single.as_object().unwrap().clone();
    single.remove("book");
    single.remove("chapter");
    let mut block = blocks[0].as_object().unwrap().clone();
    block.remove("id");
    assert_eq!(
        serde_json::to_string(&serde_json::Value::Object(block)).unwrap(),
        serde_json::to_string(&serde_json::Value::Object(single)).unwrap(),
        "块自己的负载必须在两条路径上逐字节相同"
    );

    // 没有书名与章节标题时，语境字段整段省略 —— 与单块路径同一套规则。
    let unnamed = [item(&first, TranslationContext::default())];
    let payload: serde_json::Value = serde_json::from_str(&batch_request_input(&unnamed)).unwrap();
    assert!(payload.get("book").is_none());
    assert!(payload.get("chapter").is_none());
}

/// 「每请求文本块数」：`0` = 自动（本机一块一次、远程打包），`1` = 关闭，`> 1` = 上限。
#[test]
pub(crate) fn batch_limit_defaults_to_one_inside_the_machine_and_packs_remote_endpoints() {
    assert_eq!(batch_block_limit(0, true), 1, "本机端点默认一块一次");
    assert_eq!(batch_block_limit(0, false), MAX_TRANSLATION_BATCH_BLOCKS);
    assert_eq!(batch_block_limit(1, false), 1, "显式 1 = 关闭打包");
    assert_eq!(batch_block_limit(4, true), 4, "显式值覆盖自动档");
    assert_eq!(
        batch_block_limit(u32::MAX, false),
        MAX_TRANSLATION_BATCH_BLOCKS_SETTING as usize
    );
}

/// 用户在设置里显式填的块数上限原样进预算：「自动」档的 8 只是默认值，不是天花板，
/// 预算的钳制只兜住「至少一块」与设置验证之外的上界。
#[test]
pub(crate) fn an_explicit_block_limit_above_the_automatic_one_is_kept() {
    let explicit = TranslationBatchBudget::new(100_000, 16);
    assert_eq!(explicit.max_blocks, 16, "9–64 的设置不得被压回自动档的 8");
    assert_eq!(
        TranslationBatchBudget::new(100_000, usize::MAX).max_blocks,
        MAX_TRANSLATION_BATCH_BLOCKS_SETTING as usize,
        "预算只兜住设置验证之外的天花板"
    );
    assert_eq!(
        TranslationBatchBudget::new(100_000, 0).max_blocks,
        1,
        "至少一块"
    );
}

/// 打包的三个上限：输出预算、块数、请求体字节。空批永远收得下第一块。
#[test]
pub(crate) fn packer_fills_a_batch_by_output_budget_block_count_and_bytes() {
    // 输出上限 1000 → 留给译文 600；每块 250，两块 500 放得下，第三块 750 超了。
    let budget = TranslationBatchBudget::new(1000, MAX_TRANSLATION_BATCH_BLOCKS);
    assert_eq!(budget.output_tokens, 600);
    let normal = TranslationBatchEstimate {
        output_tokens: 250,
        request_bytes: 100,
    };
    let mut packer = TranslationBatchPacker::new(budget);
    assert!(packer.is_empty() && packer.accepts(&normal));
    packer.push(&normal);
    assert!(packer.accepts(&normal));
    packer.push(&normal);
    assert!(!packer.accepts(&normal), "600 token 的预算装不下第三块");
    assert_eq!(packer.len(), 2);

    // 单块就超预算时仍然收下：打包是为了少发请求，不是「装不下就不翻」。
    let mut packer = TranslationBatchPacker::new(TranslationBatchBudget::new(100, 4));
    let huge = TranslationBatchEstimate {
        output_tokens: 5_000,
        request_bytes: 10,
    };
    assert!(packer.accepts(&huge));
    packer.push(&huge);
    assert!(!packer.accepts(&huge));
    assert_eq!(packer.len(), 1);

    // 块数上限。
    let mut packer = TranslationBatchPacker::new(TranslationBatchBudget::new(100_000, 3));
    let small = TranslationBatchEstimate {
        output_tokens: 1,
        request_bytes: 1,
    };
    for _ in 0..3 {
        assert!(packer.accepts(&small));
        packer.push(&small);
    }
    assert!(!packer.accepts(&small));

    // 请求体字节上限。
    let mut packer = TranslationBatchPacker::new(TranslationBatchBudget::new(100_000, 8));
    let fat = TranslationBatchEstimate {
        output_tokens: 1,
        request_bytes: MAX_TRANSLATION_BATCH_REQUEST_BYTES / 2 + 1,
    };
    packer.push(&fat);
    assert!(
        !packer.accepts(&fat),
        "两个这样的块会超过批请求体的字节上限"
    );

    // 估算：输出按源文字符数（上界），请求体按实际 payload 加外壳。
    let source = two_segment_source();
    let estimate = estimate_batch_block(&source, &TranslationContext::default());
    assert_eq!(estimate.output_tokens, source.text.chars().count());
    assert!(estimate.request_bytes > source.text.len());
}

/// 批回复按块编号还原，顺序打乱也要落到各自那一块上，结果与单块回复逐字段相同。
#[test]
pub(crate) fn batch_response_restores_every_block_exactly_like_a_single_block_response() {
    let first = two_segment_source();
    let second = one_segment_source();
    let context = TranslationContext::new("Book", "Chapter");
    let items = [item(&first, context), item(&second, context)];
    let response = r#"{"blocks":[
        {"id":1,"translations":[{"id":0,"text":"第二块"}]},
        {"id":0,"translations":[{"id":0,"text":"你好 "},{"id":1,"text":"世界"}]}
    ]}"#;
    let parsed = parse_batch_response(&items, response, "v2").unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0].as_ref().unwrap(),
        &parse_response(
            &first,
            r#"{"translations":[{"id":0,"text":"你好 "},{"id":1,"text":"世界"}]}"#,
            "v2"
        )
        .unwrap()
    );
    assert_eq!(
        parsed[1].as_ref().unwrap(),
        &parse_response(
            &second,
            r#"{"translations":[{"id":0,"text":"第二块"}]}"#,
            "v2"
        )
        .unwrap()
    );
}

/// 信封不合格时整批不可用：块数不符、编号重复或越界、外壳不是批形状。调用方据此拆半重发
/// 或退回单块请求 —— 所以这里必须报出可区分的固定分类，而不是「某一块的形状不对」。
#[test]
pub(crate) fn batch_response_rejects_an_envelope_that_does_not_describe_this_batch() {
    let first = two_segment_source();
    let second = one_segment_source();
    let context = TranslationContext::new("Book", "Chapter");
    let items = [item(&first, context), item(&second, context)];
    let cases = [
        (
            r#"{"blocks":[{"id":0,"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"}]}]}"#,
            "batch_block_count_mismatch",
        ),
        (
            r#"{"blocks":[{"id":0,"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"}]},
                         {"id":0,"translations":[{"id":0,"text":"丙"}]}]}"#,
            "batch_block_ids",
        ),
        (
            r#"{"blocks":[{"id":0,"translations":[{"id":0,"text":"甲"},{"id":1,"text":"乙"}]},
                         {"id":2,"translations":[{"id":0,"text":"丙"}]}]}"#,
            "batch_block_ids",
        ),
        // 模型照旧按单块形状回答：信封不对，退回单块路径（那里能直接接受这个形状）。
        (
            r#"{"translations":[{"id":0,"text":"甲"}]}"#,
            "invalid_schema",
        ),
        // 换成顶层数组不认：单块路径的宽容解码只覆盖单块的答案形状。
        (
            r#"[{"id":0,"translations":[{"id":0,"text":"甲"}]}]"#,
            "invalid_schema",
        ),
    ];
    for (response, kind) in cases {
        for wrapped in [
            response.to_string(),
            format!("翻译如下：\n```json\n{response}\n```\n完成。"),
        ] {
            let error = parse_batch_response(&items, &wrapped, "v2").unwrap_err();
            assert_eq!(error.kind(), kind, "{wrapped}");
        }
    }

    let ambiguous =
        parse_batch_response(&items, r#"{"blocks":[]}{"blocks":[]}"#, "v2").unwrap_err();
    assert_eq!(ambiguous.kind(), "ambiguous_json");
    let incomplete = parse_batch_response(&items, r#"{"blocks":[{"id":0},"#, "v2").unwrap_err();
    assert_eq!(incomplete.kind(), "incomplete_json");
}

/// 一块的形状问题只影响那一块：同一批里答对的块照常落库，坏块交给它自己的单块重试路径。
#[test]
pub(crate) fn batch_response_isolates_one_rejected_block_from_its_neighbours() {
    let first = two_segment_source();
    let second = one_segment_source();
    let context = TranslationContext::default();
    let items = [item(&first, context), item(&second, context)];
    // 第 0 块把两个片段合并成一条（弱模型的典型失败），第 1 块正常。
    let response = r#"{"blocks":[
        {"id":0,"translations":[{"id":0,"text":"你好世界"}]},
        {"id":1,"translations":[{"id":0,"text":"第二块"}]}
    ]}"#;
    let parsed = parse_batch_response(&items, response, "v2").unwrap();
    let rejected = parsed[0].as_ref().unwrap_err();
    assert_eq!(rejected.kind(), "segment_count_mismatch");
    assert_eq!(rejected.expected_segments(), Some(2));
    assert_eq!(rejected.actual_segments(), Some(1));
    assert_eq!(parsed[1].as_ref().unwrap().segments[0].translated, "第二块");
    assert_eq!(
        parsed[1].as_ref().unwrap().execution_identity,
        "v2",
        "批里落库的身份必须与单块一致"
    );
}

/// 批路径接受与单块相同的包装（说明文字、围栏、显式闭合的思考块），片段规则也逐字相同：
/// `keep` 仍然必须与空 text 同时给出，违反时只拒绝那一块。
#[test]
pub(crate) fn batch_response_accepts_the_same_wrappers_and_keeps_the_same_segment_rules() {
    let source = TranslationSource {
        text: "12 Hello world".to_string(),
        segments: vec!["12 ".into(), "Hello world".into()],
    };
    let context = TranslationContext::default();
    let items = [item(&source, context)];
    let answer = r#"{"blocks":[{"id":0,"translations":[
        {"id":0,"text":"","keep":true},{"id":1,"text":"你好世界"}]}]}"#;
    let expected = parse_batch_response(&items, answer, "v2").unwrap();
    assert_eq!(
        expected[0].as_ref().unwrap(),
        &parse_response(
            &source,
            r#"{"translations":[{"id":0,"text":"","keep":true},{"id":1,"text":"你好世界"}]}"#,
            "v2"
        )
        .unwrap()
    );
    for wrapped in [
        format!("好的，这是译文：\n```json\n{answer}\n```\n全部完成。"),
        format!("<think>先看术语，再逐块翻译。</think>\n{answer}"),
        format!("\u{feff}{answer}"),
    ] {
        let parsed = parse_batch_response(&items, &wrapped, "v2").unwrap();
        assert_eq!(parsed[0].as_ref().unwrap().segments[0].translated, "12 ");
    }

    // 声明 keep 又给译文：只拒绝这一块，分类与单块一致。
    let rejected = parse_batch_response(
        &items,
        r#"{"blocks":[{"id":0,"translations":[
            {"id":0,"text":"12","keep":true},{"id":1,"text":"你好世界"}]}]}"#,
        "v2",
    )
    .unwrap();
    assert_eq!(rejected[0].as_ref().unwrap_err().kind(), "keep_with_text");
}
