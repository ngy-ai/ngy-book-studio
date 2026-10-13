//! Integration tests of vision publication and control: late pause and
//! cancel, stale generations and rejected writes.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use crate::{ai::OpenAiCompatibleProvider, ai::job_diagnostics::JobLogErrorKind, db};

#[test]
pub(crate) fn duplicate_reconciliation_preserves_pending_cancel() {
    let fixture = Fixture::new();
    let canonical_id = format!("vision:{}", fixture.source_id);
    let mut conn = db::open_conn(&fixture.db_path).unwrap();
    let mut canonical = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    let cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "vision".to_string(),
        model: Some("vision-test".to_string()),
        execution_identity: Some("vision-endpoint-a:vision-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 0,
        retry_ordinals: Vec::new(),
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
    }
    .encode()
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Running,
        &cursor,
        None,
        10,
        Some(10),
        None,
    )
    .unwrap();
    assert_eq!(
        db::index_jobs::request_cancel(&conn, &canonical_id, 11).unwrap(),
        1
    );
    canonical.id = "legacy-vision-duplicate".to_string();
    canonical.cursor_json = cursor;
    db::index_jobs::insert(&conn, &canonical).unwrap();
    let source = db::book_sources::get(&conn, &fixture.source_id)
        .unwrap()
        .unwrap();
    db::transactions::reconcile_current_vision_job(
        &mut conn,
        &source,
        "vision-test",
        "vision-endpoint-a:vision-test",
        12,
    )
    .unwrap();
    let reconciled = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    assert_eq!(reconciled.status, db::index_jobs::IndexJobStatus::Cancelled);
    assert!(!reconciled.pause_requested && !reconciled.cancel_requested);
    assert_eq!(
        db::index_jobs::list_for_source_kind(&conn, &fixture.source_id, "vision")
            .unwrap()
            .len(),
        1
    );
}

#[test]
pub(crate) fn vision_model_change_supersedes_running_generation_without_late_publish() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let provider = Arc::new(MockProvider::default());
    provider.vision_delay_ms.store(5_000, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    let canonical_id = format!("vision:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &canonical_id,
            IndexingJobStatus::Running,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    let deadline = Instant::now() + TEST_PROGRESS_WAIT;
    while provider.vision_png_calls.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "old vision request did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let provider_for_reconfigure: Arc<dyn OpenAiCompatibleProvider> = provider.clone();
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider_for_reconfigure.clone(),
            provider_for_reconfigure,
            test_model_config(
                "embed-test",
                "embedding-endpoint-a:embed-test",
                "vision-next",
                "vision-endpoint-a:vision-next",
            ),
        ))
        .unwrap();
    provider.vision_delay_ms.store(0, Ordering::SeqCst);
    let completed = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &canonical_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    let cursor: JobCursor = serde_json::from_str(&completed.cursor_json).unwrap();
    assert_eq!(cursor.model.as_deref(), Some("vision-next"));
    assert_eq!(completed.attempts, 1);
    assert_eq!(
        db::index_jobs::list_for_source_kind(
            &db::open_conn(&fixture.db_path).unwrap(),
            &fixture.source_id,
            "vision",
        )
        .unwrap()
        .len(),
        1
    );
    let visual = db::search_chunks::list_for_source(
        &db::open_conn(&fixture.db_path).unwrap(),
        &fixture.source_id,
    )
    .unwrap()
    .into_iter()
    .filter(|chunk| chunk.id.starts_with("vision-chunk-"))
    .collect::<Vec<_>>();
    assert!(!visual.is_empty());
    assert!(
        visual
            .iter()
            .any(|chunk| chunk.body.contains("vision-next"))
    );
    assert!(
        visual
            .iter()
            .all(|chunk| !chunk.body.contains("vision-test"))
    );
}

#[test]
pub(crate) fn legacy_vision_ids_cannot_be_claimed_or_retried() {
    let fixture = Fixture::new();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let canonical_id = format!("vision:{}", fixture.source_id);
    let canonical = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Failed,
        &canonical.cursor_json,
        Some(db::index_jobs::VISION_WAITING_FOR_PAGES_ERROR),
        10,
        None,
        Some(10),
    )
    .unwrap();
    let mut duplicate = canonical;
    duplicate.id = "legacy-model-specific-vision".to_string();
    duplicate.status = db::index_jobs::IndexJobStatus::Failed;
    duplicate.finished_at = Some(10);
    db::index_jobs::insert(&conn, &duplicate).unwrap();

    assert_eq!(db::index_jobs::retry(&conn, &duplicate.id, 11).unwrap(), 0);
    assert_eq!(
        db::index_jobs::retry_failed_for_source_kind(&conn, &fixture.source_id, "vision", 12,)
            .unwrap(),
        1
    );
    assert_eq!(
        db::index_jobs::retry_failed_for_source_kind(&conn, &fixture.source_id, "vision", 13,)
            .unwrap(),
        0
    );
    assert!(!db::index_jobs::claim(&conn, &duplicate.id, &duplicate.cursor_json, 14).unwrap());
    assert_eq!(
        db::index_jobs::get(&conn, &duplicate.id)
            .unwrap()
            .unwrap()
            .status,
        db::index_jobs::IndexJobStatus::Failed
    );
    let queued = db::index_jobs::list_queued_ids_for_kind(&conn, "vision", 10).unwrap();
    // This diagnostic query intentionally exposes all rows; the worker's
    // executable queue applies the canonical guard.
    assert!(queued.contains(&canonical_id));
    assert!(
        !db::index_jobs::list_queued(&conn, 10)
            .unwrap()
            .iter()
            .any(|job| job.id == duplicate.id)
    );
}

#[test]
pub(crate) fn manual_vision_retry_requires_current_execution_identity() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job_id = format!("vision:{}", fixture.source_id);
    let mut cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "vision".to_string(),
        model: Some("vision-test".to_string()),
        execution_identity: Some("vision-endpoint-a:vision-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 0,
        retry_ordinals: Vec::new(),
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
    };
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Failed,
        &cursor.encode().unwrap(),
        Some("test failure"),
        10,
        None,
        Some(10),
    )
    .unwrap();
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry(&job_id))
            .unwrap()
    );

    cursor.execution_identity = Some("vision-endpoint-old:vision-test".to_string());
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Failed,
        &cursor.encode().unwrap(),
        Some("stale execution"),
        11,
        None,
        Some(11),
    )
    .unwrap();
    assert!(
        !fixture
            .runtime
            .block_on(coordinator.retry(&job_id))
            .unwrap()
    );
    assert_eq!(
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap().status,
        db::index_jobs::IndexJobStatus::Failed
    );
}

#[test]
pub(crate) fn cancelling_a_paused_job_publishes_terminal_state_immediately() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job_id = format!("vision:{}", fixture.source_id);
    let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Paused,
        &job.cursor_json,
        None,
        10,
        None,
        None,
    )
    .unwrap();
    assert!(
        fixture
            .runtime
            .block_on(coordinator.cancel(&job_id))
            .unwrap()
    );
    let cancelled = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(cancelled.status, db::index_jobs::IndexJobStatus::Cancelled);
    assert!(!cancelled.pause_requested && !cancelled.cancel_requested);
    assert!(cancelled.finished_at.is_some());
}

#[test]
pub(crate) fn rejected_vision_publication_settles_control_and_cursor_conflicts() {
    fn running_job(fixture: &Fixture) -> db::index_jobs::IndexJob {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let job_id = format!("vision:{}", fixture.source_id);
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Running,
            &job.cursor_json,
            None,
            10,
            Some(10),
            None,
        )
        .unwrap();
        job
    }

    let paused_fixture = Fixture::new();
    let paused_job = running_job(&paused_fixture);
    let paused_conn = db::open_conn(&paused_fixture.db_path).unwrap();
    assert_eq!(
        db::index_jobs::request_pause(&paused_conn, &paused_job.id, 11).unwrap(),
        1
    );
    assert!(matches!(
        paused_fixture
            .runtime
            .block_on(settle_interrupted_or_abandoned(
                &paused_fixture.indexing_inner(),
                &paused_job,
            ))
            .unwrap(),
        RunOutcome::Abandoned
    ));
    let paused = db::index_jobs::get(&paused_conn, &paused_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(paused.status, db::index_jobs::IndexJobStatus::Paused);
    assert!(!paused.pause_requested && !paused.cancel_requested);

    let cancelled_fixture = Fixture::new();
    let cancelled_job = running_job(&cancelled_fixture);
    let cancelled_conn = db::open_conn(&cancelled_fixture.db_path).unwrap();
    assert_eq!(
        db::index_jobs::request_cancel(&cancelled_conn, &cancelled_job.id, 11).unwrap(),
        1
    );
    cancelled_fixture
        .runtime
        .block_on(settle_interrupted_or_abandoned(
            &cancelled_fixture.indexing_inner(),
            &cancelled_job,
        ))
        .unwrap();
    let cancelled = db::index_jobs::get(&cancelled_conn, &cancelled_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, db::index_jobs::IndexJobStatus::Cancelled);
    assert!(!cancelled.pause_requested && !cancelled.cancel_requested);

    let conflict_fixture = Fixture::new();
    let conflict_job = running_job(&conflict_fixture);
    let conflict_conn = db::open_conn(&conflict_fixture.db_path).unwrap();
    let mut different_cursor: JobCursor = serde_json::from_str(&conflict_job.cursor_json).unwrap();
    different_cursor.next_ordinal += 1;
    db::index_jobs::update_state(
        &conflict_conn,
        &conflict_job.id,
        db::index_jobs::IndexJobStatus::Running,
        &different_cursor.encode().unwrap(),
        None,
        11,
        None,
        None,
    )
    .unwrap();
    conflict_fixture
        .runtime
        .block_on(settle_interrupted_or_abandoned(
            &conflict_fixture.indexing_inner(),
            &conflict_job,
        ))
        .unwrap();
    let conflict = db::index_jobs::get(&conflict_conn, &conflict_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(conflict.status, db::index_jobs::IndexJobStatus::Failed);
    assert_eq!(
        conflict.error.as_deref(),
        Some("索引任务执行游标失去同步，请重试")
    );
}

#[test]
pub(crate) fn final_vision_publication_atomically_honors_late_pause_and_cancel() {
    fn running_vision_job(fixture: &Fixture) -> (db::index_jobs::IndexJob, JobCursor) {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        let job_id = format!("vision:{}", fixture.source_id);
        let mut job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
        job.cursor_json = cursor.encode().unwrap();
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Running,
            &job.cursor_json,
            None,
            10,
            Some(10),
            None,
        )
        .unwrap();
        (job, cursor)
    }

    let paused_fixture = Fixture::new();
    let paused_inner = paused_fixture.indexing_inner();
    let (paused_job, paused_cursor) = running_vision_job(&paused_fixture);
    let paused_conn = db::open_conn(&paused_fixture.db_path).unwrap();
    assert_eq!(
        db::index_jobs::request_pause(&paused_conn, &paused_job.id, 11).unwrap(),
        1
    );
    paused_fixture
        .runtime
        .block_on(publish_outcome(
            &paused_inner,
            &paused_job,
            RunOutcome::Succeeded(paused_cursor),
            db::index_jobs::IndexJobStatus::Running,
        ))
        .unwrap();
    let paused = db::index_jobs::get(&paused_conn, &paused_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(paused.status, db::index_jobs::IndexJobStatus::Paused);
    assert!(!paused.pause_requested && !paused.cancel_requested);
    assert!(paused.finished_at.is_none());

    let cancelled_fixture = Fixture::new();
    let cancelled_inner = cancelled_fixture.indexing_inner();
    let (cancelled_job, cancelled_cursor) = running_vision_job(&cancelled_fixture);
    let cancelled_conn = db::open_conn(&cancelled_fixture.db_path).unwrap();
    assert_eq!(
        db::index_jobs::request_cancel(&cancelled_conn, &cancelled_job.id, 11).unwrap(),
        1
    );
    cancelled_fixture
        .runtime
        .block_on(publish_outcome(
            &cancelled_inner,
            &cancelled_job,
            RunOutcome::Failed(
                cancelled_cursor,
                "late provider failure".to_string(),
                Some(JobLogErrorKind::Provider),
            ),
            db::index_jobs::IndexJobStatus::Running,
        ))
        .unwrap();
    let cancelled = db::index_jobs::get(&cancelled_conn, &cancelled_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, db::index_jobs::IndexJobStatus::Cancelled);
    assert!(!cancelled.pause_requested && !cancelled.cancel_requested);
    assert!(cancelled.finished_at.is_some());
    assert!(cancelled.error.is_none());
}
