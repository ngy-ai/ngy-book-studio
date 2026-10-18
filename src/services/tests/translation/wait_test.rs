use super::*;

/// 任务正在运行时，「立即翻译本章」要先等执行器把「已暂停」落盘再规划 —— 这一等必须落在
/// 应用 I/O runtime 上，因为阅读窗口是从 GPUI 的前台执行器调用这个入口的，那里不是 Tokio
/// 上下文（`docs/agent/code-map.md` 的约定，`block_on_without_tokio` 复现的正是这种环境）。
///
/// 现场 2026-10-14：等待里直接用 `tokio::time::sleep`，在 GPUI 回调里 panic
/// （`there is no reactor running`），而 GPUI 回调不可 unwind，整个进程随之退出。既有用例
/// 的任务都停在「已暂停」，从没走到这一段。
#[test]
fn waiting_for_a_running_translation_does_not_need_an_ambient_tokio_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();

    // 与另一条「立即翻译本章」用例同一套准备：先关掉翻译把书建出来，再打开。
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let book_id = runtime
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Running book", "Author")?.id))
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
    let unit_id = {
        let conn = db::open_conn(services.database_path()).unwrap();
        db::content_units::list_for_source(&conn, &source_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("the created book has no chapter")
            .id
    };

    // 手工把任务写成「运行中」：worker 只扫 queued，所以这条行不会被它接手（确定性），
    // 而这条用例要的正是「等运行中的任务落盘」那一段。游标补上执行器留下的块版本，
    // 规划才认得出这个推进点（缺省 0 = 来源未知，会退回书首）。
    let cursor_json = {
        let conn = db::open_conn(services.database_path()).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let mut cursor: serde_json::Value = serde_json::from_str(&job.cursor_json).unwrap();
        cursor["blocks_version"] = serde_json::json!(1);
        let cursor_json = serde_json::to_string(&cursor).unwrap();
        // 先落一次「已暂停」把可能残留的暂停请求清掉（`update_state` 只在写成 paused 时
        // 清这个标志），再写成「运行中」—— 运行中的任务身上不该挂着一个旧请求。
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
        let running = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        assert_eq!(running.status, db::index_jobs::IndexJobStatus::Running);
        assert!(
            !running.pause_requested && !running.cancel_requested,
            "运行中的任务身上不该挂着旧的暂停或取消请求"
        );
        cursor_json
    };

    // 执行器在两次请求之间读到暂停请求后落盘「已暂停」；这里按同一个条件触发，不按固定
    // 延时 —— 等待因此确定性地经过至少一个轮询，而不是碰巧绕开它。
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
                    // 落盘比「服务层发出请求」晚一点：真实执行器也是在两次请求之间
                    // 读到请求。等一小会，保证服务层那次「还在运行」的检查确定跑过。
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

    // 调用者没有 Tokio 上下文：等运行中的任务落盘这一段必须仍然可用。
    block_on_without_tokio(services.translate_reader_chapter_now(book_id.clone(), unit_id.clone()))
        .expect("等到运行中的翻译落盘之后必须能规划这一章");

    runtime
        .block_on(published)
        .expect("等待暂停落盘的任务异常退出")
        .expect("执行器必须观察到暂停请求并落盘");
}
