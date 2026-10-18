use super::*;

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
