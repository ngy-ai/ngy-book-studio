use super::*;

/// 阅读端的「重新翻译」：服务层按当前显示语言找到那本书的翻译任务，再由索引层把
/// **块标识**换成任务里的序号重跑那一块。块标识在正文里找不到时明确报错，绝不换一块
/// 重跑（那会写下另一块的译文）。
#[test]
fn retranslating_one_reader_block_requeues_that_block_of_the_display_language_task() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();

    // 这条用例看的是规划后的游标：阅读端的重译现在会插到队列最前，执行器会立刻
    // 领取它，所以先把这一组 worker 停掉，免得断言跑到执行完的任务上。
    {
        let mut workers = services.indexing.workers.lock().unwrap();
        for worker in workers.drain(..) {
            worker.abort();
        }
    }

    // 先关掉翻译，把图书建出来，再打开：这条路径与
    // `configuring_a_default_language_enqueues_translation_for_existing_books` 相同，
    // 因此这里测的是「任务真的存在」之后的重跑，而不是任务怎么来的。
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let book_id = runtime
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Retranslate book", "Author")?.id))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();
    let source_id = {
        let conn = db::open_conn(services.database_path()).unwrap();
        db::book_sources::list_for_book(&conn, &book_id)
            .unwrap()
            .into_iter()
            .next()
            .context("the created book has no source")
            .unwrap()
            .id
    };
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("zh-Hans".to_string());
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let job_id = format!("translation:{source_id}:zh-Hans");
    assert!(
        db::index_jobs::get(&db::open_conn(&services.db_path).unwrap(), &job_id)
            .unwrap()
            .is_some(),
        "配置译文语言之后这本书必须有翻译任务"
    );

    // 「标题 + 一段」的模板里，段落是第二块（序号 1）。块标识与阅读端拿到的完全一致
    // （同一个读取路径），所以这里直接向它要，而不是照文档规则自己拼一个。
    let detail = block_on_without_tokio(services.background_job_translation_block_detail(
        job_id.clone(),
        1,
        vec![book_id.clone()],
    ))
    .unwrap();
    assert_eq!(detail.ordinal, 1);
    block_on_without_tokio(services.retranslate_reader_translation_block(
        book_id.clone(),
        detail.unit_id.clone(),
        detail.block_id.clone(),
    ))
    .unwrap();
    let job = db::index_jobs::get(&db::open_conn(&services.db_path).unwrap(), &job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, db::index_jobs::IndexJobStatus::Queued);
    let cursor: serde_json::Value = serde_json::from_str(&job.cursor_json).unwrap();
    assert_eq!(cursor["retry_ordinals"], serde_json::json!([1]));
    assert_eq!(cursor["next_ordinal"], serde_json::json!(1));
    assert!(
        cursor["priority"].as_u64().is_some(),
        "阅读端的「重新翻译」要像「立即翻译本章」一样插到待执行队列最前"
    );

    // 不属于这本书的块标识：报错，而不是挑一块重跑。
    let error = block_on_without_tokio(services.retranslate_reader_translation_block(
        book_id,
        detail.unit_id,
        "not-a-block".to_string(),
    ))
    .unwrap_err();
    assert!(format!("{error:#}").contains("找不到文本块"), "{error:#}");
}

/// 任务正在运行时，阅读端的「重新翻译这一块」也要先等执行器把「已暂停」落盘再规划 ——
/// 与「立即翻译本章」同一条 `pause_running_translation` 路径。
///
/// 现场 2026-10-15：整本翻译正在跑时点「重新翻译这一块」，规划对一条运行中的任务静默
/// 落空（`reset_for_translation_block_retry` 拒绝 `running` 的游标），而调用方没有先暂停，
/// 用户看到的因此是「无法重新翻译该文本块：这本书没有可重试的翻译任务」。这条用例钉住
/// 「先暂停、后规划」：调用者仍然没有 Tokio 上下文，等待照旧落在应用 I/O runtime 上。
#[test]
fn retranslating_one_reader_block_waits_for_a_running_task_to_pause() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();
    // 这条用例用一根线程模拟「正在运行的执行器」，不需要真实 worker 参与。
    // 重置会把任务放回队列，空闲 worker 会（正确地）立刻领取它，于是「断言队列状态」
    // 变成竞态 —— 2026-10-18 复现：6 次里 3 次读到 `running`（worker 已领取并开始翻）。
    // 中止本测试实例的 worker 池，断言因此只钉住「先暂停、后规划」这一步的落盘结果。
    services
        .indexing
        .inner
        .shutdown
        .store(true, std::sync::atomic::Ordering::Release);
    for worker in services.indexing.workers.lock().unwrap().drain(..) {
        worker.abort();
    }

    // 与另外两条用例同一套准备：先关掉翻译把书建出来，再打开。
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let book_id = runtime
        .block_on(async {
            services
                .spawn_library(|library| {
                    Ok(library.create_book("Running block book", "Author")?.id)
                })
                .await
                .context("library test worker stopped")?
        })
        .unwrap();
    let source_id = {
        let conn = db::open_conn(services.database_path()).unwrap();
        db::book_sources::list_for_book(&conn, &book_id)
            .unwrap()
            .into_iter()
            .next()
            .context("the created book has no source")
            .unwrap()
            .id
    };
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("zh-Hans".to_string());
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let job_id = format!("translation:{source_id}:zh-Hans");

    // 手工把任务写成「运行中」：worker 只扫 queued，所以这条行不会被它接手（确定性），
    // 而要测的正是「等运行中的任务落盘」那一段。
    let cursor_json = {
        let conn = db::open_conn(services.database_path()).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let mut cursor: serde_json::Value = serde_json::from_str(&job.cursor_json).unwrap();
        cursor["blocks_version"] = serde_json::json!(1);
        let cursor_json = serde_json::to_string(&cursor).unwrap();
        // 先落一次「已暂停」把可能残留的暂停请求清掉（`update_state` 只在写成 paused 时
        // 清这个标志），再写成「运行中」。
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Paused,
            &cursor_json,
            None,
            1,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            db::index_jobs::update_state(
                &conn,
                &job_id,
                db::index_jobs::IndexJobStatus::Running,
                &cursor_json,
                None,
                1,
                Some(1),
                None,
            )
            .unwrap(),
            1,
            "任务必须真的处于运行中"
        );
        cursor_json
    };

    // 执行器在两次请求之间读到暂停请求后落盘「已暂停」；这里按同一个条件触发，不按固定
    // 延时 —— 等待因此确定性地经过至少一个轮询。
    let db_path = services.database_path().to_path_buf();
    let published = services.runtime().handle().spawn_blocking({
        let job_id = job_id.clone();
        let cursor_json = cursor_json.clone();
        move || -> anyhow::Result<()> {
            loop {
                let requested = {
                    let conn = db::open_conn(&db_path)?;
                    db::index_jobs::get(&conn, &job_id)?.is_some_and(|job| job.pause_requested)
                };
                if requested {
                    // 落盘比「服务层发出请求」晚一点，保证服务层那次「还在运行」的检查
                    // 确定跑过。
                    std::thread::sleep(Duration::from_millis(30));
                    let conn = db::open_conn(&db_path)?;
                    db::index_jobs::update_state(
                        &conn,
                        &job_id,
                        db::index_jobs::IndexJobStatus::Paused,
                        &cursor_json,
                        None,
                        2,
                        Some(1),
                        None,
                    )?;
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    });

    // 阅读端只说得出块标识，这里用与它同一个读取路径拿到「标题 + 一段」里的段落（序号 1）。
    let detail = block_on_without_tokio(services.background_job_translation_block_detail(
        job_id.clone(),
        1,
        vec![book_id.clone()],
    ))
    .unwrap();
    assert_eq!(detail.ordinal, 1);
    block_on_without_tokio(services.retranslate_reader_translation_block(
        book_id.clone(),
        detail.unit_id.clone(),
        detail.block_id.clone(),
    ))
    .expect("等到运行中的翻译落盘之后必须能重跑这一块");

    runtime
        .block_on(published)
        .expect("等待暂停落盘的任务异常退出")
        .expect("执行器必须观察到暂停请求并落盘");

    let job = db::index_jobs::get(&db::open_conn(services.database_path()).unwrap(), &job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, db::index_jobs::IndexJobStatus::Queued);
    let cursor: serde_json::Value = serde_json::from_str(&job.cursor_json).unwrap();
    assert_eq!(cursor["retry_ordinals"], serde_json::json!([1]));
    assert_eq!(cursor["next_ordinal"], serde_json::json!(1));
    assert!(
        cursor["priority"].as_u64().is_some(),
        "等运行中的任务落盘之后，这一块也要插到待执行队列最前"
    );
}
