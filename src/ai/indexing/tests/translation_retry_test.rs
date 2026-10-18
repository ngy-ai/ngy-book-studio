//! Integration tests of translation retries: the durable cursor rewind and
//! the per-block protocol retry budget.

use super::*;

use std::sync::{Arc, Mutex, atomic::Ordering};

use crate::{
    ai::OpenAiCompatibleProvider, ai::job_diagnostics::JobLogEvent,
    ai::translation::StoredTranslation, db,
};

/// 这条任务在队列里的那一行：`run_queued_job` 要的形状与 `list_queued` 给出的一致。
fn queued_job(fixture: &Fixture, job_id: &str) -> db::index_jobs::IndexJob {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::list_queued(&conn, 8)
        .unwrap()
        .into_iter()
        .find(|job| job.id == job_id)
        .unwrap_or_else(|| panic!("任务 {job_id} 必须回到队列"))
}

fn job_row(fixture: &Fixture, job_id: &str) -> db::index_jobs::IndexJob {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::get(&conn, job_id).unwrap().unwrap()
}

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
            NO_BATCHING,
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
            NO_BATCHING,
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
            NO_BATCHING,
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
/// per-block retry, and so must a queued one (no executor owns its cursor yet,
/// so the retry planned now is exactly what the next claim reads). A running
/// one must not: there the executor owns the cursor.
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
            NO_BATCHING,
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

    // 排队中的任务照样可以被重新规划（2026-10-15）：还没有执行器拥有这条游标，现在写下
    // 的收窄就是下一次领取读到的那一份。拒绝它的后果是用户点了「重试」却什么都没发生 ——
    // 现场 2026-10-15 整本翻译在跑时点「重新翻译这一块」，「这本书没有可重试的翻译任务」
    // 报的正是这条被拒的规划。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap(),
        "排队中的任务必须还能被重新规划"
    );
    let replanned = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(replanned.status, db::index_jobs::IndexJobStatus::Queued);
    let cursor: JobCursor = serde_json::from_str(&replanned.cursor_json).unwrap();
    assert_eq!(cursor.retry_ordinals, vec![0]);

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
            NO_BATCHING,
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

/// 收窄的重试在范围之外撞上没有译文的块：已经攒下的批必须照发，运行必须按「已暂停」发布。
///
/// 现场 2026-10-17：「立即翻译本章」把任务收窄到 53 块，其中 22 块要发请求，而端点的
/// 「每请求文本块数」是 32 —— 批永远攒不满就撞线。旧代码把整批作废，随后 `continue` 又跳过
/// 了收尾发布：main loop 从此既不能提交（游标不许越过窗口里还在等的块）也不能再开窗口
/// （窗口已满），变成一个没有任何 await 点的 100% 空转。任务窗口于是永远显示「运行中 ·
/// 已处理 226 · 处理中 22」，一个请求都没发过，重启也只会在同一处复现。
#[test]
pub(crate) fn a_narrowed_retry_sends_its_batch_before_stopping_and_publishes_paused() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    // 带请求计数的 mock：这条用例要证明的就是「撞线之前攒下的批真的发了」。
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
            // 远大于重试块数：批在撞线之前永远攒不满，正是现场那一版的端点设置。
            32,
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
    finish_job_at(&fixture, &job_id, total);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap()
    );
    let queued = queued_job(&fixture, &job_id);

    // 这一轮必须自己收尾：以前它会在这里永久空转（100% CPU、永远发布不出去）。
    fixture.runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            run_queued_job(&coordinator.inner, queued),
        )
        .await
        .expect("收窄的重试必须自己收尾，不能永久空转")
        .unwrap();
    });

    assert!(
        provider.requests.load(Ordering::SeqCst) > 0,
        "撞线之前攒下的批必须照发，否则整次重试一个请求都发不出去"
    );
    let row = job_row(&fixture, &job_id);
    assert_eq!(row.status, db::index_jobs::IndexJobStatus::Paused);
    assert!(row.error.is_none());
    let cursor: JobCursor = serde_json::from_str(&row.cursor_json).unwrap();
    assert!(cursor.inflight.is_empty(), "暂停的任务不许留着在飞的块");
    assert_eq!(cursor.next_ordinal, 1, "重试的那一块必须已经提交");
}

/// 撞线时一批都攒不出来（第一个块就在重试范围之外）：运行照样必须按「已暂停」发布。
///
/// 与上一条同一条 `continue`：没有在飞请求可等的时候继续循环，只会让 main loop 退化成
/// 没有任何 await 点的 100% 空转，任务永远停在「运行中」。
#[test]
pub(crate) fn a_narrowed_retry_with_nothing_to_send_still_publishes_paused() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
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
            32,
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
    finish_job_at(&fixture, &job_id, total);
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap()
    );

    // 游标已经越过重试范围（要重试的那一块在别处提交过）：这一轮第一个块就在范围之外，
    // 一批都攒不出来。
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let mut cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
        cursor.next_ordinal = 1;
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Queued,
            &cursor.encode().unwrap(),
            None,
            20,
            None,
            None,
        )
        .unwrap();
    }
    let queued = queued_job(&fixture, &job_id);

    fixture.runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            run_queued_job(&coordinator.inner, queued),
        )
        .await
        .expect("撞线且无批可发时必须按暂停发布，不能永久空转")
        .unwrap();
    });

    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "重试范围之外不许发请求"
    );
    let row = job_row(&fixture, &job_id);
    assert_eq!(row.status, db::index_jobs::IndexJobStatus::Paused);
    assert!(row.error.is_none());
    let cursor: JobCursor = serde_json::from_str(&row.cursor_json).unwrap();
    assert!(cursor.inflight.is_empty(), "暂停的任务不许留着在飞的块");
}
