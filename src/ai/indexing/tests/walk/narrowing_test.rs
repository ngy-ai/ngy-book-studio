use super::*;

/// 收窄的重试不许把范围之外的块「跳过」掉：那一跳等于把它们记成已处理，而它们
/// 可能从来没有译文。
///
/// 现场 2026-09-20：第 70 块在 provider 500 之后失败，用户在明细里重试这一块；
/// 那次运行把 71 到 18168 全部按「不必处理」推了过去，任务显示「已完成 / 已处理
/// 18168」，而整本书只有 71 行译文 —— 阅读端因此整本没有译文可显示。范围之外只
/// 允许缓存命中，撞上没译文的块就停下，任务留在可继续的状态。
#[test]
pub(crate) fn a_narrowed_retry_stops_at_an_untranslated_block_outside_its_targets() {
    let provider = Arc::new(MockMergeProvider::default());
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            "translation-v2:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let total = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        translation_blocks(&conn, &fixture.book_id, &fixture.source_id)
            .unwrap()
            .len()
    };
    assert!(total >= 2, "这条用例需要至少两个文本块，实际 {total}");

    // 只重跑第 0 块，其余块都还没有译文。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap()
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
        .block_on(run_queued_job(&inner, queued))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let row = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(
        row.status,
        db::index_jobs::IndexJobStatus::Paused,
        "范围之外还有没翻译的块时，任务必须留在可继续的状态"
    );
    let cursor: JobCursor = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(
        cursor.next_ordinal, 1,
        "游标要停在第一个既没有译文、又不在重试范围里的块上"
    );
    assert!(
        cursor.retry_ordinals.is_empty(),
        "用完的重试集合必须从持久游标里清掉，否则以后每次运行都还会被它收窄"
    );
    assert!(cursor.inflight.is_empty());
    let rows =
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap();
    assert_eq!(rows.len(), 1, "只有被要求重试的那一块该留下译文");
    assert_eq!(rows[0].ordinal, 0);
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "范围之外的块绝不许替它发请求"
    );
    drop(conn);

    // 用户点「恢复」：重试集合已经清掉，这一次按整本书继续翻。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.resume(&job_id))
            .unwrap()
    );
    let queued = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::list_queued(&conn, 8)
            .unwrap()
            .into_iter()
            .find(|job| job.id == job_id)
            .expect("恢复之后的任务必须回到队列")
    };
    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let row = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(row.status, db::index_jobs::IndexJobStatus::Succeeded);
    let cursor: JobCursor = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(cursor.next_ordinal, total, "恢复之后要一直翻到书末");
    assert_eq!(
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap()
            .len(),
        total,
        "恢复之后每一块都要有译文"
    );
}

/// 范围之外已经翻过的块照常按缓存命中重挂，运行因此照旧走到书末：重试一块不会让
/// 整本书的进度倒退成「只处理到这一块」。
#[test]
pub(crate) fn a_narrowed_retry_republishes_cached_blocks_beyond_its_targets() {
    let provider = Arc::new(MockMergeProvider::default());
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            "translation-v2:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    // 给最后一块种一行真译文：它就是重试范围之外、但已经翻过的块。
    let (unit_id, unit_revision, block_id, source_text, segments, total) = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
        assert!(blocks.len() >= 2, "这条用例需要至少两个文本块");
        let block = &blocks[1];
        (
            block.unit_id.clone(),
            block.unit_revision,
            block.block_id.clone(),
            block.source.text.clone(),
            block.source.segments.clone(),
            blocks.len(),
        )
    };
    let stored = StoredTranslation {
        execution_identity: "translation-v2:chat-test".to_string(),
        segments: segments
            .iter()
            .map(|segment| crate::ai::translation::TranslationSegment {
                source: segment.clone(),
                translated: format!("译文{segment}"),
            })
            .collect(),
    };
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: fixture.book_id.clone(),
                content_unit_id: unit_id,
                block_id,
                ordinal: 1,
                document_revision: fixture.revision,
                unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("en".to_string()),
                model: "chat-test".to_string(),
                source_text,
                translated_text: serde_json::to_string(&stored).unwrap(),
                created_at: 10,
                updated_at: 10,
            },
        )
        .unwrap();
    }

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry_translation_blocks(&job_id, vec![0]))
            .unwrap()
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
        .block_on(run_queued_job(&inner, queued))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let row = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(
        row.status,
        db::index_jobs::IndexJobStatus::Succeeded,
        "范围之外都有译文可复用时，运行照旧走到书末"
    );
    let cursor: JobCursor = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(cursor.next_ordinal, total);
    assert!(cursor.retry_ordinals.is_empty());
    let rows =
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap();
    assert_eq!(rows.len(), total, "重挂之后每一块都还有译文");
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "范围之外的块只走缓存，不许再发请求"
    );
    assert!(
        recorded_events(&fixture, &job_id)
            .iter()
            .any(|entry| entry.event == JobLogEvent::CacheHit && entry.metrics.ordinal == Some(1)),
        "范围之外的块必须按缓存命中走过，而不是被当成「不必处理」"
    );
}
