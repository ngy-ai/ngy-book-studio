//! The projections the background-job window reads: progress counters, the
//! translation block window and which raw cursors are exposed at all.

use super::*;

/// 运行中的整本翻译必须把「处理中」的窗口投影给界面。
///
/// 现场 2026-09-20（任务并发 16）：图书翻译任务的「已处理」一直在涨，「处理中」却恒为 0，
/// 手工刷新也是 0。原因是运行中的翻译任务不下发原始游标（`cursor_json` 只留给失败任务和
/// 页面渲染），而块级明细的窗口当时只能去那份永远缺失的游标里找，于是永远读到 0。窗口是
/// 服务层该解析的读数，必须随快照一起交出去。
#[test]
fn a_running_translation_snapshot_projects_its_in_flight_window() {
    let temp = tempfile::tempdir().unwrap();
    let conn = db::open_or_recreate(&temp.path().join(db::DATABASE_FILE)).unwrap();
    let job = |kind: &str, status: db::index_jobs::IndexJobStatus, cursor_json: String| {
        db::index_jobs::IndexJob {
            id: format!("{kind}:source:zh-Hans"),
            book_id: "book".to_string(),
            source_id: Some("source".to_string()),
            kind: kind.to_string(),
            status,
            pause_requested: false,
            cancel_requested: false,
            attempts: 1,
            cursor_json,
            error: None,
            created_at: 1,
            updated_at: 2,
            started_at: Some(1),
            finished_at: None,
        }
    };
    let running = db::index_jobs::IndexJobStatus::Running;
    let cursor = serde_json::json!({
        "schema_version": 1,
        "next_ordinal": 17,
        "retry_ordinals": [],
        "inflight": [18, 17, 19, 17],
        "blocks_version": 3,
    })
    .to_string();

    let snapshot = background_job_snapshot(&conn, job("translation", running, cursor.clone()))
        .expect("翻译任务的快照必须能建出来");
    assert_eq!(snapshot.progress.completed, 17);
    assert_eq!(
        snapshot.progress.processing,
        vec![17, 18, 19],
        "窗口必须升序去重地投影出来"
    );
    // 窗口是投影出来的读数，不是把内部游标交给界面自己解析。
    assert!(snapshot.cursor_json.is_none());

    // 写坏的窗口按「没有在飞的块」处理，绝不猜一个出来。
    let broken = serde_json::json!({ "next_ordinal": 5, "inflight": [7, "9", -1, 5] }).to_string();
    let snapshot = background_job_snapshot(&conn, job("translation", running, broken))
        .expect("窗口写坏不是快照失败");
    assert_eq!(snapshot.progress.processing, vec![5, 7]);

    // 旧格式游标没有窗口字段，同样不许凭空显示处理中。
    let legacy = serde_json::json!({ "next_ordinal": 5 }).to_string();
    let snapshot = background_job_snapshot(&conn, job("translation", running, legacy))
        .expect("旧游标仍然要能读出进度");
    assert_eq!(snapshot.progress.completed, 5);
    assert!(snapshot.progress.processing.is_empty());
    assert!(snapshot.progress.scope.is_empty());
    assert_eq!(snapshot.progress.resume, None);

    // 收窄的章节跳转同时交出范围与借出点：游标停在那一章的第一块，界面要靠这两份
    // 读数才能把范围之外的块算成已处理（现场 2026-10-18：整本早已翻完，点「立即翻译
    // 本章」后表头显示待处理 7763）。
    let narrowed = serde_json::json!({
        "next_ordinal": 40,
        "retry_ordinals": [43, 40, 41, 43],
        "resume_ordinal": 78,
        "inflight": [],
    })
    .to_string();
    let snapshot = background_job_snapshot(&conn, job("translation", running, narrowed))
        .expect("收窄的翻译任务也要能读出快照");
    assert_eq!(snapshot.progress.completed, 40);
    assert_eq!(
        snapshot.progress.scope,
        vec![40, 41, 43],
        "范围必须升序去重"
    );
    assert_eq!(snapshot.progress.resume, Some(78));
    // 收窄一样不下发原始游标：这份读数就是界面需要的全部。
    assert!(snapshot.cursor_json.is_none());

    // 其它任务逐项走、窗口恒空：这个集合只属于整本翻译。
    let snapshot = background_job_snapshot(&conn, job("embedding", running, cursor))
        .expect("向量任务的快照必须能建出来");
    assert!(snapshot.progress.processing.is_empty());
    assert!(snapshot.progress.scope.is_empty());
    assert_eq!(snapshot.progress.resume, None);
}

/// 已取消与失败的任务可以删除（TODO 2026-10-18：取消后只能看着）；排队、运行、暂停与
/// 已成功的行不能删，删除也不经过 `control_background_job`。
#[test]
fn deleting_a_background_job_only_removes_terminal_unsuccessful_rows() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();
    let book_id = runtime
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Delete book", "Author")?.id))
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
    let insert = |id: &str, status: db::index_jobs::IndexJobStatus| {
        let conn = db::open_conn(services.database_path()).unwrap();
        db::index_jobs::insert(
            &conn,
            &db::index_jobs::IndexJob {
                id: id.to_string(),
                book_id: book_id.clone(),
                source_id: Some(source_id.clone()),
                kind: "embedding".to_string(),
                status,
                pause_requested: false,
                cancel_requested: false,
                attempts: 1,
                cursor_json: "{}".to_string(),
                error: None,
                created_at: 1,
                updated_at: 2,
                started_at: Some(1),
                finished_at: Some(2),
            },
        )
        .unwrap();
    };
    let cancelled = "embedding:delete-cancelled";
    let failed = "embedding:delete-failed";
    let running = "embedding:delete-running";
    let succeeded = "embedding:delete-succeeded";
    insert(cancelled, db::index_jobs::IndexJobStatus::Cancelled);
    insert(failed, db::index_jobs::IndexJobStatus::Failed);
    insert(running, db::index_jobs::IndexJobStatus::Running);
    insert(succeeded, db::index_jobs::IndexJobStatus::Succeeded);

    assert!(block_on_without_tokio(services.delete_background_job(cancelled.into())).unwrap());
    assert!(block_on_without_tokio(services.delete_background_job(failed.into())).unwrap());
    // 不该删的一个不动，谎报也不行。
    for kept in [running, succeeded] {
        assert!(
            !block_on_without_tokio(services.delete_background_job(kept.into())).unwrap(),
            "{kept} 不是可删除的终态"
        );
    }
    let conn = db::open_conn(services.database_path()).unwrap();
    assert!(db::index_jobs::get(&conn, cancelled).unwrap().is_none());
    assert!(db::index_jobs::get(&conn, failed).unwrap().is_none());
    assert!(db::index_jobs::get(&conn, running).unwrap().is_some());
    assert!(db::index_jobs::get(&conn, succeeded).unwrap().is_some());

    // 删除不是状态转换：路由到 control_background_job 必须报错，而不是静默无事。
    assert!(
        block_on_without_tokio(
            services.control_background_job(running.into(), BackgroundJobAction::Delete)
        )
        .is_err(),
        "Delete 路由到控制入口是 bug，必须大声失败"
    );
}
