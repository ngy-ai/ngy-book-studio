use super::*;

/// 阅读端只说得出块标识：`retranslate_translation_block` 负责把它换成任务里的序号，
/// 换成不出来的就明确报错，绝不随手挑一块重跑（那会写下另一块的译文）。
#[test]
pub(crate) fn retranslating_by_block_id_requeues_exactly_that_blocks_ordinal() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockMergeProvider::default());
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v2:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
    assert!(
        blocks.len() >= 2,
        "这条用例需要至少两个文本块，实际 {}",
        blocks.len()
    );
    let total = blocks.len();
    // 阅读端拿到的是块标识：这里故意挑**第二块**，证明映射没有退化成「总是第 0 块」。
    let (unit_id, block_id, ordinal) = (
        blocks[1].unit_id.clone(),
        blocks[1].block_id.clone(),
        blocks[1].ordinal,
    );
    assert_ne!(ordinal, 0, "第二块的序号不能是 0，否则这条用例证明不了映射");
    drop(conn);
    // 任务先按「跑完」停在书末：重试要能把它重新排队。
    finish_job_at(&fixture, &job_id, total);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate_translation_block(&job_id, &unit_id, &block_id))
            .unwrap()
    );
    let queued = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(queued.status, db::index_jobs::IndexJobStatus::Queued);
    let cursor: JobCursor = serde_json::from_str(&queued.cursor_json).unwrap();
    assert_eq!(cursor.retry_ordinals, vec![ordinal]);
    assert_eq!(cursor.next_ordinal, ordinal);

    // 找不到的块标识、以及不属于这一章的块标识，都必须报错而不是换一块重跑。
    let error = fixture
        .runtime
        .block_on(coordinator.retranslate_translation_block(&job_id, &unit_id, "no-such-block"))
        .unwrap_err();
    assert!(format!("{error:#}").contains("找不到文本块"), "{error:#}");
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate_translation_block(&job_id, "other-unit", &block_id))
            .is_err()
    );
}

/// 阅读端的逐块「重新翻译」：用户对译文不满意、或者换了更好的模型想重做这一块，所以
/// **已有可显示的译文也要重新发给模型**（`TranslationRange::Retry { force: true }`），
/// 旧行由新译文覆盖。任务窗口的普通重试没有这个权力 —— 那些块本来就没有译文，命中缓存
/// 是「别为同一块重复付费」的保护。
///
/// 两条路径共用同一份规划，区别只在 `force`：这条用例把两者放在同一块上一前一后地跑，
/// 「请求数没变」与「请求数涨了」因此只能由这一点差别解释。
#[test]
pub(crate) fn retranslating_a_translated_block_by_id_re_sends_it_and_a_plain_retry_keeps_the_cache()
{
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let provider = Arc::new(MockMergeProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            // 自动运行打开：整本先真的翻一遍，目标块才会有一份阅读端会显示、缓存也会命中
            // 的译文 —— 手工种一行证明得了游标形状，证明不了「有译文还要重发」。
            true,
        ))
        .unwrap();
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();
    assert_eq!(
        job_row(&fixture, &job_id).status,
        db::index_jobs::IndexJobStatus::Succeeded,
        "整本必须先跑完"
    );

    let (unit_id, block_id, ordinal, unit_revision, source_text, segments) = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
        let block = blocks.first().expect("fixture 至少要有一个文本块");
        (
            block.unit_id.clone(),
            block.block_id.clone(),
            block.ordinal,
            block.unit_revision,
            block.source.text.clone(),
            block.source.segments.clone(),
        )
    };
    let stored_text = |ordinal: usize| {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap()
            .into_iter()
            .find(|row| row.ordinal == ordinal as u64)
            .map(|row| row.translated_text)
    };
    let answer = stored_text(ordinal).expect("整本跑完之后这一块必须有译文");
    let after_first_run = provider.requests.load(Ordering::SeqCst);
    assert!(after_first_run > 0, "整本跑完必须真的发过请求");

    // 普通重试：这一块已经有可显示的译文，缓存命中，一个请求都不发。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![ordinal]))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        after_first_run,
        "普通重试必须尊重缓存，否则每次修复失败块都会为整本书重复付费"
    );

    // 把这一块的行换成一个哨兵：新译文写回时，这一次覆盖才看得出来。
    let now = unix_timestamp().unwrap();
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: fixture.book_id.clone(),
                content_unit_id: unit_id.clone(),
                block_id: block_id.clone(),
                ordinal: ordinal as u64,
                document_revision: fixture.revision,
                unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("en".to_string()),
                model: "chat-test".to_string(),
                source_text,
                translated_text: serde_json::to_string(&StoredTranslation {
                    execution_identity: "translation-v1:chat-test".to_string(),
                    segments: segments
                        .iter()
                        .map(|segment| crate::ai::translation::TranslationSegment {
                            source: segment.clone(),
                            translated: "旧译文".to_string(),
                        })
                        .collect(),
                })
                .unwrap(),
                created_at: now,
                updated_at: now,
            },
        )
        .unwrap();
    }

    // 阅读端「重新翻译」：按块标识强制重跑这一块。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate_translation_block(&job_id, &unit_id, &block_id))
            .unwrap(),
        "块标识必须映射回这一块"
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();
    assert!(
        provider.requests.load(Ordering::SeqCst) > after_first_run,
        "强制重发必须真的发出请求，否则用户点了「重新翻译」什么也不会发生"
    );
    assert_eq!(
        stored_text(ordinal).as_deref(),
        Some(answer.as_str()),
        "新译文必须覆盖哨兵，而不是把旧行原样留下"
    );
    let cursor: JobCursor = serde_json::from_str(&job_row(&fixture, &job_id).cursor_json).unwrap();
    assert!(
        !cursor.force_retry && cursor.retry_ordinals.is_empty(),
        "这份收窄跑完之后强制重发必须随之失效，否则整本剩下的块都会被强制重发"
    );
}

/// 模型把整块原文原样抄回来时落下的行不算译文（现场 2026-10-15：`Type: Build …` 这类元
/// 信息段落被原样返回，系统当成有效译文落盘 —— 阅读端显示的仍是英文，而游标已经走过去、
/// 缓存又把它算作「已翻」，除了用户点「重新翻译」没有任何入口会再碰它）。
///
/// 现在它按「还没有译文」处理：重扫到这里要重新发请求，任务日志里留一条 `SourceEchoed`。
/// 同一本书里真正的译文不受影响，照旧缓存命中 —— 判定保守到「拼起来逐字相同」正是为了
/// 不把「保留人名、URL、代码片段」的正常翻译误判成没翻过。
#[test]
pub(crate) fn a_stored_translation_that_echoes_the_source_is_re_sent_instead_of_cache_hit() {
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let provider = Arc::new(MockMergeProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    // 两块各种一行：第 0 块是「原样抄回来」的，第 1 块是真译文。种下的原文与片段都取自
    // 任务自己的文本块列表，缓存键因此与 walk 算出来的一模一样（`translation_cache_key`）。
    let (echoed_ordinal, genuine_ordinal, total) = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
        assert!(blocks.len() >= 2, "这条用例需要至少两个文本块");
        let now = unix_timestamp().unwrap();
        for (index, echoed) in [(0usize, true), (1usize, false)] {
            let block = &blocks[index];
            let stored = StoredTranslation {
                execution_identity: "translation-v1:chat-test".to_string(),
                segments: block
                    .source
                    .segments
                    .iter()
                    .map(|segment| crate::ai::translation::TranslationSegment {
                        source: segment.clone(),
                        translated: if echoed {
                            segment.clone()
                        } else {
                            format!("已有译文{segment}")
                        },
                    })
                    .collect(),
            };
            db::translations::upsert(
                &conn,
                &db::translations::NewTranslation {
                    book_id: fixture.book_id.clone(),
                    content_unit_id: block.unit_id.clone(),
                    block_id: block.block_id.clone(),
                    ordinal: block.ordinal as u64,
                    document_revision: fixture.revision,
                    unit_revision: block.unit_revision,
                    target_language: "zh-Hans".to_string(),
                    source_language: Some("en".to_string()),
                    model: "chat-test".to_string(),
                    source_text: block.source.text.clone(),
                    translated_text: serde_json::to_string(&stored).unwrap(),
                    created_at: now,
                    updated_at: now,
                },
            )
            .unwrap();
        }
        (blocks[0].ordinal, blocks[1].ordinal, blocks.len())
    };
    finish_job_at(&fixture, &job_id, total);

    // 只点名真译文那一块：缓存命中，一个请求都不发。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![genuine_ordinal]))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "真译文照旧缓存命中，不花钱重发"
    );
    assert!(
        recorded_events(&fixture, &job_id)
            .iter()
            .any(|entry| entry.event == JobLogEvent::CacheHit
                && entry.metrics.ordinal == Some(genuine_ordinal as u64)),
        "真译文必须按缓存命中走过"
    );

    // 再点名「抄回来」的那一块：它不算译文，必须重新发给模型。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![echoed_ordinal]))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();
    assert!(
        provider.requests.load(Ordering::SeqCst) > 0,
        "原样抄回来的块必须重新发给模型，否则阅读端永远显示原文"
    );
    let echoed_events: Vec<_> = recorded_events(&fixture, &job_id)
        .into_iter()
        .filter(|entry| entry.event == JobLogEvent::SourceEchoed)
        .collect();
    assert_eq!(echoed_events.len(), 1, "抄回来必须留下一条记录");
    assert_eq!(
        echoed_events[0].metrics.ordinal,
        Some(echoed_ordinal as u64)
    );

    // 新请求的答案覆盖了那一行，并且不再被判成抄回来；真译文那一块没有被碰。
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let rows =
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap();
    assert_eq!(rows.len(), 2, "两块都必须有译文");
    let repaired = rows
        .iter()
        .find(|row| row.ordinal == echoed_ordinal as u64)
        .expect("抄回来的那一块必须有行");
    let stored: StoredTranslation = serde_json::from_str(&repaired.translated_text).unwrap();
    assert!(!stored.is_passthrough(), "重新翻过之后不再是抄回来的");
    let untouched = rows
        .iter()
        .find(|row| row.ordinal == genuine_ordinal as u64)
        .expect("真译文那一块必须有行");
    let stored: StoredTranslation = serde_json::from_str(&untouched.translated_text).unwrap();
    assert!(
        stored
            .segments
            .iter()
            .all(|segment| segment.translated.starts_with("已有译文")),
        "真译文那一块不该被重发或改写"
    );
}
