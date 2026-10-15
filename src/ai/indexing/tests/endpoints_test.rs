//! Integration tests of model and endpoint identity reconciliation.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use crate::{ai::OpenAiCompatibleProvider, db};

#[test]
pub(crate) fn unchanged_vision_model_does_not_requeue_terminal_canonical_job() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::default());
    let coordinator = fixture.coordinator(provider.clone());
    let canonical_id = format!("vision:{}", fixture.source_id);
    let before = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &canonical_id,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();

    let provider_for_reconfigure: Arc<dyn OpenAiCompatibleProvider> = provider;
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider_for_reconfigure.clone(),
            provider_for_reconfigure,
            test_model_config(
                "embed-test",
                "embedding-endpoint-a:embed-test",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();
    let after = fixture
        .runtime
        .block_on(coordinator.job(&canonical_id))
        .unwrap()
        .unwrap();
    assert_eq!(after.status, IndexingJobStatus::Failed);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.cursor_json, before.cursor_json);
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
}

#[test]
pub(crate) fn unchanged_embedding_execution_does_not_create_or_reset_model_job() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let base_id = format!("embedding:{}", fixture.source_id);
    let cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "embedding".to_string(),
        model: Some("embed-test".to_string()),
        execution_identity: Some("embedding-endpoint-a:embed-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 9,
        retry_ordinals: Vec::new(),
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
        force_retry: false,
    }
    .encode()
    .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state(
        &conn,
        &base_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &cursor,
        None,
        10,
        Some(9),
        Some(10),
    )
    .unwrap();
    let before = db::index_jobs::get(&conn, &base_id).unwrap().unwrap();

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider.clone(),
            provider,
            test_model_config(
                "embed-test",
                "embedding-endpoint-a:embed-test",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();

    assert_eq!(db::index_jobs::get(&conn, &base_id).unwrap(), Some(before));
    let embedding_jobs =
        db::index_jobs::list_for_source_kind(&conn, &fixture.source_id, "embedding").unwrap();
    assert_eq!(embedding_jobs.len(), 1);
    assert_eq!(embedding_jobs[0].id, base_id);
}

#[test]
pub(crate) fn embedding_model_or_endpoint_change_is_enqueued_only_once() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let model_job_id = format!("embedding:{}", fixture.source_id);

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider.clone(),
            provider,
            test_model_config(
                "embed-next",
                "embedding-endpoint-a:embed-next",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let first = db::index_jobs::get(&conn, &model_job_id).unwrap().unwrap();
    assert_eq!(first.status, db::index_jobs::IndexJobStatus::Queued);
    let first_cursor: JobCursor = serde_json::from_str(&first.cursor_json).unwrap();
    assert_eq!(first_cursor.model.as_deref(), Some("embed-next"));
    assert_eq!(
        first_cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-a:embed-next")
    );
    db::index_jobs::update_state(
        &conn,
        &model_job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &first.cursor_json,
        None,
        20,
        Some(19),
        Some(20),
    )
    .unwrap();
    let completed_for_model = db::index_jobs::get(&conn, &model_job_id).unwrap().unwrap();

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider.clone(),
            provider,
            test_model_config(
                "embed-next",
                "embedding-endpoint-a:embed-next",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();
    assert_eq!(
        db::index_jobs::get(&conn, &model_job_id).unwrap(),
        Some(completed_for_model)
    );

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider.clone(),
            provider,
            test_model_config(
                "embed-next",
                "embedding-endpoint-b:embed-next",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();
    let first_for_endpoint = db::index_jobs::get(&conn, &model_job_id).unwrap().unwrap();
    assert_eq!(
        first_for_endpoint.status,
        db::index_jobs::IndexJobStatus::Queued
    );
    let endpoint_cursor: JobCursor = serde_json::from_str(&first_for_endpoint.cursor_json).unwrap();
    assert_eq!(
        endpoint_cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-b:embed-next")
    );
    db::index_jobs::update_state(
        &conn,
        &model_job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &first_for_endpoint.cursor_json,
        None,
        30,
        Some(29),
        Some(30),
    )
    .unwrap();
    let completed_for_endpoint = db::index_jobs::get(&conn, &model_job_id).unwrap().unwrap();

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider.clone(),
            provider,
            test_model_config(
                "embed-next",
                "embedding-endpoint-b:embed-next",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();
    assert_eq!(
        db::index_jobs::get(&conn, &model_job_id).unwrap(),
        Some(completed_for_endpoint)
    );
    assert_eq!(
        db::index_jobs::list_for_source_kind(&conn, &fixture.source_id, "embedding")
            .unwrap()
            .iter()
            .filter(|job| job.id == model_job_id)
            .count(),
        1
    );
    assert_eq!(
        coordinator
            .inner
            .models
            .read()
            .unwrap()
            .config
            .embedding_execution_identity,
        "embedding-endpoint-b:embed-next"
    );
}

#[test]
pub(crate) fn stale_embedding_generation_cannot_publish_after_endpoint_reconcile() {
    let fixture = Fixture::new();
    let job_id = format!("embedding:{}", fixture.source_id);
    let mut conn = db::open_conn(&fixture.db_path).unwrap();
    let chunk = db::search_chunks::list_for_source(&conn, &fixture.source_id)
        .unwrap()
        .remove(0);
    let old_cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "embedding".to_string(),
        model: Some("embed-test".to_string()),
        execution_identity: Some("embedding-endpoint-a:embed-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 0,
        retry_ordinals: Vec::new(),
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
        force_retry: false,
    };
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Running,
        &old_cursor.encode().unwrap(),
        None,
        10,
        Some(10),
        None,
    )
    .unwrap();
    db::embeddings::upsert_f32(
        &conn,
        "old-endpoint-vector",
        &chunk.id,
        "embed-test",
        &[2.0, 1.0, 0.5],
        10,
    )
    .unwrap();

    db::transactions::reconfigure_current_index_jobs(
        &mut conn,
        "embed-test",
        "embedding-endpoint-b:embed-test",
        true,
        "vision-test",
        "vision-endpoint-a:vision-test",
        11,
    )
    .unwrap();
    let replacement = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    let replacement_cursor: JobCursor = serde_json::from_str(&replacement.cursor_json).unwrap();
    assert_eq!(replacement.status, db::index_jobs::IndexJobStatus::Queued);
    assert_eq!(
        replacement_cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-b:embed-test")
    );
    assert!(
        db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
            .unwrap()
            .is_none()
    );

    let mut next_old_cursor = old_cursor.clone();
    next_old_cursor.next_ordinal = 1;
    let stale_row = db::embeddings::Embedding {
        id: "late-old-endpoint-vector".to_string(),
        search_chunk_id: chunk.id.clone(),
        model: "embed-test".to_string(),
        dimensions: 3,
        vector: db::embeddings::encode_f32(&[2.0, 1.0, 0.5]).unwrap(),
        created_at: 12,
    };
    assert!(
        !db::transactions::publish_embedding_batch_for_running_job(
            &mut conn,
            &job_id,
            &old_cursor.encode().unwrap(),
            &next_old_cursor.encode().unwrap(),
            &[stale_row],
            12,
        )
        .unwrap()
    );
    assert!(
        db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap(),
        replacement
    );
}

#[test]
pub(crate) fn endpoint_switch_while_provider_is_running_uses_new_generation() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let old_provider = Arc::new(MockProvider::default());
    old_provider.block_embeddings.store(true, Ordering::SeqCst);
    old_provider.embedding_marker.store(2, Ordering::SeqCst);
    let vision_provider = Arc::new(MockProvider::default());
    let coordinator =
        fixture.coordinator_with_providers(old_provider.clone(), vision_provider.clone());
    let job_id = format!("embedding:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Running,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    let deadline = Instant::now() + TEST_PROGRESS_WAIT;
    while old_provider.embedding_calls.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "old embedding request did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let new_provider = Arc::new(MockProvider::default());
    new_provider.embedding_marker.store(9, Ordering::SeqCst);
    let next_provider: Arc<dyn OpenAiCompatibleProvider> = new_provider.clone();
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            next_provider,
            vision_provider.clone(),
            test_model_config(
                "embed-test",
                "embedding-endpoint-b:embed-test",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        ))
        .unwrap();
    old_provider.block_embeddings.store(false, Ordering::SeqCst);
    let completed = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert!(new_provider.embedding_calls.load(Ordering::SeqCst) > 0);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &format!("vision:{}", fixture.source_id),
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert_eq!(new_provider.vision_png_calls.load(Ordering::SeqCst), 0);
    assert_eq!(old_provider.vision_png_calls.load(Ordering::SeqCst), 0);
    assert_eq!(vision_provider.embedding_calls.load(Ordering::SeqCst), 0);
    assert_eq!(vision_provider.vision_png_calls.load(Ordering::SeqCst), 1);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    let cursor: JobCursor = serde_json::from_str(&completed.cursor_json).unwrap();
    assert_eq!(
        cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-b:embed-test")
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let chunk = db::search_chunks::list_for_source(&conn, &fixture.source_id)
        .unwrap()
        .remove(0);
    let (_, vector) = db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
        .unwrap()
        .unwrap();
    assert_eq!(vector[0], 9.0);
    assert_eq!(
        db::index_jobs::list_for_source_kind(&conn, &fixture.source_id, "embedding")
            .unwrap()
            .iter()
            .filter(|job| job.id == job_id)
            .count(),
        1
    );
}
