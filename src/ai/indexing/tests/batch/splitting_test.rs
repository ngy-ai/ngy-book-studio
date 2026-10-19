use super::*;

/// 一块自己的形状不合格时只有它退回单块请求，同批其余块照常落库。
///
/// 坏形状必须落在**多片段**块上才成立：整段合并成一条时，只有片段数大于一的块才数得
/// 出「少了」。
#[test]
pub(crate) fn a_rejected_block_is_retried_on_its_own_while_its_neighbours_are_stored() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    // 中间那一段带行内标记，因此有两个片段。
    fixture.rewrite_first_unit("<p>alpha</p><p>beta <em>inner</em></p><p>gamma</p>");
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    let troubled = &blocks[2];
    assert!(
        troubled.source.segments.len() >= 2,
        "被拒的那块必须是多片段块，否则「合并成一条」不是坏形状"
    );
    // 那一块在批里只答一条：片段数量不符 —— 这是块级错误，整批仍然可用。
    provider
        .merged_in_batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(troubled.source.text.clone());

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4, 1],
        "先是一发四块，然后是坏块自己的一发（单块路径）"
    );
    assert_eq!(
        provider.sent_sources()[1],
        vec![troubled.source.text.clone()],
        "回退的那一发只带坏块自己"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4, "坏块的邻居不该被牵连");
    // 坏块走的是单块路径：那条路径自己的读数留在任务日志里（这一发是它的第一次尝试）。
    let retried = recorded_events(&fixture, &job.id)
        .into_iter()
        .filter(|entry| entry.event == JobLogEvent::ModelRequested)
        .count();
    assert_eq!(
        retried, 5,
        "四块各有一发批请求，坏块另有它自己的一发单块请求"
    );
    let shapes = provider
        .batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert!(
        shapes[0].2 && !shapes[1].2,
        "首发是批信封，回退的那一发是单块请求"
    );
}

/// 信封不合格（块数与输入不符）时整批拆半重发，四块最终都落库。
#[test]
pub(crate) fn a_batch_with_a_wrong_envelope_is_split_in_half() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    // 第一发回一个块数不符的信封：整批一个块都不能用，必须拆半。
    provider.reject_envelopes.store(1, Ordering::SeqCst);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4, 2, 2],
        "一发四块 → 两发两块，块数每层减半"
    );
    assert_eq!(
        provider.sent_sources()[1..].to_vec(),
        vec![
            vec![blocks[0].source.text.clone(), blocks[1].source.text.clone()],
            vec![blocks[2].source.text.clone(), blocks[3].source.text.clone()],
        ],
        "拆半后每一半仍是连续的块，而且都按自己的块号落库"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4);
    assert_eq!(
        (stats.batched_requests, stats.batched_blocks),
        (1, 4),
        "walk 只记它自己打包的那一发；拆半的两发由批层直接发出，不进这项读数"
    );
}

/// 整批被输出上限截断时同样拆半，一直拆到单块请求为止。
#[test]
pub(crate) fn a_truncated_batch_splits_until_the_blocks_go_out_on_their_own() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    fixture.rewrite_first_unit("<p>alpha</p>");
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 2);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 2, "章标题 + 一个段落");
    provider.truncate_requests.store(1, Ordering::SeqCst);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![2, 1, 1],
        "截断的那一批先拆成两块，再各自走单块路径"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 2);
    assert_eq!(
        stats.batched_requests, 1,
        "只有第一发是真正的批量请求，拆到单块之后已经不再打包"
    );
}

/// 端点收流之前就以「超出上下文长度」拒掉整批：这是容量问题，拆半重发 ——
/// 一直拆到单块路径，而不是像传输层失败那样整批逐块跳过。
#[test]
pub(crate) fn a_context_rejected_batch_is_split_until_every_block_goes_out_on_its_own() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 2);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.reject_context.store(1, Ordering::SeqCst);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![2, 1, 1, 2],
        "被拒的两块批拆成两个单块请求，下一窗的两块批照常发出"
    );
    assert_eq!(
        provider
            .batches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(_, _, batch)| *batch)
            .collect::<Vec<_>>(),
        vec![true, false, false, true],
        "拆半之后的两发走单块路径：一条请求只装一块时批信封没有意义"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4, "每一块都翻出来了，没有哪一块被整批跳过");
}

#[test]
pub(crate) fn a_narrowed_retry_only_batches_the_blocks_it_is_allowed_to_send() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    // 第二块已经有译文（同模型、同版本）：本次重试只针对它之后的两块。
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: fixture.book_id.clone(),
                content_unit_id: blocks[1].unit_id.clone(),
                block_id: blocks[1].block_id.clone(),
                ordinal: 1,
                document_revision: fixture.revision,
                unit_revision: blocks[1].unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("en".to_string()),
                model: translation.model.clone(),
                source_text: blocks[1].source.text.clone(),
                translated_text: serde_json::to_string(&StoredTranslation {
                    execution_identity: translation.execution_identity.clone(),
                    segments: blocks[1]
                        .source
                        .segments
                        .iter()
                        .map(|segment| crate::ai::translation::TranslationSegment {
                            source: segment.clone(),
                            translated: "已有译文".to_string(),
                        })
                        .collect(),
                })
                .unwrap(),
                created_at: unix_timestamp().unwrap(),
                updated_at: unix_timestamp().unwrap(),
            },
        )
        .unwrap();
    }
    cursor.retry_ordinals = vec![2, 3];
    cursor.next_ordinal = 0;
    // 运行开始时拿数据库里的任务行对账，所以这次重试的游标也要落库、并刷新手里的任务行。
    let job = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::update_state(
            &conn,
            &job.id,
            db::index_jobs::IndexJobStatus::Running,
            &cursor.encode().unwrap(),
            None,
            1,
            Some(1),
            None,
        )
        .unwrap();
        db::index_jobs::get(&conn, &job.id).unwrap().unwrap()
    };

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    let sent = provider.sent_sources();
    assert_eq!(
        sent.concat(),
        vec![blocks[2].source.text.clone(), blocks[3].source.text.clone()],
        "重试集合之外的块绝不出现在请求里（缓存命中也不会被顺手带上）"
    );
    assert_eq!(
        provider.batch_sizes().len(),
        1,
        "两发目标是同一章、又装得下：仍然可以合并成一发"
    );
    assert_block_stored(&fixture, 2, &blocks[2].source);
    assert_block_stored(&fixture, 3, &blocks[3].source);
    let stored = stored_translations(&fixture);
    assert!(
        stored[&1]
            .segments
            .iter()
            .all(|segment| segment.translated == "已有译文"),
        "没被重试的块保持原样"
    );
    assert_eq!(
        (stats.saved, stats.cached),
        (2, 0),
        "重试目标之外、但还在重试范围内的块按「不必处理」走过：既不算缓存命中，也不重新落库"
    );
}
