use super::*;

/// 个别文本块的请求失败不再让整次运行失败：这一块被跳过（原文保留、游标照常前进），
/// 后面的块继续翻完，任务按 `Succeeded` 收尾。
///
/// 现场 2026-09-22：一块的请求撞上端点抖动，整本图书的翻译立刻失败，已经翻好的部分
/// 停在半路 —— 用户要的是「跳过这一块，继续翻下一个」。
#[test]
pub(crate) fn a_failed_block_request_is_skipped_and_the_run_finishes() {
    let provider = Arc::new(MockMergeProvider::default());
    // 只让第一发请求失败：并发 1，请求严格按序号发出，所以它就是序号 0 那一块。
    provider.fail_requests.store(1, Ordering::SeqCst);
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    let blocks = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap()
    };
    assert!(
        blocks.len() >= 2,
        "这条用例需要至少两个文本块，实际 {}",
        blocks.len()
    );
    let mut stats = TranslationRunStats::default();

    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("个别块的请求失败不得让整次运行失败");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("跳过一个块之后运行必须走完并按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, blocks.len());
    assert_eq!(stats.untranslated, 1);
    assert_eq!(stats.saved, blocks.len() - 1);

    // 失败的那一块没有译文，其余块都有：跳过不等于整本停下。
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let rows = db::translations::list_for_unit(&conn, &fixture.unit_id, "zh-Hans").unwrap();
    assert_eq!(rows.len(), blocks.len() - 1);

    // 日志里必须留下可读记录，块号指向失败的那一块，任务窗口据此把它列进「失败」。
    let events = recorded_events(&fixture, &job.id);
    let skipped: Vec<_> = events
        .iter()
        .filter(|entry| entry.event == JobLogEvent::ProviderSkipped)
        .collect();
    assert_eq!(skipped.len(), 1, "跳过必须留下记录：{events:#?}");
    assert_eq!(skipped[0].metrics.ordinal, Some(0));
    assert_eq!(
        skipped[0].metrics.error_kind,
        Some(JobLogErrorKind::Unknown),
        "mock 的 provider 失败没有更细的分类"
    );
    assert!(
        !events
            .iter()
            .any(|entry| entry.event == JobLogEvent::ProtocolSkipped),
        "请求级失败不是协议失败，两者不能混记"
    );
}

/// 端点彻底联系不上时，整本书的每一个块都失败：运行必须一路走到书末，每连续
/// `TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT` 块失败只冷却一次，冷却之后连续计数清零、
/// 下一组重新累计。任务按 `Succeeded` 收尾，每个失败块各自留一条 `ProviderSkipped`，
/// 用户在模型或端点修好之后用「重试失败块」重跑它们。
///
/// 2026-09-24 用户裁定：原来「连续 5 块请求失败就中止整次运行」是错的。现场一次端点抖动
/// 让任务在连续 5 块失败之后按 `Failed` 收尾（已处理 3045 块、还有 4921 块没翻），已经
/// 翻好的部分停在半路，剩下的块再也没人翻。
///
/// 这里只固定「冷却触发与计数清零」的语义，冷却时长用 fixture 的短值
/// （`TEST_TRANSLATION_FAILURE_COOLDOWN`）；生产那 60 秒真的等满一次，由
/// `translation_flow` 的端到端用例断言。
#[test]
pub(crate) fn consecutive_failed_block_requests_cool_down_and_do_not_stop_the_run() {
    let provider = Arc::new(MockMergeProvider::default());
    provider.fail_provider.store(true, Ordering::SeqCst);
    let mut fixture = Fixture::new();
    // 默认模板只有两个文本块；这条用例要凑出两次冷却，所以正文要够长。
    fixture.rewrite_first_unit(
        "<p>One</p><p>Two</p><p>Three</p><p>Four</p><p>Five</p><p>Six</p><p>Seven</p>\
         <p>Eight</p><p>Nine</p><p>Ten</p><p>Eleven</p><p>Twelve</p><p>Thirteen</p>\
         <p>Fourteen</p>",
    );

    let (fixture, job, mut cursor, translation) =
        translation_block_fixture_in(fixture, &provider, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    let mut stats = TranslationRunStats::default();

    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("块级请求失败不许让整次运行失败");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("整本书逐块跳过之后必须按 Succeeded 收尾");
    };

    // 份数以任务窗口读到的块清单为准：walk 按 EPUB 投影枚举文本块，与只查库的那份抽取
    // 在编辑过的来源上差一块。
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let total = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job.id))
        .unwrap()
        .total;
    assert!(
        total > TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT,
        "这条用例要越过原来的「连续若干块就停」上限，实际 {total}"
    );

    // 每一块都真的发过请求、都被跳过、都提交过：游标一路走到书末。
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        total,
        "跳过一块之后照旧给后面的块发请求"
    );
    assert_eq!(stats.untranslated, total, "每一块都按跳过提交过");
    assert_eq!(stats.saved, 0, "一块都没翻出来");
    assert_eq!(finished.next_ordinal, total);
    assert!(finished.inflight.is_empty());
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let row = db::index_jobs::get(&conn, &job.id).unwrap().unwrap();
    let saved: serde_json::Value = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(saved["next_ordinal"], serde_json::json!(total));
    assert_eq!(saved["inflight"], serde_json::json!([]));
    assert_eq!(
        db::translations::list_for_unit(&conn, &fixture.unit_id, "zh-Hans")
            .unwrap()
            .len(),
        0,
        "一块都没翻出来"
    );
    assert_eq!(
        recorded_events(&fixture, &job.id)
            .iter()
            .filter(|entry| entry.event == JobLogEvent::ProviderSkipped)
            .count(),
        total,
        "每个被跳过的块都要留下记录，任务窗口据此列出「重试失败块」"
    );

    // 每连续 5 块失败冷却一次，冷却之后连续计数清零：冷却次数 = floor((块数 - 1) / 阈值)。
    // 减一是因为凑满阈值的那一块如果是最后一块，就没有下一个请求要发了，那次冷却按设计
    // 不等待；而「清零后重新累计」意味着第 11、12 块不会在第 10 块的冷却之后再触发一次。
    // 冷却真的等满 60 秒由 `translation_flow` 的端到端用例断言（这条用例用 fixture 的短
    // 冷却，只为固定触发与计数语义，不拖长单元测试）。
    assert_eq!(
        stats.cooldowns,
        (total - 1) / TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT,
        "每连续 {} 块失败冷却一次，冷却后连续计数清零",
        TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT
    );
    // 冷却同时进任务日志（2026-10-15）：任务窗口要能看出「任务不是死了，是在等端点
    // 缓过来」。书末那次按设计不等待，因此也不留记录。
    assert_eq!(
        recorded_events(&fixture, &job.id)
            .iter()
            .filter(|entry| entry.event == JobLogEvent::Cooldown)
            .count(),
        stats.cooldowns,
        "每一次真正等过的冷却都要在任务日志里留下一条"
    );
}

/// 「立即翻译本章」借出的推进点必须还回来：最后一个目标块提交时 `next_ordinal` 回到借出
/// 之前的位置，借出点与本章之间的块因此照常被翻，而不是被永久跳过。同类的事故见
/// 2026-09-20：只重试一块，其余几千块被当成「不必处理」一路推过去。
#[test]
pub(crate) fn a_chapter_jump_returns_the_cursor_to_the_borrowed_position() {
    let provider = Arc::new(MockMergeProvider::default());
    let mut fixture = Fixture::new();
    // 要有一段「借出点」和一段「本章」才能区分「回到原位」与「跟着跳过去」：
    // 默认模板只有两块，这里把第一章写长。
    fixture.rewrite_first_unit("<p>One</p><p>Two</p><p>Three</p><p>Four</p><p>Five</p><p>Six</p>");
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture_in(fixture, &provider, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    let scheduler = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    scheduler.configure_scheduling(1, Duration::ZERO);
    // 块数只认执行器那一份提取结果（任务窗口读的也是它）。
    let total = fixture
        .runtime
        .block_on(scheduler.translation_blocks(&job.id))
        .unwrap()
        .total;
    assert!(total >= 4, "这条用例需要至少四个文本块，实际 {total}");

    // 第三、四块被借走，借出点是书首：这一章跑完之后 walk 必须回到 0 把前两块补上。
    cursor.next_ordinal = 2;
    cursor.retry_ordinals = vec![2, 3];
    cursor.resume_ordinal = Some(0);
    // 运行只认「库里那份就是我自己」的游标，所以先把收窄后的游标落盘（规划路径正是这么写的）。
    {
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
    }
    let mut stats = TranslationRunStats::default();
    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("回到原位之后继续翻完整本，不该失败");

    let finished = match outcome {
        // 收窄跑完回到借出位置之后，这一章之外还没翻的块不在本次范围里，运行按既有的
        // 有界规则停在借出位置并按「已暂停」发布（绝不把它们当成「不必处理」推过去）。
        RunOutcome::Paused(cursor) => cursor,
        _ => panic!("有界运行只在借出的位置上收尾"),
    };
    assert!(
        finished.retry_ordinals.is_empty(),
        "最后一个目标提交时就要清掉范围"
    );
    assert!(
        finished.resume_ordinal.is_none(),
        "回到位置之后不该再留着它"
    );
    assert_eq!(
        finished.next_ordinal, 0,
        "必须回到借出位置（书首），而不是停在借出范围之后"
    );
    assert_eq!(stats.saved, 2, "只翻借出的那两块，别的块一块都不碰");

    // 借出位置之前的块仍然没有译文：任务停在那里，继续整本时从那里接着翻。
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let rows =
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap();
    assert_eq!(rows.len(), 2, "本轮只翻借出的两块");

    // 回到原位置之后按普通运行继续：没有任何收窄，剩下的块照常翻完。
    let row = db::index_jobs::get(&conn, &job.id).unwrap().unwrap();
    db::index_jobs::update_state(
        &conn,
        &job.id,
        db::index_jobs::IndexJobStatus::Running,
        &row.cursor_json,
        None,
        1,
        Some(1),
        None,
    )
    .unwrap();
    let mut resumed = finished;
    let mut stats = TranslationRunStats::default();
    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut resumed,
            &mut stats,
        ))
        .expect("回到原位之后继续整本不该失败");
    let RunOutcome::Succeeded(resumed) = outcome else {
        panic!("回到原位之后应该一路翻到书末");
    };
    assert_eq!(resumed.next_ordinal, total);
    assert_eq!(
        stats.saved + stats.cached,
        total,
        "本轮把剩下的块全部补上（本章两块走缓存）"
    );
    let rows =
        db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
            .unwrap();
    assert_eq!(rows.len(), total, "整本最终都有译文，一块都没被跳过");
}
