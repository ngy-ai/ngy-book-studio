use super::*;

/// 重配置路径写出的持久游标是「较小的形状」（没有 `inflight`/`retry_ordinals`），
/// 而领取后的每次比较都是 JSON 字符串相等。领取时必须先把它重写成规范编码，否则任务
/// 第一次运行立刻判成 `Abandoned`：行留在 `running`、`attempts` 已加一、没有任何执行者
/// —— 现场 2026-09-17，新导入的图书点「恢复」后一直显示运行中，已处理 0、处理中 0，
/// 刷新也不动。重写只补运行态字段，**不得改动 `next_ordinal`**：那条路径已经把位置和
/// 它的 `blocks_version` 一起带过来了（见 `translation_cursor_json`）。
#[test]
pub(crate) fn a_reconfigured_translation_job_runs_instead_of_abandoning_its_first_execution() {
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
    let row = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(row.status, db::index_jobs::IndexJobStatus::Paused);
    let persisted: serde_json::Value = serde_json::from_str(&row.cursor_json).unwrap();
    assert!(
        persisted.get("inflight").is_none() && persisted.get("retry_ordinals").is_none(),
        "重配置路径写的持久游标没有运行态字段：{}",
        row.cursor_json
    );
    let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
    let translatable = blocks
        .iter()
        .filter(|block| !block.source.segments.is_empty())
        .count();
    assert!(translatable > 0, "这条用例需要至少一个可翻译的文本块");
    drop(conn);

    // 用户点「恢复」：任务回到队列，worker 领取后必须真的开始翻。
    assert!(
        fixture
            .runtime
            .block_on(coordinator.resume(&job_id))
            .unwrap()
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let queued = db::index_jobs::list_queued(&conn, 8)
        .unwrap()
        .into_iter()
        .find(|job| job.id == job_id)
        .expect("恢复后的翻译任务必须回到队列");
    drop(conn);

    fixture
        .runtime
        .block_on(run_queued_job(&inner, queued))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let row = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(
        row.status,
        db::index_jobs::IndexJobStatus::Succeeded,
        "领取时重写成规范游标之后第一次执行必须翻完，而不是被判成已被替换：{:?}",
        row.error
    );
    let finished: serde_json::Value = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(finished["next_ordinal"], serde_json::json!(blocks.len()));
    assert_eq!(finished["inflight"], serde_json::json!([]));
    assert_eq!(
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap()
            .len(),
        translatable,
        "每个可翻译的文本块都要留下一行译文"
    );
}

/// 暂停之后不许继续声称有请求在飞：窗口清空必须落盘，而且发布的结果必须就是清空后
/// 的那份游标 —— 否则发布时会按旧 JSON 匹配不上，任务卡在「处理中」。
#[test]
pub(crate) fn a_paused_run_clears_a_window_left_by_a_previous_execution() {
    let provider = Arc::new(MockMergeProvider::default());
    let (fixture, job, cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    // 上一次执行被进程杀掉时留下的窗口，加上用户随后点的暂停。
    let mut stale = cursor.clone();
    stale.inflight = vec![0, 1];
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state_from(
        &conn,
        &job.id,
        db::index_jobs::IndexJobStatus::Running,
        db::index_jobs::IndexJobStatus::Running,
        &stale.encode().unwrap(),
        None,
        2,
        None,
        None,
    )
    .unwrap();
    db::index_jobs::request_pause(&conn, &job.id, 3).unwrap();
    let mut stats = TranslationRunStats::default();

    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &fixture.indexing_inner(),
            &job,
            &translation,
            "zh-Hans",
            &mut stale,
            &mut stats,
        ))
        .expect("暂停不是失败");

    let RunOutcome::Paused(paused) = outcome else {
        panic!("暂停请求必须按 Paused 收尾");
    };
    assert!(paused.inflight.is_empty(), "暂停的任务不许留有在飞的块");
    let row = db::index_jobs::get(&conn, &job.id).unwrap().unwrap();
    assert_eq!(
        paused.encode().unwrap(),
        row.cursor_json,
        "发布的结果必须与落盘的游标一致，否则任务永远发布不出去"
    );
    let saved: serde_json::Value = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(saved["inflight"], serde_json::json!([]));
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
}
