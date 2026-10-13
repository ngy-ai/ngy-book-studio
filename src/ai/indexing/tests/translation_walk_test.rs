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
}

/// 运行期间保存了新的 Endpoint/密钥（模型与执行身份不变）时，剩下的块必须换用新 provider，
/// 而不是用领取时的快照把整本书翻完。
///
/// 现场 2026-09-24：改完 Endpoint 与 API key 后，正在跑的任务仍用旧值翻完剩下的块 ——
/// `configure_translation` 写的是 `models.translation`，而一次运行手里握的是领取时的快照。
#[test]
pub(crate) fn a_running_translation_switches_to_the_saved_endpoint_and_key() {
    let before = Arc::new(MockMergeProvider::default());
    // 第一发请求慢一点：好在它还在飞的时候「保存」新配置，观察后面几块走了谁。
    before.delay_ms.store(300, Ordering::SeqCst);
    let after = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&before, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    let mut stats = TranslationRunStats::default();

    // 「保存设置」走真实入口：`configure_providers` → `configure_translation`。模型与执行
    // 身份不变（只是换了密钥/地址之外的连接参数），任务行因此不会被重置。
    let swapped = {
        let inner = Arc::clone(&inner);
        let coordinator = IndexingCoordinator {
            inner: Arc::clone(&inner),
            workers: Mutex::new(Vec::new()),
        };
        let waiting_on = Arc::clone(&before);
        let saved = Arc::clone(&after);
        let model = translation.model.clone();
        let execution_identity = translation.execution_identity.clone();
        async move {
            while waiting_on.requests.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            coordinator
                .configure_translation(
                    saved,
                    model,
                    execution_identity,
                    TranslationGenerationSettings::default(),
                    Some("zh-Hans".to_string()),
                    false,
                )
                .await
                .unwrap();
        }
    };
    let outcome = fixture.runtime.block_on(async {
        let run = run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        );
        let (outcome, ()) = futures_util::join!(run, swapped);
        outcome.expect("换 provider 不该让运行失败")
    });

    let sent = before.requests.load(Ordering::SeqCst) + after.requests.load(Ordering::SeqCst);
    assert!(sent >= 2, "这条用例需要至少两个文本块，实际 {sent}");
    assert_eq!(
        before.requests.load(Ordering::SeqCst),
        1,
        "只有换配置时已经在飞的那一块走旧 provider"
    );
    assert_eq!(
        after.requests.load(Ordering::SeqCst),
        sent - 1,
        "剩下的块必须走保存后的新 provider"
    );
    assert_eq!(stats.saved, sent, "每一块都要翻出来");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("换 provider 之后运行照旧按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, sent);
    assert_eq!(stats.untranslated, 0);
}

/// 保存的配置属于另一套引擎（模型、执行身份或目标语言变了）时，本次运行不换 provider：
/// 那种重配置会把任务行整个重置，本次运行在下一个控制检查里就被判成已被替换，重新领取时
/// 才按新引擎继续 —— 半路换身份只会把两种来源的译文混进同一次运行。
#[test]
pub(crate) fn a_running_translation_ignores_a_saved_service_with_another_identity() {
    let before = Arc::new(MockMergeProvider::default());
    before.delay_ms.store(300, Ordering::SeqCst);
    let after = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&before, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    // 换了地址（执行身份随之改变）的那次保存。
    let other_engine = TranslationServices {
        provider: after.clone(),
        execution_identity: "translation-v2:another-endpoint".to_string(),
        ..translation.clone()
    };
    let mut stats = TranslationRunStats::default();

    let swapped = {
        let inner = Arc::clone(&inner);
        let waiting_on = Arc::clone(&before);
        async move {
            while waiting_on.requests.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let mut models = inner
                .models
                .write()
                .unwrap_or_else(|error| error.into_inner());
            models.translation = Some(other_engine);
        }
    };
    let outcome = fixture.runtime.block_on(async {
        let run = run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        );
        let (outcome, ()) = futures_util::join!(run, swapped);
        outcome.expect("另一套引擎的服务不该让运行失败")
    });

    let sent = before.requests.load(Ordering::SeqCst);
    assert!(sent >= 2, "这条用例需要至少两个文本块，实际 {sent}");
    assert_eq!(
        after.requests.load(Ordering::SeqCst),
        0,
        "本次运行不许换用另一套引擎"
    );
    assert_eq!(stats.saved, sent, "整本书照旧由领取时的 provider 翻完");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("换用另一套引擎的请求被忽略之后仍要按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, sent);
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

/// 「任务间隔」按请求计，不按任务计：每个并发槽位在自己的模型请求收尾后先休眠这段
/// 间隔，然后才去接下一块。
///
/// 现场（2026-09-20）：间隔原本只作用于两个任务之间，而一整本书就是一个任务 ——
/// 本机模型（Ollama）因此可以毫不停顿地连续请求，把 GPU 吃到满载、桌面黑屏，而这个
/// 设置对整本书的翻译形同虚设。
#[test]
pub(crate) fn the_configured_interval_pauses_after_each_model_request() {
    let interval = Duration::from_millis(200);
    let provider = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, interval);
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
        .expect("带间隔的串行翻译同样要能翻完");

    let starts = provider.request_starts();
    assert!(
        starts.len() >= 2,
        "间隔用例需要至少两次模型请求，实际 {}",
        starts.len()
    );
    let tolerance = Duration::from_millis(20);
    for pair in starts.windows(2) {
        let gap = pair[1].saturating_duration_since(pair[0]);
        assert!(
            gap + tolerance >= interval,
            "两次模型请求之间必须休眠任务间隔：实测 {gap:?} < {interval:?}"
        );
    }
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
