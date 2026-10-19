use super::*;

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
        resume_ordinal: None,
        inflight: Vec::new(),
        blocks_version: TRANSLATION_BLOCKS_VERSION,
        priority: None,
        force_retry: false,
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
