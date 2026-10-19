use super::*;

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
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
        force_retry: false,
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
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
        force_retry: false,
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
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
        force_retry: false,
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
