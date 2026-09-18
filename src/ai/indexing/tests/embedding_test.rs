//! Integration tests of the embedding pass, the vision pipeline and the
//! Office preview gates.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use crate::{
    db,
    document::{DocumentLocator, NormalizedRect, SourceLocator},
    storage::BlobStore,
};

use std::io::Cursor;

#[test]
fn embedding_and_vision_jobs_use_their_assigned_providers() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let embedding_provider = Arc::new(MockProvider::default());
    embedding_provider
        .embedding_marker
        .store(7, Ordering::SeqCst);
    let vision_provider = Arc::new(MockProvider::default());
    let coordinator =
        fixture.coordinator_with_providers(embedding_provider.clone(), vision_provider.clone());
    for kind in ["vision", "embedding"] {
        fixture
            .runtime
            .block_on(coordinator.wait_for_state(
                &format!("{kind}:{}", fixture.source_id),
                IndexingJobStatus::Succeeded,
                TEST_PROGRESS_WAIT,
            ))
            .unwrap();
    }
    fixture.wait_for_post_vision_embedding(
        &coordinator,
        "embedding-endpoint-a:embed-test",
        "vision-endpoint-a:vision-test",
    );
    assert!(embedding_provider.embedding_calls.load(Ordering::SeqCst) > 0);
    assert_eq!(
        embedding_provider.vision_png_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(vision_provider.embedding_calls.load(Ordering::SeqCst), 0);
    assert_eq!(vision_provider.vision_png_calls.load(Ordering::SeqCst), 1);

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let chunks = db::search_chunks::list_for_source(&conn, &fixture.source_id).unwrap();
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk.id.starts_with("vision-chunk-"))
    );
    for chunk in chunks {
        let (_, vector) = db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
            .unwrap()
            .unwrap();
        assert_eq!(vector[0], 7.0);
    }
}

#[test]
fn execution_diagnostics_keep_failed_and_retried_batches_in_their_library() {
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let provider = Arc::new(MockProvider::default());
    provider.fail_embeddings.store(true, Ordering::SeqCst);
    inner.models.write().unwrap().embedding_provider = provider.clone();
    let coordinator = IndexingCoordinator {
        inner,
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("embedding:{}", fixture.source_id);
    let read_job = || {
        db::index_jobs::get(&db::open_conn(&fixture.db_path).unwrap(), &job_id)
            .unwrap()
            .unwrap()
    };
    fixture
        .runtime
        .block_on(run_queued_job(&coordinator.inner, read_job()))
        .unwrap();
    assert_eq!(read_job().status, db::index_jobs::IndexJobStatus::Failed);
    provider.fail_embeddings.store(false, Ordering::SeqCst);
    assert!(
        fixture
            .runtime
            .block_on(coordinator.retry(&job_id))
            .unwrap()
    );
    fixture
        .runtime
        .block_on(run_queued_job(&coordinator.inner, read_job()))
        .unwrap();
    assert_eq!(read_job().status, db::index_jobs::IndexJobStatus::Succeeded);

    let logs = crate::ai::job_diagnostics::JobDiagnosticStore::for_database(&fixture.db_path)
        .unwrap()
        .read(&job_id)
        .unwrap();
    let starts: Vec<_> = logs
        .entries
        .iter()
        .filter(|entry| entry.message == "任务开始执行")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].metrics.attempt, Some(1));
    assert_eq!(starts[1].metrics.attempt, Some(2));
    assert_ne!(starts[0].metrics.run_id, starts[1].metrics.run_id);
    assert!(
        logs.entries
            .iter()
            .any(|entry| entry.metrics.error_kind.is_some())
    );
    assert!(
        logs.entries
            .iter()
            .any(|entry| entry.stage == "persist" && entry.metrics.actual_count.is_some())
    );
    assert_eq!(logs.entries.last().unwrap().message, "任务执行完成");
    let rendered = logs
        .entries
        .iter()
        .map(|entry| entry.format_line())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("mock embedding unavailable"));
    assert!(!rendered.contains("Index test"));

    let other = Fixture::new();
    assert!(
        crate::ai::job_diagnostics::JobDiagnosticStore::for_database(&other.db_path)
            .unwrap()
            .read(&job_id)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn queued_embedding_resumes_and_persists_vectors() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::default());
    let coordinator = fixture.coordinator(provider.clone());
    let job_id = format!("embedding:{}", fixture.source_id);
    let job = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &job_id,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert!(job.attempts >= 1);
    assert!(provider.embedding_calls.load(Ordering::SeqCst) >= 1);

    let chunks = db::search_chunks::list_for_source(
        &db::open_conn(&fixture.db_path).unwrap(),
        &fixture.source_id,
    )
    .unwrap();
    for chunk in chunks {
        assert!(
            db::embeddings::get_for_chunk_model(
                &db::open_conn(&fixture.db_path).unwrap(),
                &chunk.id,
                "embed-test",
            )
            .unwrap()
            .is_some()
        );
    }
}

#[test]
fn vision_waits_for_pages_then_indexes_and_embeds_description() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::default());
    let coordinator = fixture.coordinator(provider.clone());
    let vision_job = format!("vision:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();

    let stale_region = NormalizedRect::new(800, 800, 900, 900);
    let stale_id = visual_chunk_id(&fixture.source_id, 0, Some(stale_region));
    let stale_body = "旧的多余视觉区域";
    let stale_ordinal = visual_chunk_ordinal(0, MAX_VISION_REGIONS_PER_PAGE - 1).unwrap();
    let stale_chunk = db::search_chunks::SearchChunk {
        id: stale_id.clone(),
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        content_unit_id: fixture.unit_id.clone(),
        ordinal: stale_ordinal,
        heading: "视觉区域内容".to_string(),
        body: stale_body.to_string(),
        token_count: stale_body.chars().count().div_ceil(4),
        content_hash: blake3::hash(stale_body.as_bytes()).to_hex().to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(&fixture.book_id, &fixture.unit_id).with_region(stale_region),
        )
        .unwrap(),
        created_at: unix_timestamp().unwrap(),
    };
    db::transactions::replace_visual_search_chunks_from_ordinal(
        &mut db::open_conn(&fixture.db_path).unwrap(),
        &fixture.book_id,
        &fixture.source_id,
        stale_ordinal,
        &[stale_chunk],
    )
    .unwrap();

    let mut bytes = Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let bytes = bytes.into_inner();
    let key = fixture.runtime.block_on(fixture.blobs.put(&bytes)).unwrap();
    let blob = db::blobs::BlobRecord {
        object_key: key.to_string(),
        media_type: "image/png".to_string(),
        byte_len: bytes.len() as u64,
        hash: blake3::hash(&bytes).to_hex().to_string(),
        created_at: unix_timestamp().unwrap(),
    };
    let page = db::visual_pages::VisualPage {
        id: "page-1".to_string(),
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        content_unit_id: Some(fixture.unit_id.clone()),
        page_index: 0,
        object_key: key.to_string(),
        width: 2,
        height: 3,
        render_scale: 1.0,
        renderer: "test-renderer".to_string(),
        renderer_version: "1".to_string(),
        document_revision: fixture.revision,
        unit_revision: fixture.revision,
        profile_id: "test-profile".to_string(),
        fidelity: "structural".to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(&fixture.book_id, &fixture.unit_id)
                .with_source(crate::document::SourceLocator::created()),
        )
        .unwrap(),
        created_at: unix_timestamp().unwrap(),
    };
    let _ = db::transactions::replace_visual_pages(
        &mut db::open_conn(&fixture.db_path).unwrap(),
        &fixture.book_id,
        &fixture.source_id,
        fixture.revision,
        &[blob],
        &[page],
    )
    .unwrap();
    fixture
        .runtime
        .block_on(coordinator.visual_pages_ready(&fixture.source_id))
        .unwrap();
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();
    assert_eq!(provider.vision_png_calls.load(Ordering::SeqCst), 1);

    let chunks = db::search_chunks::list_for_source(
        &db::open_conn(&fixture.db_path).unwrap(),
        &fixture.source_id,
    )
    .unwrap();
    let visual = chunks
        .iter()
        .filter(|chunk| chunk.id.starts_with("vision-chunk-"))
        .collect::<Vec<_>>();
    assert_eq!(visual.len(), 3);
    assert!(visual.iter().any(|chunk| chunk.body.contains("页面文字")));
    assert!(!visual.iter().any(|chunk| chunk.id == stale_id));
    assert_eq!(
        visual.iter().map(|chunk| chunk.ordinal).collect::<Vec<_>>(),
        vec![
            visual_chunk_ordinal(0, 0).unwrap(),
            visual_chunk_ordinal(0, 1).unwrap(),
            visual_chunk_ordinal(0, 2).unwrap(),
        ]
    );
    let locators = visual
        .iter()
        .map(|chunk| serde_json::from_str::<DocumentLocator>(&chunk.locator_json).unwrap())
        .collect::<Vec<_>>();
    assert!(
        locators
            .iter()
            .all(|locator| { locator.source == Some(crate::document::SourceLocator::created()) })
    );
    assert_eq!(locators[0].region, None);
    assert_eq!(
        locators[1].region,
        Some(NormalizedRect::new(20, 30, 500, 200))
    );
    assert_eq!(
        locators[2].region,
        Some(NormalizedRect::new(20, 250, 900, 900))
    );
    assert_eq!(
        visual[1].id,
        visual_chunk_id(&fixture.source_id, 0, locators[1].region)
    );
    let deadline = Instant::now() + TEST_PROGRESS_WAIT;
    loop {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        if visual.iter().all(|chunk| {
            db::embeddings::get_for_chunk_model(&conn, &chunk.id, "embed-test")
                .unwrap()
                .is_some()
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "visual region chunks were not embedded"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn legacy_office_preview_pages_with_guessed_units_are_rejected() {
    let fixture = Fixture::new();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let first_unit = db::content_units::list_for_source(&conn, &fixture.source_id)
        .unwrap()
        .remove(0);
    let mut second_unit = first_unit.clone();
    second_unit.id = "office-preview-unit-2".to_string();
    second_unit.ordinal = 1;
    second_unit.title = Some("第二个内容单元".to_string());
    second_unit.href = Some("office-preview-unit-2.xhtml".to_string());
    second_unit.source_locator_json =
        serde_json::to_string(&Some(SourceLocator::office_section(2))).unwrap();
    db::content_units::insert(&conn, &second_unit).unwrap();

    let stale_body = "错误归属的旧视觉摘要";
    let stale_chunk = db::search_chunks::SearchChunk {
        id: visual_chunk_id(&fixture.source_id, 0, None),
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        content_unit_id: first_unit.id.clone(),
        ordinal: visual_chunk_ordinal(0, 0).unwrap(),
        heading: "视觉页面摘要".to_string(),
        body: stale_body.to_string(),
        token_count: stale_body.chars().count().div_ceil(4),
        content_hash: blake3::hash(stale_body.as_bytes()).to_hex().to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(&fixture.book_id, &first_unit.id)
                .with_source(SourceLocator::office_rendered_page(1)),
        )
        .unwrap(),
        created_at: unix_timestamp().unwrap(),
    };
    drop(conn);
    db::transactions::replace_visual_search_chunks_from_ordinal(
        &mut db::open_conn(&fixture.db_path).unwrap(),
        &fixture.book_id,
        &fixture.source_id,
        visual_chunk_ordinal(0, 0).unwrap(),
        &[stale_chunk],
    )
    .unwrap();

    let provider = Arc::new(MockProvider::default());
    let coordinator = fixture.coordinator(provider.clone());
    let vision_job = format!("vision:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();

    let mut image_bytes = Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut image_bytes, image::ImageFormat::Png)
        .unwrap();
    let image_bytes = image_bytes.into_inner();
    let key = fixture
        .runtime
        .block_on(fixture.blobs.put(&image_bytes))
        .unwrap();
    let blob = db::blobs::BlobRecord {
        object_key: key.to_string(),
        media_type: "image/png".to_string(),
        byte_len: image_bytes.len() as u64,
        hash: blake3::hash(&image_bytes).to_hex().to_string(),
        created_at: unix_timestamp().unwrap(),
    };
    let unit_ids = [first_unit.id, second_unit.id];
    let pages = (0..3)
        .map(|page_index| {
            let unit_id = &unit_ids[usize::from(page_index >= 2)];
            db::visual_pages::VisualPage {
                id: format!("office-preview-page-{page_index}"),
                book_id: fixture.book_id.clone(),
                source_id: fixture.source_id.clone(),
                content_unit_id: Some(unit_id.clone()),
                page_index,
                object_key: key.to_string(),
                width: 2,
                height: 3,
                render_scale: 1.0,
                renderer: "forged-non-office-renderer".to_string(),
                renderer_version: "test-office-renderer".to_string(),
                document_revision: fixture.revision,
                unit_revision: fixture.revision,
                profile_id: "test-profile".to_string(),
                fidelity: "office_enhanced".to_string(),
                locator_json: serde_json::to_string(
                    &DocumentLocator::unit(&fixture.book_id, unit_id).with_source(
                        SourceLocator::office_rendered_page(u32::try_from(page_index + 1).unwrap()),
                    ),
                )
                .unwrap(),
                created_at: unix_timestamp().unwrap(),
            }
        })
        .collect::<Vec<_>>();
    // Bypass the normal publication transaction to simulate a legacy or
    // externally corrupted mapping. Production publication rejects an
    // OfficeRenderedPage that claims a canonical content unit.
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::blobs::ensure_present(&conn, &blob).unwrap();
    db::visual_pages::delete_for_source(&conn, &fixture.source_id).unwrap();
    for page in &pages {
        db::visual_pages::upsert(&conn, page).unwrap();
    }
    fixture
        .runtime
        .block_on(coordinator.visual_pages_ready(&fixture.source_id))
        .unwrap();
    let failed = fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();

    assert_eq!(
        provider.vision_png_calls.load(Ordering::SeqCst),
        0,
        "preview-only Office pages must not be sent to a vision model"
    );
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("preview-only")),
        "a legacy guessed-unit mapping must fail closed"
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    assert_eq!(
        db::visual_pages::list_for_source(&conn, &fixture.source_id)
            .unwrap()
            .len(),
        3,
        "the pages remain available for enhanced preview"
    );
}

#[test]
fn valid_office_preview_pages_skip_vision_and_clear_stale_chunks() {
    let fixture = Fixture::new();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    conn.execute(
        "UPDATE books SET format = 'docx' WHERE id = ?1",
        [&fixture.book_id],
    )
    .unwrap();
    conn.execute(
        "UPDATE book_sources SET format = 'docx', source_kind = 'original' WHERE id = ?1",
        [&fixture.source_id],
    )
    .unwrap();
    let stale_body = "错误归属的旧视觉摘要";
    let stale_id = visual_chunk_id(&fixture.source_id, 0, None);
    let stale_chunk = db::search_chunks::SearchChunk {
        id: stale_id.clone(),
        book_id: fixture.book_id.clone(),
        source_id: fixture.source_id.clone(),
        content_unit_id: fixture.unit_id.clone(),
        ordinal: visual_chunk_ordinal(0, 0).unwrap(),
        heading: "视觉页面摘要".to_string(),
        body: stale_body.to_string(),
        token_count: stale_body.chars().count().div_ceil(4),
        content_hash: blake3::hash(stale_body.as_bytes()).to_hex().to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(&fixture.book_id, &fixture.unit_id)
                .with_source(SourceLocator::office_rendered_page(1)),
        )
        .unwrap(),
        created_at: unix_timestamp().unwrap(),
    };
    drop(conn);
    db::transactions::replace_visual_search_chunks_from_ordinal(
        &mut db::open_conn(&fixture.db_path).unwrap(),
        &fixture.book_id,
        &fixture.source_id,
        visual_chunk_ordinal(0, 0).unwrap(),
        &[stale_chunk],
    )
    .unwrap();

    let provider = Arc::new(MockProvider::default());
    let coordinator = fixture.coordinator(provider.clone());
    let vision_job = format!("vision:{}", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Failed,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();

    let mut image_bytes = Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut image_bytes, image::ImageFormat::Png)
        .unwrap();
    let image_bytes = image_bytes.into_inner();
    let key = fixture
        .runtime
        .block_on(fixture.blobs.put(&image_bytes))
        .unwrap();
    let blob = db::blobs::BlobRecord {
        object_key: key.to_string(),
        media_type: "image/png".to_string(),
        byte_len: image_bytes.len() as u64,
        hash: blake3::hash(&image_bytes).to_hex().to_string(),
        created_at: unix_timestamp().unwrap(),
    };
    let preview_unit_id =
        crate::preview::office_preview_unit_id(&fixture.book_id, &fixture.source_id);
    let pages = (0..3)
        .map(|page_index| db::visual_pages::VisualPage {
            id: format!("office-preview-page-{page_index}"),
            book_id: fixture.book_id.clone(),
            source_id: fixture.source_id.clone(),
            content_unit_id: None,
            page_index,
            object_key: key.to_string(),
            width: 2,
            height: 3,
            render_scale: 1.0,
            renderer: "ngy-office-com-enhanced".to_string(),
            renderer_version: "test-office-renderer".to_string(),
            document_revision: fixture.revision,
            unit_revision: 0,
            profile_id: "test-profile".to_string(),
            fidelity: "office_enhanced".to_string(),
            locator_json: serde_json::to_string(
                &DocumentLocator::unit(&fixture.book_id, &preview_unit_id).with_source(
                    SourceLocator::office_rendered_page(u32::try_from(page_index + 1).unwrap()),
                ),
            )
            .unwrap(),
            created_at: unix_timestamp().unwrap(),
        })
        .collect::<Vec<_>>();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::blobs::ensure_present(&conn, &blob).unwrap();
    db::visual_pages::delete_for_source(&conn, &fixture.source_id).unwrap();
    for page in &pages {
        db::visual_pages::upsert(&conn, page).unwrap();
    }
    drop(conn);

    fixture
        .runtime
        .block_on(coordinator.visual_pages_ready(&fixture.source_id))
        .unwrap();
    fixture
        .runtime
        .block_on(coordinator.wait_for_state(
            &vision_job,
            IndexingJobStatus::Succeeded,
            TEST_PROGRESS_WAIT,
        ))
        .unwrap();

    assert_eq!(
        provider.vision_png_calls.load(Ordering::SeqCst),
        0,
        "preview-only Office pages must not be sent to a vision model"
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let persisted_pages = db::visual_pages::list_for_source(&conn, &fixture.source_id).unwrap();
    assert_eq!(persisted_pages.len(), 3);
    assert!(
        persisted_pages
            .iter()
            .all(|page| page.content_unit_id.is_none()),
        "preview pages must retain their non-canonical identity"
    );
    assert!(db::search_chunks::get(&conn, &stale_id).unwrap().is_none());
    assert!(
        db::search_chunks::list_for_source(&conn, &fixture.source_id)
            .unwrap()
            .iter()
            .all(|chunk| !chunk.id.starts_with("vision-chunk-")),
        "a preview-only page cannot leave a chunk that could become a citation"
    );
    assert!(
        db::embeddings::get_for_chunk_model(&conn, &stale_id, "embed-test")
            .unwrap()
            .is_none(),
        "deleting the stale chunk must cascade to any derived embedding"
    );
}
