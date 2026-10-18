use super::*;

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
