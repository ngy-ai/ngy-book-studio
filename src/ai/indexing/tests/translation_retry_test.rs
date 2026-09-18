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

/// A requested ordinal whose block already has a translation must be dropped
/// from the set: retrying would otherwise overwrite good output. When every
/// requested block is already translated the task is not requeued at all,
/// because a run over the same set would send nothing.
#[test]
pub(crate) fn retrying_skips_blocks_that_already_have_a_translation() {
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

    // The already translated block drops out; the other one is retried.
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
    assert_eq!(cursor.retry_ordinals, vec![untranslated]);
    assert_eq!(cursor.next_ordinal, untranslated);

    // Naming only the translated block is a no-op, not a requeue.
    finish_job_at(&fixture, &job_id, total);
    assert!(
        !fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![translated]))
            .unwrap()
    );
    let untouched = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(untouched.status, db::index_jobs::IndexJobStatus::Succeeded);
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
