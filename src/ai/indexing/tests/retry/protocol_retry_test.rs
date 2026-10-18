use super::*;

#[test]
pub(crate) fn a_rejected_block_is_re_sent_until_the_model_answers() {
    // 一个只有一个片段的文本块被模型答垮时，逐片段回退帮不上忙（片段数必须 > 1），
    // 整块重发是唯一的补救：第二发的答案被接受，这一块不必留在原文。
    let provider = Arc::new(MockMergeProvider::default());
    // 第一轮的两发（严格尝试与纠正）都答成空数组（`segment_count_mismatch`）。
    provider.reject_requests.store(2, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let walk = translate_block_source(
        &fixture,
        &job,
        &cursor,
        &translation,
        &single_fragment_source(),
    )
    .unwrap();
    let BlockWalk::Stored(stored, stats) = walk else {
        panic!("重发之后必须产出译文");
    };
    assert_eq!(stored.segments[0].translated, "译文11");
    assert_eq!(stats.fragment_fallback_blocks, 0);
    assert_eq!(provider.requests.load(Ordering::SeqCst), 3);
    assert_eq!(provider.fragment_requests.load(Ordering::SeqCst), 1);

    let events = recorded_events(&fixture, &job.id);
    assert_eq!(
        events
            .iter()
            .filter(|entry| entry.event == JobLogEvent::BlockRetry)
            .count(),
        1,
        "重发一轮就成功，日志里只该有一次整块重发"
    );
    assert!(
        !events
            .iter()
            .any(|entry| entry.event == JobLogEvent::ProtocolSkipped),
        "救回来的块绝不能同时记成跳过"
    );
}

/// 模型显式声明「这段不需要翻译」时，块按原文保存：不算失败、不算跳过、也不会重发。
///
/// 现场（2026-09-20，qwen3:0.6b）：模型对纯符号/数字片段留空回答，而空 text 一直被当成
/// `empty_segment_text` 错误 —— 整块因此重试、逐片段回退，最后按未翻译跳过。现在这种片段
/// 由 `{"id":N,"text":"","keep":true}` 显式声明，第一次请求就被接受。
#[test]
pub(crate) fn a_kept_segment_is_published_as_the_source_text_without_retrying() {
    let provider = Arc::new(MockMergeProvider::default());
    provider.keep_segments.store(true, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let source = single_fragment_source();

    let walk = translate_block_source(&fixture, &job, &cursor, &translation, &source).unwrap();
    let BlockWalk::Stored(stored, _) = walk else {
        panic!("声明不需要翻译的答案必须被接受");
    };
    assert_eq!(stored.segments[0].translated, source.segments[0]);
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "第一发就该被接受，不需要纠正或重发"
    );

    let events = recorded_events(&fixture, &job.id);
    assert!(
        !events
            .iter()
            .any(|entry| entry.event == JobLogEvent::ProtocolSkipped),
        "模型声明过的片段不是失败块"
    );
    assert!(
        !events
            .iter()
            .any(|entry| entry.event == JobLogEvent::ProtocolRejected),
        "声明不需要翻译的答案不该被拒"
    );
}

#[test]
pub(crate) fn a_block_that_keeps_failing_is_skipped_after_the_retry_budget() {
    // 每一次请求都不合协议：整块重发用满 `TRANSLATION_BLOCK_RETRIES` 轮，然后照原来的
    // 规则跳过并保留原文。请求数就是「每轮两次严格尝试 × 轮数」，单片段块没有回退请求。
    let provider = Arc::new(MockMergeProvider::default());
    provider.reject_requests.store(usize::MAX, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let error = match translate_block_source(
        &fixture,
        &job,
        &cursor,
        &translation,
        &single_fragment_source(),
    ) {
        Ok(_) => panic!("协议始终不合格的块绝不能产出译文"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .downcast_ref::<crate::ai::translation::ResponseError>()
            .unwrap()
            .kind(),
        "segment_count_mismatch"
    );
    assert!(format!("{error:#}").contains(&TRANSLATION_BLOCK_RETRIES.to_string()));
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        (TRANSLATION_BLOCK_RETRIES + 1) * TRANSLATION_RESPONSE_ATTEMPTS
    );

    let events = recorded_events(&fixture, &job.id);
    assert_eq!(
        events
            .iter()
            .filter(|entry| entry.event == JobLogEvent::BlockRetry)
            .count(),
        TRANSLATION_BLOCK_RETRIES,
        "第一轮之后每一轮都要留下一次重发记录"
    );
}

#[test]
pub(crate) fn a_provider_failure_is_never_re_sent() {
    // provider、数据库与取消类失败不是「模型答得不对」，重发同一个请求解决不了：整块
    // 立刻上抛，让整次运行失败（块级重试只管协议失败）。
    let provider = Arc::new(MockMergeProvider::default());
    provider.fail_provider.store(true, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let error = match translate_block_source(
        &fixture,
        &job,
        &cursor,
        &translation,
        &single_fragment_source(),
    ) {
        Ok(_) => panic!("provider 失败必须上抛"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("mock provider is unavailable"));
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    assert!(
        !recorded_events(&fixture, &job.id)
            .iter()
            .any(|entry| entry.event == JobLogEvent::BlockRetry)
    );
}

#[test]
pub(crate) fn a_merged_multi_fragment_response_is_retried_one_fragment_at_a_time() {
    // 现场：每个多片段文本块都在两次严格尝试后判 segment_count_mismatch，连续三个
    // 文本块就让整本图书的翻译任务失败（当时连续跳过上限是 3，该上限已于 2026-09-15
    // 取消）。逐片段回退必须让这种模型整块翻完。
    let identity = "translation-v2:chat-test";
    let provider = Arc::new(MockMergeProvider::default());
    let (fixture, job, cursor, translation) = translation_block_fixture(&provider, identity);
    let walk = translate_author_bio_block(&fixture, &job, &cursor, &translation).unwrap();
    let BlockWalk::Stored(stored, stats) = walk else {
        panic!("逐片段回退必须产出一个完整译文");
    };
    assert_eq!(stored.execution_identity, identity);
    assert_eq!(
        stored
            .segments
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<Vec<_>>(),
        ["Andrew Stellman", " is a developer and an author."]
    );
    // 第二个片段的原文以空格开头，译文拼回去时边界空白必须原样保留。
    assert_eq!(stored.segments[0].translated, "译文15");
    assert_eq!(stored.segments[1].translated, " 译文30");
    assert_eq!(
        stored
            .segments
            .iter()
            .map(|segment| segment.translated.as_str())
            .collect::<String>(),
        "译文15 译文30"
    );
    assert_eq!(stats.fragment_fallback_blocks, 1);
    assert_eq!(stats.fragment_fallback_requests, 2);
    // 每一轮都拿到合并答案（两次严格尝试都被拒），六轮之后才在最后一轮逐片段重发，
    // 每个片段各一次请求。
    assert_eq!(
        provider.merged_requests.load(Ordering::SeqCst),
        (TRANSLATION_BLOCK_RETRIES + 1) * TRANSLATION_RESPONSE_ATTEMPTS
    );
    assert_eq!(provider.fragment_requests.load(Ordering::SeqCst), 2);
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        (TRANSLATION_BLOCK_RETRIES + 1) * TRANSLATION_RESPONSE_ATTEMPTS + 2
    );
}

#[test]
pub(crate) fn a_correction_that_breaks_json_still_falls_back_on_a_multi_fragment_block() {
    // 现场（2026-09-15，run 2，block 1786/1787，qwen3.5:0.8b）：attempt 1 把 3 个片段
    // 合并进 id=0，纠正提示之后模型开始按段写、但 JSON 没闭合。只按最后一次的
    // `incomplete_json` 判定会放弃回退，三块连续跳过就结束整次运行（当时的上限，
    // 现已取消）；按整块判定则回退成立，这块照常译出。这个测试固定「看整块」这条规则，
    // 也固定失败类别必须跨重发轮累积：块级重发会把「合并成一条」推到前面的轮次里，
    // 只看最后一轮的 `incomplete_json` 一样会误判。
    let provider = Arc::new(MockMergeProvider::default());
    provider
        .correction_breaks_json
        .store(true, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let walk = translate_author_bio_block(&fixture, &job, &cursor, &translation).unwrap();
    let BlockWalk::Stored(stored, stats) = walk else {
        panic!("整轮出现过可回退失败的多片段块必须走逐片段回退");
    };
    assert_eq!(
        stored
            .segments
            .iter()
            .map(|segment| segment.translated.as_str())
            .collect::<String>(),
        "译文15 译文30"
    );
    assert_eq!(stats.fragment_fallback_blocks, 1);
    assert_eq!(stats.fragment_fallback_requests, 2);
    assert_eq!(provider.fragment_requests.load(Ordering::SeqCst), 2);
}

#[test]
pub(crate) fn a_fragment_that_still_fails_the_protocol_leaves_the_whole_block_untranslated() {
    let provider = Arc::new(MockMergeProvider::default());
    provider.fail_fragments.store(true, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let error = match translate_author_bio_block(&fixture, &job, &cursor, &translation) {
        Ok(_) => panic!("协议仍不合格的片段绝不能产出译文"),
        Err(error) => error,
    };
    // 仍然是协议错误，因此调用方沿用原来的「跳过这一块并继续」规则，不保存部分译文。
    assert_eq!(
        error
            .downcast_ref::<crate::ai::translation::ResponseError>()
            .unwrap()
            .kind(),
        "segment_count_mismatch"
    );
    assert!(format!("{error:#}").contains("第 1 个片段"));
    // 块自己的计数只在产出译文时随 `BlockWalk` 交出，所以失败的这一路只能靠请求次数与
    // 错误文本来固定「确实走到了逐片段回退」。
    assert_eq!(provider.fragment_requests.load(Ordering::SeqCst), 1);
}

#[test]
pub(crate) fn a_truncated_response_is_rejected_as_a_fixed_block_failure() {
    // 现场（2026-09-15，block 1762，qwen3.5:0.8b）：模型在 4096 个输出 token 处被
    // 截断，两次运行都在同一个 ordinal、同一个字节数停下。调用方靠 `ResponseError`
    // 决定「跳过当前文本块并继续」，所以截断必须带着这个类型返回，而不是一个不透明的
    // 运行级错误——否则整本书永远停在这里。
    let provider = Arc::new(MockMergeProvider::default());
    provider.truncated.store(true, Ordering::SeqCst);
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");

    let error = match translate_author_bio_block(&fixture, &job, &cursor, &translation) {
        Ok(_) => panic!("截断的响应绝不能产出译文"),
        Err(error) => error,
    };

    assert_eq!(
        error
            .downcast_ref::<crate::ai::translation::ResponseError>()
            .unwrap()
            .kind(),
        "response_truncated"
    );
    // 截断属于可重试的协议类失败：整块按原来的请求重发 `TRANSLATION_BLOCK_RETRIES`
    // 次（每轮一次请求就撞上输出上限），用尽之后才沿用「跳过这一块」。换请求形状
    // （逐片段）解决不了截断，所以这条路径不发片段请求。
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        TRANSLATION_BLOCK_RETRIES + 1
    );
    assert_eq!(provider.fragment_requests.load(Ordering::SeqCst), 0);
}

#[test]
pub(crate) fn a_truncated_block_is_skipped_and_the_run_still_finishes() {
    // 同一个现场的另一半：截断曾经是运行级失败，于是一个退化文本块让整本书的翻译永远
    // 停在 ordinal 1762 上——任务只有 attempts 在涨，next_ordinal 一动不动。现在它走
    // 「跳过这一块」的规则：整本遍历到底、游标停在书末、按 Succeeded 收尾，被跳过的块
    // 只留在文本块明细里等用户换模型后重试。
    let provider = Arc::new(MockMergeProvider::default());
    provider.truncated.store(true, Ordering::SeqCst);
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let mut stats = TranslationRunStats::default();

    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &fixture.indexing_inner(),
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("截断只让当前文本块跳过，绝不能让整次运行返回 Err");

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
    assert!(!blocks.is_empty(), "这个用例需要一个有正文的图书");
    // 每一块都被跳过，但整本确实跑完了：没有中途停下，游标也没有回滚。
    assert_eq!(stats.untranslated, blocks.len());
    assert_eq!(cursor.next_ordinal, blocks.len());
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("被跳过的文本块不得让整次运行失败");
    };
    assert_eq!(finished.next_ordinal, blocks.len());
    assert!(
        finished.retry_ordinals.is_empty(),
        "普通运行不加限定，整本都在重试范围里"
    );
    // 每个文本块各发 `TRANSLATION_BLOCK_RETRIES + 1` 次请求：截断每一次都同样失败，
    // 用尽重试之后按块跳过；这条路径不走逐片段回退。
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        blocks.len() * (TRANSLATION_BLOCK_RETRIES + 1)
    );
    assert_eq!(stats.fragment_fallback_blocks, 0);
}
