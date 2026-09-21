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

    // 其它任务逐项走、窗口恒空：这个集合只属于整本翻译。
    let snapshot = background_job_snapshot(&conn, job("embedding", running, cursor))
        .expect("向量任务的快照必须能建出来");
    assert!(snapshot.progress.processing.is_empty());
}
