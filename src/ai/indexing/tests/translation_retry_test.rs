//! Integration tests of translation retries: the durable cursor rewind and
//! the per-block protocol retry budget.

use super::*;

use std::sync::{Arc, Mutex, atomic::Ordering};

use crate::{
    ai::OpenAiCompatibleProvider, ai::job_diagnostics::JobLogEvent,
    ai::translation::StoredTranslation, db,
};

/// Retrying named blocks rewinds the durable cursor to the first requested
/// ordinal and records the bounded set. Reproduces the state a finished run
/// leaves behind (cursor at the end of the book) instead of driving a whole
/// translation, because that end state is exactly what makes the retry the
/// only way back to a skipped block.
#[test]
pub(crate) fn retrying_named_blocks_rewinds_the_cursor_and_narrows_the_walk() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let total = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job_id))
        .unwrap()
        .total;
    assert!(total >= 2, "fixture needs two blocks to retry: {total}");
    let (first, second) = (0usize, 1usize);

    // Park the job where a run that skipped the last blocks ends: succeeded,
    // cursor at the end of the book.
    finish_job_at(&fixture, &job_id, total);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![second, first, first]))
            .unwrap()
    );

    let retrying = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    // Back to queued with the cursor parked on the first requested block, and
    // the walk bounded to the requested set (sorted and deduplicated).
    assert_eq!(retrying.status, db::index_jobs::IndexJobStatus::Queued);
    assert!(retrying.error.is_none());
    let cursor: JobCursor = serde_json::from_str(&retrying.cursor_json).unwrap();
    assert_eq!(cursor.next_ordinal, first);
    assert_eq!(cursor.retry_ordinals, vec![first, second]);

    // An empty set is not a way to requeue the whole task.
    assert!(
        !fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, Vec::new()))
            .unwrap()
    );
    // Another job kind is refused instead of rewritten.
    assert!(
        !fixture
            .runtime
            .block_on(
                coordinator
                    .retry_translation_blocks(&format!("embedding:{}", fixture.source_id), vec![0])
            )
            .unwrap()
    );
}

/// 重试请求按原样排队，**不做前置过滤**：已有译文的块由 walk 按内容缓存键命中、不发
/// 请求，所以「不用重发」这件事本来就由 walk 判定。用 ordinal 预先剔除会与 walk 的判定
/// 不一致 —— 剔除错了的后果是用户点了「重试」而一个请求都不发（现场 2026-09-22）。
#[test]
pub(crate) fn retrying_queues_the_requested_blocks_and_leaves_the_decision_to_the_walk() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let total = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job_id))
        .unwrap()
        .total;
    assert!(total >= 2, "fixture needs two blocks: {total}");
    let (translated, untranslated) = (0usize, 1usize);

    // Seed a finished row for the first block, keyed to the same revision and
    // unit revision the retry reads.
    let unit_revision = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::content_units::list_for_source(&conn, &fixture.source_id)
            .unwrap()
            .into_iter()
            .find(|unit| unit.id == fixture.unit_id)
            .expect("fixture unit must exist")
            .revision
    };
    let now = unix_timestamp().unwrap();
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: fixture.book_id.clone(),
                content_unit_id: fixture.unit_id.clone(),
                block_id: format!("{}:block", fixture.unit_id),
                ordinal: translated as u64,
                document_revision: fixture.revision,
                unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("en".to_string()),
                model: "chat-test".to_string(),
                source_text: "already translated".to_string(),
                translated_text: serde_json::to_string(&StoredTranslation {
                    execution_identity: "translation-v1:chat-test".to_string(),
                    segments: vec![crate::ai::translation::TranslationSegment {
                        source: "already translated".to_string(),
                        translated: "已有译文".to_string(),
                    }],
                })
                .unwrap(),
                created_at: now,
                updated_at: now,
            },
        )
        .unwrap();
    }
    finish_job_at(&fixture, &job_id, total);

    // 两块都进集合：已有译文的那一块由 walk 判成缓存命中，不会重发。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![translated, untranslated]))
            .unwrap()
    );
    let retrying = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    let cursor: JobCursor = serde_json::from_str(&retrying.cursor_json).unwrap();
    assert_eq!(cursor.retry_ordinals, vec![translated, untranslated]);
    assert_eq!(cursor.next_ordinal, translated);

    // 只点名已有译文的那一块也照样排队：要不要发请求不是这里说了算。
    finish_job_at(&fixture, &job_id, total);
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![translated]))
            .unwrap()
    );
    let queued = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(queued.status, db::index_jobs::IndexJobStatus::Queued);
    let cursor: JobCursor = serde_json::from_str(&queued.cursor_json).unwrap();
    assert_eq!(cursor.retry_ordinals, vec![translated]);
}

/// 重试必须真的把请求发出去：一块只有「阅读端看不到」的译文行（别的模型与执行身份写的）
/// 时，按 ordinal 预先剔除会把它整掉，用户点了「重试」也没有任何请求 —— 日志里既没有
/// 请求体也没有响应（现场 2026-09-22）。这条用例跑到请求计数，钉住「请求确实发了」。
#[test]
pub(crate) fn retrying_sends_a_request_for_a_block_whose_stored_row_is_not_displayable() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    // 带请求计数的 mock：这条用例要证明的就是「真的发了请求」。
    let provider = Arc::new(MockMergeProvider::default());
    let translation_provider: Arc<dyn OpenAiCompatibleProvider> = provider.clone();
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            translation_provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let total = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job_id))
        .unwrap()
        .total;
    assert!(total >= 2, "fixture needs two blocks: {total}");
    let unit_revision = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::content_units::list_for_source(&conn, &fixture.source_id)
            .unwrap()
            .into_iter()
            .find(|unit| unit.id == fixture.unit_id)
            .expect("fixture unit must exist")
            .revision
    };
    let now = unix_timestamp().unwrap();
    // 第 0 块有一行译文，但属于另一套模型与执行身份：阅读端不会显示它。
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: fixture.book_id.clone(),
                content_unit_id: fixture.unit_id.clone(),
                block_id: format!("{}:block", fixture.unit_id),
                ordinal: 0,
                document_revision: fixture.revision,
                unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("en".to_string()),
                model: "other-model".to_string(),
                source_text: "block".to_string(),
                translated_text: serde_json::to_string(&StoredTranslation {
                    execution_identity: "translation-v1:other-model".to_string(),
                    segments: vec![crate::ai::translation::TranslationSegment {
                        source: "block".to_string(),
                        translated: "别的模型写的译文".to_string(),
                    }],
                })
                .unwrap(),
                created_at: now,
                updated_at: now,
            },
        )
        .unwrap();
    }
    finish_job_at(&fixture, &job_id, total);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap(),
        "这一块没有阅读端能显示的译文，重试必须排队"
    );
    let queued = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::list_queued(&conn, 8)
            .unwrap()
            .into_iter()
            .find(|job| job.id == job_id)
            .expect("重试之后的任务必须回到队列")
    };
    fixture
        .runtime
        .block_on(run_queued_job(&coordinator.inner, queued))
        .unwrap();
    assert!(
        provider.requests.load(Ordering::SeqCst) > 0,
        "重试必须真的发请求，否则用户看不到任何请求体与响应"
    );
}

/// New translation jobs start paused, and repairing a failed block before
/// resuming the task is the normal case — so a paused task must accept a
/// per-block retry. A running one must not: the executor owns its cursor.
#[test]
pub(crate) fn retrying_works_on_a_paused_task_and_refuses_a_running_one() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    // `auto_run: false` leaves the job paused, which is the product default.
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let paused = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(paused.status, db::index_jobs::IndexJobStatus::Paused);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap()
    );
    let queued = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(queued.status, db::index_jobs::IndexJobStatus::Queued);

    // A running job is left alone: its cursor belongs to the executor.
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Running,
            &queued.cursor_json,
            None,
            unix_timestamp().unwrap(),
            Some(unix_timestamp().unwrap()),
            None,
        )
        .unwrap();
    }
    assert!(
        !fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap()
    );
}

#[test]
pub(crate) fn a_cursor_from_older_block_rules_restarts_the_book() {
    // 提取文本块的规则变化会改变块数，旧游标的序号不再指向同一块。翻译任务因此退回
    // 书首重扫（已存译文仍按缓存键命中，重扫不花请求），窗口与按块重试的目标一并清空；
    // 版本已经是最新的游标则原样保留。
    let provider = Arc::new(MockMergeProvider::default());
    let (fixture, job, cursor, _translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let write_cursor = |cursor: &JobCursor| {
        conn.execute(
            "UPDATE index_jobs SET cursor_json = ?1 WHERE id = ?2",
            rusqlite::params![cursor.encode().unwrap(), job.id],
        )
        .unwrap();
    };
    let rebound = || {
        let row = db::index_jobs::get(&conn, &job.id).unwrap().unwrap();
        JobCursor::from_job(&row, "chat-test", Some("translation-v2:chat-test")).unwrap()
    };

    let mut stale = cursor.clone();
    stale.next_ordinal = 7;
    stale.retry_ordinals = vec![9];
    stale.inflight = vec![8];
    stale.blocks_version = TRANSLATION_BLOCKS_VERSION - 1;
    write_cursor(&stale);
    let reset = rebound();
    assert_eq!(reset.next_ordinal, 0);
    assert!(reset.retry_ordinals.is_empty());
    assert!(reset.inflight.is_empty());
    assert_eq!(reset.blocks_version, TRANSLATION_BLOCKS_VERSION);

    let mut current = reset;
    current.next_ordinal = 5;
    current.retry_ordinals = vec![6];
    write_cursor(&current);
    let kept = rebound();
    assert_eq!(kept.next_ordinal, 5);
    assert_eq!(kept.retry_ordinals, vec![6]);
}

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

/// 「立即翻译本章」把推进点借给一章：收窄到**本章**的块、记下借出前的位置、写下插队
/// 优先级，并把任务重新排队。收窄结束之后回到借出位置由 walk 的用例固定
/// （`translation_walk_test::a_chapter_jump_returns_the_cursor_to_the_borrowed_position`）。
#[test]
pub(crate) fn translating_a_chapter_narrows_the_walk_to_that_chapter_and_borrows_its_position() {
    let mut fixture = Fixture::new();
    let second_unit = fixture.add_second_unit("第二章");
    // 模型快照活在 `IndexingInner` 里，而 `Fixture::indexing_inner()` 每次都新建一个：
    // 配置与规划必须走同一个 inner，否则规划路径看不到翻译模型（会静默返回 false）。
    let inner = fixture.indexing_inner();
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v2:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
    let chapter: Vec<usize> = blocks
        .iter()
        .filter(|block| block.unit_id == second_unit)
        .map(|block| block.ordinal)
        .collect();
    let other: Vec<usize> = blocks
        .iter()
        .filter(|block| block.unit_id != second_unit)
        .map(|block| block.ordinal)
        .collect();
    assert!(!chapter.is_empty(), "第二章必须有文本块");
    assert!(!other.is_empty(), "第一章必须有文本块");
    let cursor: JobCursor = serde_json::from_str(
        &db::index_jobs::get(&conn, &job_id)
            .unwrap()
            .expect("配置译文语言必须建出这一任务")
            .cursor_json,
    )
    .unwrap();
    let borrowed = cursor.next_ordinal;
    assert!(
        chapter.iter().all(|ordinal| *ordinal > borrowed),
        "要借的位置在第二章之前，用例才有区分度"
    );
    // 运行中的执行器拥有游标，规划之前必须先让它落盘 —— 服务层会先请求暂停。
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Paused,
        &cursor.encode().unwrap(),
        None,
        unix_timestamp().unwrap(),
        Some(1),
        None,
    )
    .unwrap();

    assert!(
        fixture
            .runtime
            .block_on(coordinator.translate_translation_chapter(&job_id, &second_unit))
            .unwrap()
    );

    let planned = {
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.status, db::index_jobs::IndexJobStatus::Queued);
        assert!(job.error.is_none());
        let cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
        cursor
    };
    assert_eq!(planned.retry_ordinals, chapter, "收窄的必须正好是本章的块");
    assert_eq!(planned.next_ordinal, chapter[0]);
    assert_eq!(
        planned.resume_ordinal,
        Some(borrowed),
        "借出推进点之前的位置必须留下来"
    );
    assert!(planned.priority.is_some(), "插队优先级必须写进游标");
    assert!(
        other
            .iter()
            .all(|ordinal| !planned.retry_ordinals.contains(ordinal)),
        "别的章不该进收窄范围"
    );

    // 找不到这一章就报错，绝不顺手挑别的块充数。
    let error = fixture
        .runtime
        .block_on(coordinator.translate_translation_chapter(&job_id, "unit-missing"))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("找不到这一章的文本块"),
        "{error:#}"
    );
}
