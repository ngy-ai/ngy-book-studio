//! Integration tests of the visual-understanding pass and of endpoint
//! reconciliation.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use crate::{
    ai::OpenAiCompatibleProvider,
    db,
    document::{DocumentLocator, SourceLocator, deterministic_id},
    storage::BlobStore,
};

#[test]
pub(crate) fn vision_endpoint_switch_uses_new_provider_and_preserves_embedding_route() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let embedding_provider = Arc::new(MockProvider::default());
    let old_vision_provider = Arc::new(MockProvider::default());
    old_vision_provider
        .vision_delay_ms
        .store(60_000, Ordering::SeqCst);
    let coordinator =
        fixture.coordinator_with_providers(embedding_provider.clone(), old_vision_provider.clone());
    let deadline = Instant::now() + TEST_PROGRESS_WAIT;
    while old_vision_provider.vision_png_calls.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "old vision request did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let new_vision_provider = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            embedding_provider.clone(),
            new_vision_provider.clone(),
            test_model_config(
                "embed-test",
                "embedding-endpoint-a:embed-test",
                "vision-test",
                "vision-endpoint-b:vision-test",
            ),
        ))
        .unwrap();
    let vision_job_id = format!("vision:{}", fixture.source_id);
    let completed = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &format!("embedding:{}", fixture.source_id),
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    fixture.wait_for_post_vision_embedding(
        &coordinator,
        "embedding-endpoint-a:embed-test",
        "vision-endpoint-b:vision-test",
    );
    let cursor: JobCursor = serde_json::from_str(&completed.cursor_json).unwrap();
    assert_eq!(
        cursor.execution_identity.as_deref(),
        Some("vision-endpoint-b:vision-test")
    );
    assert_eq!(
        new_vision_provider.vision_png_calls.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        new_vision_provider.embedding_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        old_vision_provider.embedding_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        embedding_provider.vision_png_calls.load(Ordering::SeqCst),
        0
    );
    assert!(embedding_provider.embedding_calls.load(Ordering::SeqCst) > 0);
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let visual_chunks = db::search_chunks::list_for_source(&conn, &fixture.source_id)
        .unwrap()
        .into_iter()
        .filter(|chunk| chunk.id.starts_with("vision-chunk-"))
        .collect::<Vec<_>>();
    assert!(!visual_chunks.is_empty());
    for chunk in visual_chunks {
        assert!(
            db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
                .unwrap()
                .is_some()
        );
    }
}

#[test]
pub(crate) fn startup_reconciles_saved_endpoint_identity_before_worker_runs() {
    let fixture = Fixture::new();
    let job_id = format!("embedding:{}", fixture.source_id);
    let conn = db::open_conn(&fixture.db_path).unwrap();
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
        next_ordinal: 1,
        retry_ordinals: Vec::new(),
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
    }
    .encode()
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Paused,
        &old_cursor,
        None,
        10,
        None,
        None,
    )
    .unwrap();
    db::embeddings::upsert_f32(
        &conn,
        "old-startup-vector",
        &chunk.id,
        "embed-test",
        &[2.0, 1.0, 0.5],
        10,
    )
    .unwrap();
    drop(conn);

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let blobs: Arc<dyn BlobStore> = fixture.blobs.clone();
    let coordinator = IndexingCoordinator::start(
        fixture.runtime.handle(),
        &fixture.db_path,
        blobs,
        provider.clone(),
        provider,
        test_model_config(
            "embed-test",
            "embedding-endpoint-b:embed-test",
            "vision-test",
            "vision-endpoint-a:vision-test",
        ),
    )
    .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let reconciled = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    let cursor: JobCursor = serde_json::from_str(&reconciled.cursor_json).unwrap();
    assert_eq!(reconciled.status, db::index_jobs::IndexJobStatus::Paused);
    assert_eq!(cursor.next_ordinal, 0);
    assert_eq!(
        cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-b:embed-test")
    );
    assert!(
        db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db::index_jobs::list_for_source_kind(&conn, &fixture.source_id, "embedding")
            .unwrap()
            .len(),
        1
    );
    drop(coordinator);
}

#[test]
pub(crate) fn post_vision_embedding_records_both_generations_and_rejects_stale_retry() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let vision_job_id = format!("vision:{}", fixture.source_id);
    let vision_cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "vision".to_string(),
        model: Some("vision-test".to_string()),
        execution_identity: Some("vision-endpoint-a:vision-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 1,
        retry_ordinals: Vec::new(),
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
    };
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state(
        &conn,
        &vision_job_id,
        db::index_jobs::IndexJobStatus::Running,
        &vision_cursor.encode().unwrap(),
        None,
        10,
        Some(10),
        None,
    )
    .unwrap();
    let vision_job = db::index_jobs::get(&conn, &vision_job_id).unwrap().unwrap();
    drop(conn);
    assert!(
        fixture
            .runtime
            .block_on(enqueue_post_vision_embedding(
                &coordinator.inner,
                &vision_job,
                &vision_cursor,
            ))
            .unwrap()
    );

    let expected_id = deterministic_id(
            "index-job",
            format!(
                "embedding-after-vision\0{}\0embed-test\0embedding-endpoint-a:embed-test\0vision-test\0vision-endpoint-a:vision-test",
                fixture.source_id
            )
            .as_bytes(),
        );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let post = db::index_jobs::get(&conn, &expected_id).unwrap().unwrap();
    let post_cursor: JobCursor = serde_json::from_str(&post.cursor_json).unwrap();
    assert_eq!(
        post_cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-a:embed-test")
    );
    assert_eq!(
        post_cursor.input_execution_identity.as_deref(),
        Some("vision-endpoint-a:vision-test")
    );
    db::index_jobs::update_state(
        &conn,
        &post.id,
        db::index_jobs::IndexJobStatus::Failed,
        &post.cursor_json,
        Some("test failure"),
        11,
        None,
        Some(11),
    )
    .unwrap();
    drop(conn);

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
                "vision-endpoint-b:vision-test",
            ),
        ))
        .unwrap();
    assert!(
        !fixture
            .runtime
            .block_on(coordinator.retry(&expected_id))
            .unwrap()
    );
    assert_eq!(
        db::index_jobs::get(&db::open_conn(&fixture.db_path).unwrap(), &expected_id)
            .unwrap()
            .unwrap()
            .status,
        db::index_jobs::IndexJobStatus::Failed
    );
}

#[test]
pub(crate) fn endpoint_identity_change_requeues_same_named_vision_model() {
    let fixture = Fixture::new();
    let canonical_id = format!("vision:{}", fixture.source_id);
    let mut conn = db::open_conn(&fixture.db_path).unwrap();
    let cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "vision".to_string(),
        model: Some("vision-test".to_string()),
        execution_identity: Some("vision-endpoint-a:vision-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 3,
        retry_ordinals: Vec::new(),
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
    }
    .encode()
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &cursor,
        None,
        10,
        None,
        Some(10),
    )
    .unwrap();
    let source = db::book_sources::get(&conn, &fixture.source_id)
        .unwrap()
        .unwrap();

    let outcome = db::transactions::reconcile_current_vision_job(
        &mut conn,
        &source,
        "vision-test",
        "vision-endpoint-b:vision-test",
        11,
    )
    .unwrap();
    assert_eq!(outcome, db::transactions::VisionJobReconciliation::Requeued);
    let job = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    assert_eq!(job.status, db::index_jobs::IndexJobStatus::Queued);
    let cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
    assert_eq!(cursor.model.as_deref(), Some("vision-test"));
    assert_eq!(
        cursor.execution_identity.as_deref(),
        Some("vision-endpoint-b:vision-test")
    );
    assert_eq!(cursor.next_ordinal, 0);
}

#[test]
pub(crate) fn reconfigure_rolls_back_all_jobs_before_publishing_models() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let canonical_id = format!("vision:{}", fixture.source_id);
    let first_ordinal = visual_chunk_ordinal(0, 0).unwrap();
    let visual_chunk = db::search_chunks::SearchChunk {
        id: visual_chunk_id(&fixture.source_id, 0, None),
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        content_unit_id: fixture.unit_id.clone(),
        ordinal: first_ordinal,
        heading: "视觉页面摘要".to_string(),
        body: "旧视觉结果".to_string(),
        token_count: 3,
        content_hash: blake3::hash(b"old vision").to_hex().to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(&fixture.book_id, &fixture.unit_id)
                .with_source(SourceLocator::created()),
        )
        .unwrap(),
        created_at: 10,
    };
    let old_cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "vision".to_string(),
        model: Some("vision-test".to_string()),
        execution_identity: Some("vision-endpoint-a:vision-test".to_string()),
        input_execution_identity: None,
        next_ordinal: 1,
        retry_ordinals: Vec::new(),
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
    }
    .encode()
    .unwrap();
    let mut conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &old_cursor,
        None,
        10,
        None,
        Some(10),
    )
    .unwrap();
    db::transactions::replace_visual_search_chunks_from_ordinal(
        &mut conn,
        &fixture.book_id,
        &fixture.source_id,
        first_ordinal,
        std::slice::from_ref(&visual_chunk),
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER test_reconfigure_embedding_failure
             BEFORE INSERT ON index_jobs
             WHEN NEW.kind = 'embedding'
              AND instr(NEW.cursor_json, '\"model\":\"embed-next\"') > 0
             BEGIN
                 SELECT RAISE(ABORT, 'injected embedding failure');
             END;",
    )
    .unwrap();

    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    assert!(
        fixture
            .runtime
            .block_on(coordinator.reconfigure(
                provider.clone(),
                provider,
                test_model_config(
                    "embed-next",
                    "embedding-endpoint-a:embed-next",
                    "vision-next",
                    "vision-endpoint-b:vision-next",
                ),
            ))
            .is_err()
    );
    let unchanged = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    assert_eq!(unchanged.status, db::index_jobs::IndexJobStatus::Succeeded);
    assert_eq!(unchanged.cursor_json, old_cursor);
    assert_eq!(
        db::search_chunks::get(&conn, &visual_chunk.id)
            .unwrap()
            .unwrap()
            .body,
        visual_chunk.body
    );
    let next_embedding_id = format!("embedding:{}", fixture.source_id);
    let unchanged_embedding = db::index_jobs::get(&conn, &next_embedding_id)
        .unwrap()
        .unwrap();
    let unchanged_embedding_cursor: JobCursor =
        serde_json::from_str(&unchanged_embedding.cursor_json).unwrap();
    assert_ne!(
        unchanged_embedding_cursor.model.as_deref(),
        Some("embed-next")
    );
    {
        let models = coordinator.inner.models.read().unwrap();
        assert_eq!(models.config.embedding_model, "embed-test");
        assert_eq!(
            models.config.embedding_execution_identity,
            "embedding-endpoint-a:embed-test"
        );
        assert_eq!(models.config.vision_model, "vision-test");
        assert_eq!(
            models.config.vision_execution_identity,
            "vision-endpoint-a:vision-test"
        );
    }

    conn.execute_batch("DROP TRIGGER test_reconfigure_embedding_failure")
        .unwrap();
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    fixture
        .runtime
        .block_on(coordinator.reconfigure(
            provider.clone(),
            provider,
            test_model_config(
                "embed-next",
                "embedding-endpoint-a:embed-next",
                "vision-next",
                "vision-endpoint-b:vision-next",
            ),
        ))
        .unwrap();
    let reconfigured = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    let reconfigured_cursor: JobCursor = serde_json::from_str(&reconfigured.cursor_json).unwrap();
    assert_eq!(reconfigured.status, db::index_jobs::IndexJobStatus::Queued);
    assert_eq!(reconfigured_cursor.model.as_deref(), Some("vision-next"));
    assert_eq!(
        reconfigured_cursor.execution_identity.as_deref(),
        Some("vision-endpoint-b:vision-next")
    );
    assert!(
        db::search_chunks::get(&conn, &visual_chunk.id)
            .unwrap()
            .is_none()
    );
    let embedding = db::index_jobs::get(&conn, &next_embedding_id)
        .unwrap()
        .unwrap();
    let embedding_cursor: JobCursor = serde_json::from_str(&embedding.cursor_json).unwrap();
    assert_eq!(embedding_cursor.model.as_deref(), Some("embed-next"));
    assert_eq!(
        embedding_cursor.execution_identity.as_deref(),
        Some("embedding-endpoint-a:embed-next")
    );
    let models = coordinator.inner.models.read().unwrap();
    assert_eq!(models.config.embedding_model, "embed-next");
    assert_eq!(
        models.config.embedding_execution_identity,
        "embedding-endpoint-a:embed-next"
    );
    assert_eq!(models.config.vision_model, "vision-next");
    assert_eq!(
        models.config.vision_execution_identity,
        "vision-endpoint-b:vision-next"
    );
}

#[test]
pub(crate) fn unknown_or_duplicate_vision_provenance_forces_clean_rebuild() {
    let fixture = Fixture::new();
    let canonical_id = format!("vision:{}", fixture.source_id);
    let first_ordinal = visual_chunk_ordinal(0, 0).unwrap();
    let visual_chunk = || db::search_chunks::SearchChunk {
        id: visual_chunk_id(&fixture.source_id, 0, None),
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        content_unit_id: fixture.unit_id.clone(),
        ordinal: first_ordinal,
        heading: "视觉页面摘要".to_string(),
        body: "来源未知的旧视觉结果".to_string(),
        token_count: 6,
        content_hash: blake3::hash(b"legacy vision").to_hex().to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(&fixture.book_id, &fixture.unit_id)
                .with_source(SourceLocator::created()),
        )
        .unwrap(),
        created_at: 10,
    };
    let mut conn = db::open_conn(&fixture.db_path).unwrap();
    db::transactions::replace_visual_search_chunks_from_ordinal(
        &mut conn,
        &fixture.book_id,
        &fixture.source_id,
        first_ordinal,
        &[visual_chunk()],
    )
    .unwrap();
    let unknown_cursor = JobCursor {
        schema_version: 1,
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        revision: fixture.revision,
        kind: "vision".to_string(),
        model: Some("vision-test".to_string()),
        execution_identity: None,
        input_execution_identity: None,
        next_ordinal: 1,
        retry_ordinals: Vec::new(),
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
    }
    .encode()
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &unknown_cursor,
        None,
        10,
        None,
        Some(10),
    )
    .unwrap();
    let source = db::book_sources::get(&conn, &fixture.source_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        db::transactions::reconcile_current_vision_job(
            &mut conn,
            &source,
            "vision-test",
            "vision-endpoint-a:vision-test",
            11,
        )
        .unwrap(),
        db::transactions::VisionJobReconciliation::Requeued
    );
    let rebuilt = db::index_jobs::get(&conn, &canonical_id).unwrap().unwrap();
    let rebuilt_cursor: JobCursor = serde_json::from_str(&rebuilt.cursor_json).unwrap();
    assert_eq!(rebuilt.status, db::index_jobs::IndexJobStatus::Queued);
    assert_eq!(rebuilt_cursor.next_ordinal, 0);
    assert_eq!(
        rebuilt_cursor.execution_identity.as_deref(),
        Some("vision-endpoint-a:vision-test")
    );
    assert!(
        db::search_chunks::list_for_source(&conn, &fixture.source_id)
            .unwrap()
            .iter()
            .all(|chunk| !chunk.id.starts_with("vision-chunk-"))
    );

    db::transactions::replace_visual_search_chunks_from_ordinal(
        &mut conn,
        &fixture.book_id,
        &fixture.source_id,
        first_ordinal,
        &[visual_chunk()],
    )
    .unwrap();
    db::index_jobs::update_state(
        &conn,
        &canonical_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &rebuilt.cursor_json,
        None,
        12,
        None,
        Some(12),
    )
    .unwrap();
    let mut duplicate = rebuilt;
    duplicate.id = "legacy-conflicting-vision".to_string();
    let mut duplicate_cursor: JobCursor = serde_json::from_str(&duplicate.cursor_json).unwrap();
    duplicate_cursor.execution_identity = Some("vision-endpoint-old:vision-test".to_string());
    duplicate.cursor_json = duplicate_cursor.encode().unwrap();
    db::index_jobs::insert(&conn, &duplicate).unwrap();
    assert_eq!(
        db::transactions::reconcile_current_vision_job(
            &mut conn,
            &source,
            "vision-test",
            "vision-endpoint-a:vision-test",
            13,
        )
        .unwrap(),
        db::transactions::VisionJobReconciliation::Requeued
    );
    assert_eq!(
        db::index_jobs::list_for_source_kind(&conn, &fixture.source_id, "vision")
            .unwrap()
            .len(),
        1
    );
    assert!(
        db::search_chunks::list_for_source(&conn, &fixture.source_id)
            .unwrap()
            .iter()
            .all(|chunk| !chunk.id.starts_with("vision-chunk-"))
    );
}
