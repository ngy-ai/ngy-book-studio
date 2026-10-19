use super::*;

#[test]
pub(crate) fn legacy_office_preview_pages_with_guessed_units_are_rejected() {
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
pub(crate) fn valid_office_preview_pages_skip_vision_and_clear_stale_chunks() {
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
