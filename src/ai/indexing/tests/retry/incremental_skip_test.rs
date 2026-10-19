use super::*;

use std::sync::{Arc, Mutex, atomic::Ordering};

use crate::ai::translation::StoredTranslation;

/// 给当前修订的每个文本块种一行可显示的机器译文，返回 `(unit, block, ordinal)` 列表。
///
/// 这些行与 `configure_translation` 写下的模型/执行身份完全相同，所以行走的缓存规则
/// 一定命中——用例要证明的正是「命中之后不再花请求」，而不是「缓存为什么不命中」。
fn seed_translation_rows(fixture: &Fixture, identity: &str) -> Vec<(String, String, usize)> {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
    assert!(blocks.len() >= 2, "这条用例需要至少两个文本块");
    let now = unix_timestamp().unwrap();
    for block in &blocks {
        let stored = StoredTranslation {
            execution_identity: identity.to_string(),
            segments: block
                .source
                .segments
                .iter()
                .map(|segment| crate::ai::translation::TranslationSegment {
                    source: segment.clone(),
                    translated: format!("已有译文·{segment}"),
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
    blocks
        .into_iter()
        .map(|block| (block.unit_id, block.block_id, block.ordinal))
        .collect()
}

fn translated_rows(fixture: &Fixture) -> Vec<String> {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
        .unwrap()
        .into_iter()
        .map(|row| row.translated_text)
        .collect()
}

/// 取消/失败后点「重试」是「从断点继续整本」，不是重做上一次没收尾的收窄。
///
/// 现场（2026-10-19）：逐块「重新翻译」跑到一半被取消，游标里还留着
/// `retry_ordinals` + `force_retry`；点「重试」后那一块（已有译文）又被强制重发。
/// 重试必须把这份收窄连同强制标记一起作废，并把游标留在当前推进点。
#[test]
pub(crate) fn retrying_a_cancelled_task_drops_the_leftover_narrowing_and_sends_nothing() {
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
    let seeded = seed_translation_rows(&fixture, "translation-v1:chat-test");
    let total = seeded.len();
    let before = translated_rows(&fixture);

    // 一次逐块重译被取消后的样子：推进点在书末，收窄与强制标记还没被清掉。
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let mut cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
        cursor.next_ordinal = total;
        cursor.retry_ordinals = vec![0];
        cursor.resume_ordinal = Some(0);
        cursor.force_retry = true;
        cursor.blocks_version = TRANSLATION_BLOCKS_VERSION;
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Cancelled,
            &cursor.encode().unwrap(),
            None,
            unix_timestamp().unwrap(),
            Some(1),
            Some(unix_timestamp().unwrap()),
        )
        .unwrap();
    }

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry(&job_id))
            .unwrap(),
        "取消的翻译任务必须能被重试"
    );
    let planned: JobCursor = serde_json::from_str(&job_row(&fixture, &job_id).cursor_json).unwrap();
    assert!(
        planned.retry_ordinals.is_empty(),
        "重试不能沿用上一次没收尾的收窄"
    );
    assert!(!planned.force_retry, "强制重发必须随收窄一起作废");
    assert!(planned.resume_ordinal.is_none(), "借出点也要一起清掉");
    assert_eq!(
        planned.next_ordinal, total,
        "推进点必须留在断点，不能退回书首"
    );

    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "断点之前全是已有译文的块，重试一个请求都不该发"
    );
    assert_eq!(
        job_row(&fixture, &job_id).status,
        db::index_jobs::IndexJobStatus::Succeeded
    );
    assert_eq!(translated_rows(&fixture), before, "已有译文不能被改写");
}

/// 「立即翻译本章」对本章已经翻完的书不花钱：所有块按缓存命中推进，一个请求都不发。
#[test]
pub(crate) fn a_chapter_jump_over_translated_blocks_keeps_them_cached() {
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
    let seeded = seed_translation_rows(&fixture, "translation-v1:chat-test");
    let total = seeded.len();
    finish_job_at(&fixture, &job_id, total);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.translate_translation_chapter(&job_id, &fixture.unit_id))
            .unwrap(),
        "本章必须能排到队首"
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();

    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "本章已有译文，立即翻译本章不能重新请求任何一块"
    );
    let finished: JobCursor =
        serde_json::from_str(&job_row(&fixture, &job_id).cursor_json).unwrap();
    assert_eq!(
        finished.next_ordinal, total,
        "本章跑完必须回到原来的推进点（书末）"
    );
}

/// 阅读端逐块「重新翻译」只强制重发这一块；同一本书里其它已译块一律按缓存复用。
#[test]
pub(crate) fn retranslating_one_block_only_sends_that_block() {
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
    let seeded = seed_translation_rows(&fixture, "translation-v1:chat-test");
    let total = seeded.len();
    finish_job_at(&fixture, &job_id, total);
    let before = translated_rows(&fixture);
    // 故意挑第二块：证明只按这一个序号收窄，而不是退回书首重扫。
    let (unit_id, block_id, ordinal) = seeded[1].clone();
    assert_ne!(ordinal, 0, "第二块的序号不能是 0，否则这条用例证明不了收窄");

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate_translation_block(&job_id, &unit_id, &block_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();

    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "只允许目标块发出一次请求，其它已译块一块都不能重发"
    );
    let after = translated_rows(&fixture);
    assert_eq!(after.len(), before.len(), "译文行数不能变");
    let changed = after
        .iter()
        .zip(before.iter())
        .filter(|(new, old)| new != old)
        .count();
    assert_eq!(changed, 1, "只有目标块那一行被新译文覆盖");
    let finished: JobCursor =
        serde_json::from_str(&job_row(&fixture, &job_id).cursor_json).unwrap();
    assert_eq!(
        finished.next_ordinal, total,
        "只跑目标块：跑完回到原来的推进点，后面的块不进这一轮"
    );
    assert!(finished.retry_ordinals.is_empty(), "收窄用完必须清掉");
    assert!(
        !finished.stops_after_targets,
        "「跑完就停」也要随收窄一起清掉"
    );
}

/// 目标块还在未翻译区域（推进点停在它之前）时，跑完要回到那个推进点；否则它前面的
/// 未译块会被整本任务跳过，再也不会有入口处理它们。
#[test]
pub(crate) fn retranslating_a_block_ahead_of_the_frontier_returns_to_the_frontier() {
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
    let seeded = seed_translation_rows(&fixture, "translation-v1:chat-test");
    let total = seeded.len();
    finish_job_at(&fixture, &job_id, total);
    // 推进点退到书首：目标块（第二块）还在这条推进线之前没被处理过。
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let mut cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
        cursor.next_ordinal = 0;
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Queued,
            &cursor.encode().unwrap(),
            None,
            unix_timestamp().unwrap(),
            Some(1),
            None,
        )
        .unwrap();
    }
    let (unit_id, block_id, ordinal) = seeded[1].clone();
    assert!(ordinal > 0);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate_translation_block(&job_id, &unit_id, &block_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();

    assert_eq!(provider.requests.load(Ordering::SeqCst), 1, "只重发目标块");
    let finished: JobCursor =
        serde_json::from_str(&job_row(&fixture, &job_id).cursor_json).unwrap();
    assert_eq!(
        finished.next_ordinal, 0,
        "跑完要回到原来的推进点，不能越过目标块前面的未译块"
    );
    assert!(!finished.stops_after_targets);
}

/// 目标块之后还没翻译的块不能把这一轮「拖」成暂停，也不该被算进任务范围。
///
/// 现场（2026-10-19）：点一块「重新翻译」，任务窗口里这一块之后的所有块都显示成
/// 这一轮的范围；整本没翻完时运行还会停在下一个未译块上。逐块重译只该做这一块。
#[test]
pub(crate) fn retranslating_one_block_ignores_untranslated_blocks_after_it() {
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
    let seeded = seed_translation_rows(&fixture, "translation-v1:chat-test");
    let total = seeded.len();
    finish_job_at(&fixture, &job_id, total);
    // 只留目标块（第一块）的译文，后面的块当作「还没翻」。
    let (unit_id, block_id, ordinal) = seeded[0].clone();
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        conn.execute(
            "DELETE FROM translations
             WHERE book_id = ?1 AND target_language = 'zh-Hans' AND ordinal > ?2",
            rusqlite::params![fixture.book_id, ordinal as i64],
        )
        .unwrap();
    }
    assert!(total > 1, "这条用例需要目标块之后还有别的块");

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate_translation_block(&job_id, &unit_id, &block_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued_job(&fixture, &job_id)))
        .unwrap();

    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "目标块之后没有译文的块不该被拖进这一轮"
    );
    assert_eq!(
        job_row(&fixture, &job_id).status,
        db::index_jobs::IndexJobStatus::Succeeded,
        "单块重译跑完就该收尾，不能因为后面的块没译文而停成暂停"
    );
    let finished: JobCursor =
        serde_json::from_str(&job_row(&fixture, &job_id).cursor_json).unwrap();
    assert_eq!(
        finished.next_ordinal, total,
        "推进点回到原处，后面的块仍然留在整本任务里等以后处理"
    );
    assert!(finished.retry_ordinals.is_empty());
    assert!(!finished.stops_after_targets);
}
