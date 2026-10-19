use super::*;

/// 块级并行的主用例：一次整本翻译同时开 `任务并发` 个块请求。
///
/// 现场（2026-09-15，「任务并发」改成 8、只开一本翻译）：窗口里的「处理中」恒为 1，
/// 因为并发只决定几个任务在跑，任务内部仍逐块串行。这条用例同时固定三件事：模型真的
/// 被并发调用、窗口写进了持久游标（任务窗口就靠它显示处理中）、游标永不越过还在等的块。
#[test]
pub(crate) fn a_translation_run_keeps_the_configured_number_of_blocks_in_flight() {
    let provider = Arc::new(MockMergeProvider::default());
    // 延迟要盖得住「把下一个块规划出来」的时间：窗口里的第二个块要先走一遍
    // `plan_translation_block`（内部是 spawn_blocking 查库）才会发出请求，全量跑测试时
    // blocking 池被几百个用例抢，这一段能超过 20ms —— 第一个请求已经睡醒，第二次
    // `active.fetch_add` 就永远叠不上去，`peak` 停在 1。200ms 是留给这段规划的余量。
    provider.delay_ms.store(200, Ordering::SeqCst);
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let blocks = translation_blocks(&conn, &fixture.book_id, &fixture.source_id).unwrap();
    assert!(
        blocks.len() >= 2,
        "窗口用例需要至少两个文本块，实际 {}",
        blocks.len()
    );
    let window = blocks.len().min(2);
    provider.attach_probe(fixture.db_path.clone(), job.id.clone());
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(window, Duration::ZERO);
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
        .expect("并发运行不该失败");

    // 模型确实被同时调用：窗口开满，而且峰值不会超过并发数。
    assert_eq!(provider.peak.load(Ordering::SeqCst), window);
    assert_eq!(stats.saved, blocks.len());

    // 每个请求打开时读到的游标都是这条任务的真实窗口：非空、不超过并发数，并且整段
    // 窗口都在 `next_ordinal` 之后 —— 游标不许越过还在等的块，否则中断一次就会把
    // 没翻完的块当成已完成。
    let windows = provider.windows();
    assert!(
        windows.iter().any(|(_, open)| open.len() == window),
        "至少要有一个请求看见满窗口：{windows:?}"
    );
    for (next_ordinal, open) in &windows {
        assert!(!open.is_empty(), "请求在飞的时候窗口不许为空");
        assert!(open.len() <= window, "窗口不得超过并发数：{open:?}");
        assert!(
            open.iter().all(|ordinal| *ordinal >= *next_ordinal),
            "游标 {next_ordinal} 越过了还在等的块 {open:?}"
        );
    }

    // 终态必须把窗口清空，游标停在书末。
    let row = db::index_jobs::get(&conn, &job.id).unwrap().unwrap();
    let saved: serde_json::Value = serde_json::from_str(&row.cursor_json).unwrap();
    assert_eq!(saved["inflight"], serde_json::json!([]));
    assert_eq!(saved["next_ordinal"], serde_json::json!(blocks.len()));
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("整本翻完必须按 Succeeded 收尾");
    };
    assert!(finished.inflight.is_empty());
}

/// 并发数就是窗口上限：设成 1 时一次只有一个块在飞（同一个设置既限任务也限块）。
#[test]
pub(crate) fn a_concurrency_of_one_keeps_the_translation_serial() {
    let provider = Arc::new(MockMergeProvider::default());
    provider.delay_ms.store(5, Ordering::SeqCst);
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    provider.attach_probe(fixture.db_path.clone(), job.id.clone());
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    let mut stats = TranslationRunStats::default();

    fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("逐块串行同样要能翻完");

    assert_eq!(provider.peak.load(Ordering::SeqCst), 1);
    let windows = provider.windows();
    assert!(!windows.is_empty(), "串行也要有窗口读数");
    assert!(
        windows.iter().all(|(_, open)| open.len() == 1),
        "并发 1 时窗口只能有一个块：{windows:?}"
    );
}

/// 一个整窗都命中缓存的运行仍要把这些块按序提交，而不是立刻按 `Succeeded` 收尾。
///
/// 现场（2026-09-18，图书翻译任务）：点「恢复」后一会儿就显示完成，打开文本块明细
/// 却一个块都没处理。当时游标因模型/端点变化退回书首重扫（`blocks_version` 不符），
/// 开头这些块早已有译文，于是这一轮的窗口里没有任何请求在飞；walk 见到空窗口就结束
/// 运行，槽位一个都没提交 —— 任务按 Succeeded 收尾，游标却停在原处，而任务窗口的
/// 「已处理」正是从游标读出来的，于是恒为 0。重扫本身是对的（已存译文按缓存键命中，
/// 不花请求），错的是把它当成「这一轮没有活干」。
#[test]
pub(crate) fn a_window_resolved_from_the_cache_still_commits_in_ordinal_order() {
    let provider = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();

    // 第一次运行把整本书翻出来，于是重扫时每一个块都是缓存命中。
    let mut stats = TranslationRunStats::default();
    fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("首次运行不该失败");
    let total = cursor.next_ordinal;
    assert_eq!(stats.saved, total);
    let sent = provider.requests.load(Ordering::SeqCst);
    assert_eq!(sent, total, "每个文本块各发一次请求");

    // 把持久游标退回书首：运行手上的游标与库里那一行必须逐字一致，否则运行第一步
    // 就会把自己判成已被替换。
    let mut rescanned = cursor.clone();
    rescanned.next_ordinal = 0;
    rescanned.inflight.clear();
    let cursor_json = rescanned.encode().unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state_from(
        &conn,
        &job.id,
        db::index_jobs::IndexJobStatus::Running,
        db::index_jobs::IndexJobStatus::Running,
        &cursor_json,
        None,
        unix_timestamp().unwrap(),
        None,
        None,
    )
    .unwrap();
    drop(conn);
    let mut rescanned_job = job.clone();
    rescanned_job.cursor_json = cursor_json;

    let mut stats = TranslationRunStats::default();
    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &rescanned_job,
            &translation,
            "zh-Hans",
            &mut rescanned,
            &mut stats,
        ))
        .expect("重扫不该失败");

    assert_eq!(stats.cached, total, "重扫的每一块都必须走缓存");
    assert_eq!(stats.saved, 0);
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        sent,
        "缓存命中不得再打扰模型"
    );
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("重扫必须按 Succeeded 收尾");
    };
    assert_eq!(
        finished.next_ordinal, total,
        "整窗命中缓存也必须按序提交：游标要走到书末，而不是停在原处"
    );
    assert!(finished.inflight.is_empty());
}
