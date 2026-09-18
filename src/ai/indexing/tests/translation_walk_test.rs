//! Integration tests of the parallel walk: the in-flight block window and
//! reconfiguration.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use crate::db;

/// 块级并行的主用例：一次整本翻译同时开 `任务并发` 个块请求。
///
/// 现场（2026-09-15，「任务并发」改成 8、只开一本翻译）：窗口里的「处理中」恒为 1，
/// 因为并发只决定几个任务在跑，任务内部仍逐块串行。这条用例同时固定三件事：模型真的
/// 被并发调用、窗口写进了持久游标（任务窗口就靠它显示处理中）、游标永不越过还在等的块。
#[test]
pub(crate) fn a_translation_run_keeps_the_configured_number_of_blocks_in_flight() {
    let provider = Arc::new(MockMergeProvider::default());
    provider.delay_ms.store(20, Ordering::SeqCst);
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

/// 重配置路径写出的持久游标是「较小的形状」，而领取后的每次比较都是 JSON 字符串
/// 相等。领取时必须先把它重写成规范编码，否则任务第一次运行立刻判成 `Abandoned`：
/// 行留在 `running`、`attempts` 已加一、没有任何执行者 —— 现场 2026-09-17，新导入
/// 的图书点「恢复」后一直显示运行中，已处理 0、处理中 0，刷新也不动。
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
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let row = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(row.status, db::index_jobs::IndexJobStatus::Paused);
    let persisted: serde_json::Value = serde_json::from_str(&row.cursor_json).unwrap();
    assert!(
        persisted.get("inflight").is_none() && persisted.get("blocks_version").is_none(),
        "重配置路径写的持久游标没有块级字段：{}",
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
