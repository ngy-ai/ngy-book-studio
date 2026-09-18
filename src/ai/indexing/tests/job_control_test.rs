//! Integration tests of the durable job loop: embedding retry, pause, cancel
//! and restart recovery.

use super::*;

use std::sync::{Arc, atomic::Ordering};

use crate::{db, document::deterministic_id};

#[test]
pub(crate) fn embedding_failure_keeps_fts_and_can_be_retried() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::default());
    provider.fail_embeddings.store(true, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    let job_id = format!("embedding:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert!(
        db::search_chunks::count_for_book(
            &db::open_conn(&fixture.db_path).unwrap(),
            &fixture.book_id
        )
        .unwrap()
            > 0
    );

    provider.fail_embeddings.store(false, Ordering::SeqCst);
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry(&job_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
}

#[test]
pub(crate) fn queued_controls_pause_resume_and_cancel_without_losing_cursor() {
    let fixture = Fixture::new();
    let embedding_job = format!("embedding:{}", fixture.source_id);
    let vision_job = format!("vision:{}", fixture.source_id);
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::request_pause(&conn, &embedding_job, unix_timestamp().unwrap()).unwrap();
    db::index_jobs::request_cancel(&conn, &vision_job, unix_timestamp().unwrap()).unwrap();
    drop(conn);
    let coordinator = fixture.coordinator(Arc::new(MockProvider::default()));
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &embedding_job,
            IndexingJobStatus::Paused,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Cancelled,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert!(
        fixture
            .runtime
            .block_on(coordinator.resume(&embedding_job))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &embedding_job,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
}

#[test]
pub(crate) fn queued_cancel_binds_both_canonical_jobs_and_survives_restart() {
    let fixture = Fixture::new();
    let embedding_id = format!("embedding:{}", fixture.source_id);
    let vision_id = format!("vision:{}", fixture.source_id);
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let embedding_scheduled = db::index_jobs::get(&conn, &embedding_id).unwrap().unwrap();
    let vision_scheduled = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    let unbound_vision_cursor = vision_scheduled.cursor_json.clone();
    db::index_jobs::request_cancel(&conn, &embedding_id, 10).unwrap();
    db::index_jobs::request_cancel(&conn, &vision_id, 10).unwrap();
    drop(conn);

    let inner = fixture.indexing_inner();
    fixture
        .runtime
        .block_on(run_queued_job(&inner, embedding_scheduled))
        .unwrap();
    fixture
        .runtime
        .block_on(run_queued_job(&inner, vision_scheduled))
        .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let cancelled_embedding = db::index_jobs::get(&conn, &embedding_id).unwrap().unwrap();
    let cancelled_vision = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    assert_eq!(
        cancelled_embedding.status,
        db::index_jobs::IndexJobStatus::Cancelled
    );
    assert_eq!(
        cancelled_vision.status,
        db::index_jobs::IndexJobStatus::Cancelled
    );
    let embedding_cursor: JobCursor =
        serde_json::from_str(&cancelled_embedding.cursor_json).unwrap();
    let vision_cursor: JobCursor = serde_json::from_str(&cancelled_vision.cursor_json).unwrap();
    assert_eq!(embedding_cursor.model.as_deref(), Some("embed-test"));
    assert_eq!(
        embedding_cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-a:embed-test")
    );
    assert_eq!(vision_cursor.model.as_deref(), Some("vision-test"));
    assert_eq!(
        vision_cursor.execution_identity.as_deref(),
        Some("vision-endpoint-a:vision-test")
    );

    // Simulate a terminal row written by an older build before first-claim
    // binding. Startup must bind it without reversing cancellation intent.
    db::index_jobs::update_state(
        &conn,
        &vision_id,
        db::index_jobs::IndexJobStatus::Cancelled,
        &unbound_vision_cursor,
        None,
        11,
        None,
        Some(11),
    )
    .unwrap();
    drop(conn);
    drop(inner);

    let coordinator = fixture.coordinator(Arc::new(MockProvider::default()));
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let restarted_embedding = db::index_jobs::get(&conn, &embedding_id).unwrap().unwrap();
    let restarted_vision = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    assert_eq!(
        restarted_embedding.status,
        db::index_jobs::IndexJobStatus::Cancelled
    );
    assert_eq!(
        restarted_vision.status,
        db::index_jobs::IndexJobStatus::Cancelled
    );
    let rebound_vision: JobCursor = serde_json::from_str(&restarted_vision.cursor_json).unwrap();
    assert_eq!(rebound_vision.model.as_deref(), Some("vision-test"));
    assert_eq!(
        rebound_vision.execution_identity.as_deref(),
        Some("vision-endpoint-a:vision-test")
    );
    drop(conn);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry(&embedding_id))
            .unwrap()
    );
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry(&vision_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &embedding_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_id,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
}

#[test]
pub(crate) fn queued_pause_binds_both_canonical_jobs_and_survives_restart() {
    let fixture = Fixture::new();
    let embedding_id = format!("embedding:{}", fixture.source_id);
    let vision_id = format!("vision:{}", fixture.source_id);
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let embedding_scheduled = db::index_jobs::get(&conn, &embedding_id).unwrap().unwrap();
    let unbound_embedding_cursor = embedding_scheduled.cursor_json.clone();
    let vision_scheduled = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    db::index_jobs::request_pause(&conn, &embedding_id, 10).unwrap();
    db::index_jobs::request_pause(&conn, &vision_id, 10).unwrap();
    drop(conn);

    let inner = fixture.indexing_inner();
    fixture
        .runtime
        .block_on(run_queued_job(&inner, embedding_scheduled))
        .unwrap();
    fixture
        .runtime
        .block_on(run_queued_job(&inner, vision_scheduled))
        .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let paused_embedding = db::index_jobs::get(&conn, &embedding_id).unwrap().unwrap();
    let paused_vision = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    assert_eq!(
        paused_embedding.status,
        db::index_jobs::IndexJobStatus::Paused
    );
    assert_eq!(paused_vision.status, db::index_jobs::IndexJobStatus::Paused);
    for (job, model, identity) in [
        (
            &paused_embedding,
            "embed-test",
            "embedding-endpoint-a:embed-test",
        ),
        (
            &paused_vision,
            "vision-test",
            "vision-endpoint-a:vision-test",
        ),
    ] {
        let cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
        assert_eq!(cursor.model.as_deref(), Some(model));
        assert_eq!(cursor.execution_identity.as_deref(), Some(identity));
    }

    // Exercise recovery of an older unbound paused canonical row as well.
    db::index_jobs::update_state(
        &conn,
        &embedding_id,
        db::index_jobs::IndexJobStatus::Paused,
        &unbound_embedding_cursor,
        None,
        11,
        None,
        None,
    )
    .unwrap();
    drop(conn);
    drop(inner);

    let coordinator = fixture.coordinator(Arc::new(MockProvider::default()));
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let restarted_embedding = db::index_jobs::get(&conn, &embedding_id).unwrap().unwrap();
    let restarted_vision = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    assert_eq!(
        restarted_embedding.status,
        db::index_jobs::IndexJobStatus::Paused
    );
    assert_eq!(
        restarted_vision.status,
        db::index_jobs::IndexJobStatus::Paused
    );
    let rebound_embedding: JobCursor =
        serde_json::from_str(&restarted_embedding.cursor_json).unwrap();
    assert_eq!(rebound_embedding.model.as_deref(), Some("embed-test"));
    assert_eq!(
        rebound_embedding.execution_identity.as_deref(),
        Some("embedding-endpoint-a:embed-test")
    );
    drop(conn);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.resume(&embedding_id))
            .unwrap()
    );
    assert!(
        fixture
            .runtime
            .block_on(coordinator.resume(&vision_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &embedding_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_id,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
}

#[test]
pub(crate) fn running_provider_call_observes_pause_and_resumes() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::default());
    provider.embedding_delay_ms.store(5_000, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    let job_id = format!("embedding:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Running,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert!(
        fixture
            .runtime
            .block_on(coordinator.pause(&job_id))
            .unwrap()
    );
    let paused = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Paused,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    let cursor = paused.cursor_json.clone();

    provider.embedding_delay_ms.store(0, Ordering::SeqCst);
    assert!(
        fixture
            .runtime
            .block_on(coordinator.resume(&job_id))
            .unwrap()
    );
    let completed = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert_ne!(completed.cursor_json, cursor);
}

#[test]
pub(crate) fn startup_recovers_an_interrupted_job_from_its_cursor() {
    let fixture = Fixture::new();
    let job_id = format!("embedding:{}", fixture.source_id);
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Running,
        &job.cursor_json,
        None,
        unix_timestamp().unwrap(),
        Some(unix_timestamp().unwrap()),
        None,
    )
    .unwrap();
    drop(conn);

    let coordinator = fixture.coordinator(Arc::new(MockProvider::default()));
    let completed = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert!(completed.attempts >= 2);
}

#[test]
pub(crate) fn startup_collapses_legacy_model_specific_vision_job_into_canonical_row() {
    let fixture = Fixture::new();
    let canonical_id = format!("vision:{}", fixture.source_id);
    let conn = db::open_conn(&fixture.db_path).unwrap();
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
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
    }
    .encode()
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Failed,
        &cursor,
        Some("视觉页面尚未生成"),
        unix_timestamp().unwrap(),
        None,
        Some(unix_timestamp().unwrap()),
    )
    .unwrap();
    canonical.id = deterministic_id(
        "index-job",
        format!("model\0vision\0{}\0vision-test", fixture.source_id).as_bytes(),
    );
    canonical.status = db::index_jobs::IndexJobStatus::Queued;
    canonical.cursor_json = cursor;
    canonical.error = None;
    canonical.finished_at = None;
    db::index_jobs::insert(&conn, &canonical).unwrap();
    drop(conn);

    let coordinator = fixture.coordinator(Arc::new(MockProvider::default()));
    let canonical = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &canonical_id,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert_eq!(canonical.status, IndexingJobStatus::Failed);
    let vision_jobs = db::index_jobs::list_for_source_kind(
        &db::open_conn(&fixture.db_path).unwrap(),
        &fixture.source_id,
        "vision",
    )
    .unwrap();
    assert_eq!(vision_jobs.len(), 1);
    assert_eq!(vision_jobs[0].id, canonical_id);
}
