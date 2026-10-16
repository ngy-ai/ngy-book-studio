//! Integration tests of the configurable worker scheduling.

use super::*;

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use crate::db;

#[test]
pub(crate) fn configured_concurrency_runs_two_queued_jobs_at_once() {
    let fixture = Fixture::new();
    fixture.add_second_book();
    let provider = Arc::new(MockProvider::default());
    provider.block_embeddings.store(true, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    coordinator.configure_scheduling(2, Duration::ZERO);

    // The provider is blocked, so two in-flight calls can only exist when
    // two jobs run at the same time.
    fixture.wait_until(
        "two concurrent provider calls",
        Duration::from_secs(10),
        |_| provider.embedding_calls.load(Ordering::SeqCst) >= 2,
    );
    provider.block_embeddings.store(false, Ordering::SeqCst);
    fixture.wait_until("both embedding jobs", Duration::from_secs(10), |fixture| {
        fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Succeeded) == 2
    });
}

#[test]
pub(crate) fn configured_interval_pauses_between_two_queued_jobs() {
    let fixture = Fixture::new();
    fixture.add_second_book();
    let provider = Arc::new(MockProvider::default());
    provider.block_embeddings.store(true, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    fixture.wait_until(
        "one running embedding job",
        Duration::from_secs(10),
        |fixture| fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Running) == 1,
    );
    // Configured while the first job is claimed, so the single worker reads
    // the pause after that job finishes.
    coordinator.configure_scheduling(1, Duration::from_millis(1_500));
    provider.block_embeddings.store(false, Ordering::SeqCst);
    fixture.wait_until(
        "the first embedding job",
        Duration::from_secs(10),
        |fixture| fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Succeeded) == 1,
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Succeeded),
        1,
        "the configured pause must keep the worker idle between jobs"
    );
}

/// 「立即翻译本章」的插队优先级写在任务游标里（`index_jobs` 没有优先级列）：`list_queued`
/// 先看它、再看 `created_at`，所以后加入的翻译任务能排到更早入队的向量任务前面。
/// 优先级随游标持久化，重启之后依然有效。
#[test]
pub(crate) fn a_prioritized_translation_job_is_listed_before_older_work() {
    let fixture = Fixture::new();
    fixture.add_second_book();
    let provider: Arc<dyn crate::ai::OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            // 自动运行打开，新建的翻译任务才是 queued —— 队列里才有它可插队。
            true,
        ))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let queued = db::index_jobs::list_queued(&conn, 16).unwrap();
    let first = queued.first().expect("配置之后队列里必须有任务");
    assert_ne!(
        first.kind, "translation",
        "没有插队请求时，更早入队的任务排在前面：{}",
        first.id
    );

    // 只改游标里的 `priority`，正是「立即翻译本章」写下的那一项。
    let translation_id = queued
        .into_iter()
        .find(|job| job.kind == "translation")
        .expect("配置译文语言必须为每本书建一条翻译任务")
        .id;
    let cursor_json = db::index_jobs::get(&conn, &translation_id)
        .unwrap()
        .expect("刚列出来的任务必须还在")
        .cursor_json;
    let mut cursor: JobCursor = serde_json::from_str(&cursor_json).unwrap();
    cursor.priority = Some(unix_timestamp().unwrap());
    db::index_jobs::update_state(
        &conn,
        &translation_id,
        db::index_jobs::IndexJobStatus::Queued,
        &cursor.encode().unwrap(),
        None,
        unix_timestamp().unwrap(),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        db::index_jobs::list_queued(&conn, 16)
            .unwrap()
            .first()
            .expect("队列里必须有任务")
            .id,
        translation_id,
        "带上插队优先级之后这条翻译必须排在最前"
    );
}
