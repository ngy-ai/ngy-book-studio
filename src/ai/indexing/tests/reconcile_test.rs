//! 「一步一个单位」的翻译任务对账与静默看门狗。

use super::*;

/// 配置一个只用于对账的翻译服务，并声明「库刚变更过、还有一轮对账要推进」——
/// 与真实路径一致：库写入之后 `wake()` 置位标识，worker 在空闲轮次把它推进完。
/// 不启动 worker：测试自己按步调用 `advance_translation_reconcile`。
fn staged_translation_inner(fixture: &Fixture) -> Arc<IndexingInner> {
    let inner = fixture.indexing_inner();
    inner.models.write().unwrap().translation = Some(TranslationServices {
        provider: Arc::new(MockProvider::default()),
        model: "chat-test".to_string(),
        execution_identity: "translation-endpoint-a:chat-test".to_string(),
        generation: TranslationGenerationSettings::default(),
        batch_blocks: NO_BATCHING,
        target_language: Some("en".to_string()),
        auto_run: true,
    });
    inner.reconcile_translations.store(true, Ordering::Release);
    inner
}

fn translation_job_count(fixture: &Fixture) -> usize {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::list_by_kind(&conn, "translation")
        .unwrap()
        .len()
}

fn queued_job_ids(fixture: &Fixture) -> Vec<String> {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::list_queued(&conn, 64)
        .unwrap()
        .into_iter()
        .map(|job| job.id)
        .collect()
}

/// 把 fixture 建书时排下的派生任务移出队列：这些用例只关心翻译对账这一步，
/// 队列里带着别的排队任务会让每一步都正确地「让路」而推不动。
fn drain_queue(fixture: &Fixture) {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    for job in db::index_jobs::list_queued(&conn, 64).unwrap() {
        db::index_jobs::update_state(
            &conn,
            &job.id,
            db::index_jobs::IndexJobStatus::Cancelled,
            &job.cursor_json,
            None,
            3,
            None,
            Some(3),
        )
        .unwrap();
    }
    assert!(
        db::index_jobs::list_queued(&conn, 1).unwrap().is_empty(),
        "清空队列之后才允许对账推进"
    );
}

/// 直接放一条排队任务进队列，模拟「模型工作已经在等」的那一步。
fn insert_queued_job(fixture: &Fixture, id: &str) {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::insert(
        &conn,
        &db::index_jobs::IndexJob {
            id: id.to_string(),
            book_id: fixture.book_id.clone(),
            source_id: Some(fixture.source_id.clone()),
            kind: "embedding".to_string(),
            status: db::index_jobs::IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: "{}".to_string(),
            error: None,
            created_at: 1,
            updated_at: 1,
            started_at: None,
            finished_at: None,
        },
    )
    .unwrap();
}

fn insert_running_job(fixture: &Fixture, id: &str) -> String {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::insert(
        &conn,
        &db::index_jobs::IndexJob {
            id: id.to_string(),
            book_id: fixture.book_id.clone(),
            source_id: Some(fixture.source_id.clone()),
            kind: "translation".to_string(),
            status: db::index_jobs::IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: "{}".to_string(),
            error: None,
            created_at: 1,
            updated_at: 1,
            started_at: None,
            finished_at: None,
        },
    )
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        id,
        db::index_jobs::IndexJobStatus::Running,
        "{}",
        None,
        2,
        Some(2),
        None,
    )
    .unwrap();
    id.to_string()
}

/// 三本需要翻译的书产生三条「插入」单位；每推进一步只出现一条任务，全部对齐之后
/// 不再产生单位。这是「对账一步一个单位」的直接断言（现场 2026-10-15：一次大事务
/// 做完整库对账）。
#[test]
pub(crate) fn translation_reconcile_creates_one_job_per_step() {
    let fixture = Fixture::new();
    let _second = fixture.add_second_book();
    let _third = fixture.add_second_book();
    drain_queue(&fixture);
    let inner = staged_translation_inner(&fixture);
    fixture.runtime.block_on(async {
        for expected in 1..=3 {
            assert_eq!(
                advance_translation_reconcile(&inner).await.unwrap(),
                TranslationReconcileStep::Advanced,
                "第 {expected} 步应当推进一个单位"
            );
            assert_eq!(
                translation_job_count(&fixture),
                expected,
                "每步只允许新建一条翻译任务"
            );
            // 这一步插入的任务已经在队列里排队；它对后面的步骤不算「外部工作」，
            // 否则剩下的单位要等整本书翻完才建得出来。
            assert_eq!(
                queued_job_ids(&fixture).len(),
                expected,
                "对账自己的产出应当留在队列里，但阻塞不住对账"
            );
        }
        assert_eq!(
            advance_translation_reconcile(&inner).await.unwrap(),
            TranslationReconcileStep::Idle,
            "三条任务全部对齐后不再有单位"
        );
    });
    assert_eq!(translation_job_count(&fixture), 3);
}

/// 队列里已经有排队任务时，对账一步都不推进（`Yielded`）：执行权先让给队首的
/// 模型工作，等它被跑掉之后同一步才开始建翻译任务。
#[test]
pub(crate) fn translation_reconcile_yields_to_queued_work() {
    let fixture = Fixture::new();
    let _second = fixture.add_second_book();
    drain_queue(&fixture);
    let inner = staged_translation_inner(&fixture);
    insert_queued_job(&fixture, "embedding:queued-elsewhere");

    fixture.runtime.block_on(async {
        assert_eq!(
            advance_translation_reconcile(&inner).await.unwrap(),
            TranslationReconcileStep::Yielded
        );
    });
    assert_eq!(
        translation_job_count(&fixture),
        0,
        "让路的那一步不允许插入任何翻译任务"
    );

    // 队首跑完之后，同一个入口才开始推进 —— 让路不会把对账请求吃掉，
    // 否则外部任务一多，这轮对账就永远不发生了。
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state(
        &conn,
        "embedding:queued-elsewhere",
        db::index_jobs::IndexJobStatus::Succeeded,
        "{}",
        None,
        2,
        None,
        Some(2),
    )
    .unwrap();
    fixture.runtime.block_on(async {
        assert_eq!(
            advance_translation_reconcile(&inner).await.unwrap(),
            TranslationReconcileStep::Advanced
        );
    });
    assert_eq!(translation_job_count(&fixture), 1);
}

#[test]
pub(crate) fn no_progress_is_reported_exactly_at_the_watchdog_threshold() {
    assert!(!no_progress_exceeded(
        NO_PROGRESS_WATCHDOG_THRESHOLD - Duration::from_millis(1)
    ));
    assert!(no_progress_exceeded(NO_PROGRESS_WATCHDOG_THRESHOLD));
}

/// 已领取的任务超过阈值没有任何事件时，看门狗往任务日志写一条「已领取但无进展」，
/// 带上无进展时长；没到阈值、任务不在运行态、或刚报过一段无进展时不写。
#[test]
pub(crate) fn watchdog_reports_a_claimed_job_without_progress() {
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let job_id = insert_running_job(&fixture, "translation:stalled");

    // 刚到过事件：不写。
    inner.note_progress(&job_id, Instant::now());
    fixture.runtime.block_on(watchdog_tick(&inner)).unwrap();
    assert!(recorded_events(&fixture, &job_id).is_empty());

    // 超过阈值：写一条，并把无进展时长一起记下。
    inner.note_progress(
        &job_id,
        Instant::now() - NO_PROGRESS_WATCHDOG_THRESHOLD - Duration::from_secs(1),
    );
    fixture.runtime.block_on(watchdog_tick(&inner)).unwrap();
    let events = recorded_events(&fixture, &job_id);
    let reported = events
        .iter()
        .find(|entry| entry.event == JobLogEvent::ClaimedNoProgress)
        .expect("超过阈值必须留下一条「已领取但无进展」");
    assert!(
        reported.metrics.duration_ms.unwrap() >= NO_PROGRESS_WATCHDOG_THRESHOLD.as_millis() as u64,
        "记录里必须带上无进展的时长"
    );

    // 报告本身刷新观察时间戳：同一段无进展不会每个周期都刷一条。
    fixture.runtime.block_on(watchdog_tick(&inner)).unwrap();
    assert_eq!(recorded_events(&fixture, &job_id).len(), events.len());

    // 任务不在运行态时只观察、不记录。
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        "{}",
        None,
        3,
        None,
        Some(3),
    )
    .unwrap();
    inner.note_progress(
        &job_id,
        Instant::now() - NO_PROGRESS_WATCHDOG_THRESHOLD - Duration::from_secs(1),
    );
    fixture.runtime.block_on(watchdog_tick(&inner)).unwrap();
    assert_eq!(
        recorded_events(&fixture, &job_id).len(),
        events.len(),
        "任务已经不在运行态，看门狗只观察不记录"
    );
}

/// 并发大于 1 时每个静默任务都要报：一个任务刚有过事件，另一个超过阈值，
/// 只有后者留下「已领取但无进展」。回归的是 2026-10-18 的观察槽只有一个、
/// 被任意任务的事件覆写导致漏报的问题。
#[test]
pub(crate) fn watchdog_reports_every_stalled_job_under_concurrency() {
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let fresh = insert_running_job(&fixture, "translation:fresh");
    let stalled = insert_running_job(&fixture, "translation:stalled-concurrent");

    inner.note_progress(&fresh, Instant::now());
    inner.note_progress(
        &stalled,
        Instant::now() - NO_PROGRESS_WATCHDOG_THRESHOLD - Duration::from_secs(1),
    );
    fixture.runtime.block_on(watchdog_tick(&inner)).unwrap();

    assert!(
        recorded_events(&fixture, &fresh).is_empty(),
        "刚有过事件的任务不得被报告"
    );
    assert!(
        recorded_events(&fixture, &stalled)
            .iter()
            .any(|entry| entry.event == JobLogEvent::ClaimedNoProgress),
        "彻底静默的那个任务必须留下「已领取但无进展」，不能被别的任务的事件掩盖"
    );
}
