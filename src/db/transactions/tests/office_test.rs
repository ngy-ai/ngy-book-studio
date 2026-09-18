//! Tests for office opt-in atomicity and rollback.

pub(super) use super::super::*;
pub(super) use super::*;

#[test]
fn renderer_change_requeues_visual_vision_and_visual_embeddings_only() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let visual_job_id = "visual-render:source-1";
    let current_spec = running_visual_job(&conn);
    let staged_blob = staged_visual_blob("reconcile", 37);
    let staged_page = staged_visual_page(
        &fixture,
        &current_spec,
        "staged-page-reconcile",
        0,
        &staged_blob.object_key,
    );
    checkpoint_visual_page(&mut conn, &current_spec, &staged_blob, &staged_page, 1, 9).unwrap();
    let legacy_spec = VisualJobSpec {
        id: visual_job_id.to_string(),
        book_id: fixture.book.id.clone(),
        source_id: fixture.source.id.clone(),
        document_revision: Revision::new(fixture.book.revision),
        renderer: "ngy-structural-svg".to_string(),
        renderer_version: "0.0.1".to_string(),
        fidelity: crate::preview::RenderFidelity::Structural,
        unit_ids: fixture.units.iter().map(|unit| unit.id.clone()).collect(),
        profile: RenderProfile::default(),
    };
    index_jobs::update_state(
        &conn,
        visual_job_id,
        IndexJobStatus::Succeeded,
        &encode_persisted_visual_job(legacy_spec, 1).unwrap(),
        None,
        10,
        Some(9),
        Some(10),
    )
    .unwrap();
    let visual_chunk = SearchChunk {
        id: "vision-chunk-old-renderer".to_string(),
        book_id: fixture.book.id.clone(),
        source_id: fixture.source.id.clone(),
        content_unit_id: fixture.units[0].id.clone(),
        ordinal: 1_000_000_000,
        heading: "旧视觉结果".to_string(),
        body: "stale OCR".to_string(),
        token_count: 2,
        content_hash: "stale-vision-hash".to_string(),
        locator_json: fixture.units[0].source_locator_json.clone(),
        created_at: 9,
    };
    search_chunks::insert(&conn, &visual_chunk).unwrap();
    let visual_embedding = Embedding {
        id: "embedding-old-vision".to_string(),
        search_chunk_id: visual_chunk.id.clone(),
        model: "embed-test".to_string(),
        dimensions: 1,
        vector: 1_f32.to_le_bytes().to_vec(),
        created_at: 9,
    };
    embeddings::upsert(&conn, &visual_embedding).unwrap();
    index_jobs::insert(
        &conn,
        &IndexJob {
            id: "post-vision-embedding-old".to_string(),
            book_id: fixture.book.id.clone(),
            source_id: Some(fixture.source.id.clone()),
            kind: "embedding".to_string(),
            status: IndexJobStatus::Succeeded,
            pause_requested: false,
            cancel_requested: false,
            attempts: 1,
            cursor_json: serde_json::json!({
                "schema_version": 1,
                "book_id": fixture.book.id,
                "source_id": fixture.source.id,
                "revision": fixture.book.revision,
                "kind": "embedding",
                "model": "embed-test",
                "next_ordinal": 2,
            })
            .to_string(),
            error: None,
            created_at: 9,
            updated_at: 9,
            started_at: Some(9),
            finished_at: Some(9),
        },
    )
    .unwrap();

    let descriptor = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
    let reconciliation = reconcile_visual_render_jobs(
        &mut conn,
        std::slice::from_ref(&descriptor),
        &RenderProfile::default(),
        20,
    )
    .unwrap();
    assert_eq!(reconciliation.changed_sources, 1);
    assert_eq!(
        reconciliation.unreferenced_blobs,
        vec![fixture.additional_blobs[1].clone(), staged_blob]
    );

    let visual_job = index_jobs::get(&conn, visual_job_id).unwrap().unwrap();
    assert_eq!(visual_job.status, IndexJobStatus::Queued);
    let (spec, completed_pages) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
    assert_eq!(completed_pages, 0);
    assert_eq!(spec.renderer, "ngy-structural-png");
    assert_eq!(spec.renderer_version, descriptor.version);
    assert!(
        visual_pages::list_for_source(&conn, "source-1")
            .unwrap()
            .is_empty()
    );
    assert!(
        search_chunks::get(&conn, &visual_chunk.id)
            .unwrap()
            .is_none()
    );
    assert!(
        embeddings::get(&conn, &visual_embedding.id)
            .unwrap()
            .is_none()
    );
    assert!(
        index_jobs::get(&conn, "post-vision-embedding-old")
            .unwrap()
            .is_none()
    );
    let vision = index_jobs::get(&conn, "vision:source-1").unwrap().unwrap();
    assert_eq!(vision.status, IndexJobStatus::Queued);
    let vision_cursor: serde_json::Value = serde_json::from_str(&vision.cursor_json).unwrap();
    assert_eq!(vision_cursor["next_ordinal"], 0);
    assert!(
        index_jobs::get(&conn, "embedding:source-1")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        reconcile_visual_render_jobs(
            &mut conn,
            std::slice::from_ref(&descriptor),
            &RenderProfile::default(),
            21,
        )
        .unwrap(),
        VisualRenderReconciliation::default()
    );

    let changed_profile = RenderProfile {
        name: "high-density".to_string(),
        scale_milli: 1_500,
        ..RenderProfile::default()
    };
    let reconciliation = reconcile_visual_render_jobs(
        &mut conn,
        std::slice::from_ref(&descriptor),
        &changed_profile,
        22,
    )
    .unwrap();
    assert_eq!(reconciliation.changed_sources, 1);
    assert!(reconciliation.unreferenced_blobs.is_empty());
    let visual_job = index_jobs::get(&conn, visual_job_id).unwrap().unwrap();
    let (spec, completed_pages) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
    assert_eq!(completed_pages, 0);
    assert_eq!(spec.profile, changed_profile);
}

#[cfg(target_os = "windows")]
#[test]
fn office_opt_in_is_atomic_and_selects_the_enhanced_renderer() {
    let (_temp, mut conn) = open_database();
    let mut fixture = Fixture::new();
    fixture.book.format = "pptx".to_string();
    fixture.source.format = "pptx".to_string();
    fixture.source_blob.media_type =
        "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    let structural = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
    let enhanced = RendererDescriptor {
        renderer: "ngy-office-com-enhanced".to_string(),
        version: "test-office-renderer".to_string(),
        fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
    };
    let renderers = vec![structural.clone(), enhanced.clone()];
    let (source_id, reconciliation) = set_office_enhancement(
        &mut conn,
        &fixture.book.id,
        true,
        &renderers,
        &RenderProfile::default(),
        20,
    )
    .unwrap();

    assert_eq!(source_id, fixture.source.id);
    assert_eq!(reconciliation.changed_sources, 1);
    assert_eq!(
        reconciliation.unreferenced_blobs,
        vec![fixture.additional_blobs[1].clone()]
    );
    assert!(crate::db::office_enhancements::is_enabled(&conn, &fixture.book.id).unwrap());
    let visual_job = index_jobs::get(&conn, "visual-render:source-1")
        .unwrap()
        .unwrap();
    let (spec, completed_pages) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
    assert_eq!(completed_pages, 0);
    assert_eq!(spec.renderer, enhanced.renderer);
    assert_eq!(spec.renderer_version, enhanced.version);
    assert_eq!(
        spec.fidelity,
        crate::preview::RenderFidelity::OfficeEnhanced
    );
    assert!(
        visual_pages::list_for_source(&conn, &fixture.source.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        index_jobs::get(&conn, "vision:source-1")
            .unwrap()
            .unwrap()
            .status,
        IndexJobStatus::Queued
    );

    // Startup reconciliation must derive the same renderer solely from
    // the durable opt-in even if the visual job row was lost or stale.
    index_jobs::delete(&conn, "visual-render:source-1").unwrap();
    let startup =
        reconcile_visual_render_jobs(&mut conn, &renderers, &RenderProfile::default(), 20).unwrap();
    assert_eq!(startup.changed_sources, 1);
    let startup_job = index_jobs::get(&conn, "visual-render:source-1")
        .unwrap()
        .unwrap();
    let (startup_spec, _) = decode_persisted_visual_job(&startup_job.cursor_json).unwrap();
    assert_eq!(startup_spec.renderer, enhanced.renderer);

    let (_, reconciliation) = set_office_enhancement(
        &mut conn,
        &fixture.book.id,
        false,
        &renderers,
        &RenderProfile::default(),
        21,
    )
    .unwrap();
    assert_eq!(reconciliation.changed_sources, 1);
    assert!(!crate::db::office_enhancements::is_enabled(&conn, &fixture.book.id).unwrap());
    let visual_job = index_jobs::get(&conn, "visual-render:source-1")
        .unwrap()
        .unwrap();
    let (spec, _) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
    assert_eq!(spec.renderer, structural.renderer);
    assert_eq!(spec.fidelity, crate::preview::RenderFidelity::Structural);
}

#[cfg(target_os = "windows")]
#[test]
fn preview_only_office_pdf_with_fewer_pages_than_units_stays_current_after_reopen() {
    let (temp, mut conn) = open_database();
    let mut fixture = Fixture::new();
    fixture.book.format = "docx".to_string();
    fixture.source.format = "docx".to_string();
    fixture.source_blob.media_type =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string();
    let mut second_unit = fixture.units[0].clone();
    second_unit.id = "unit-2".to_string();
    second_unit.ordinal = 3;
    second_unit.href = Some("word/section-2".to_string());
    second_unit.source_locator_json =
        serde_json::to_string(&SourceLocator::office_section(2)).unwrap();
    fixture.units.push(second_unit);
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    let structural = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
    let enhanced = RendererDescriptor {
        renderer: "ngy-office-com-enhanced".to_string(),
        version: "test-office-renderer".to_string(),
        fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
    };
    let renderers = vec![structural, enhanced];
    set_office_enhancement(
        &mut conn,
        &fixture.book.id,
        true,
        &renderers,
        &RenderProfile::default(),
        20,
    )
    .unwrap();
    let spec = running_visual_job(&conn);
    assert_eq!(spec.unit_ids.len(), 2);

    let blob = staged_visual_blob("office-preview-only", 41);
    let page = VisualPage {
        id: "office-preview-page-1".to_string(),
        book_id: spec.book_id.clone(),
        source_id: spec.source_id.clone(),
        content_unit_id: None,
        page_index: 0,
        object_key: blob.object_key.clone(),
        width: 800,
        height: 1200,
        render_scale: spec.profile.scale(),
        renderer: spec.renderer.clone(),
        renderer_version: spec.renderer_version.clone(),
        document_revision: spec.document_revision.get(),
        unit_revision: Revision::INITIAL.get(),
        profile_id: spec.profile.stable_id(),
        fidelity: "office_enhanced".to_string(),
        locator_json: serde_json::to_string(
            &DocumentLocator::unit(
                &spec.book_id,
                office_preview_unit_id(&spec.book_id, &spec.source_id),
            )
            .with_source(SourceLocator::office_rendered_page(1)),
        )
        .unwrap(),
        created_at: 21,
    };
    conn.execute(
        "UPDATE book_sources SET format = 'pptx' WHERE id = ?1",
        [&spec.source_id],
    )
    .unwrap();
    assert!(
        checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 21).is_err(),
        "PPTX preview pages must retain exact slide-to-unit identity"
    );
    conn.execute(
        "UPDATE book_sources SET format = 'docx' WHERE id = ?1",
        [&spec.source_id],
    )
    .unwrap();
    checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 21).unwrap();
    publish_staged_visual_pages(&mut conn, &spec, 1, 22).unwrap();
    drop(conn);

    let database_path = temp.path().join(connection::DATABASE_FILE);
    let mut reopened = connection::open_or_recreate(&database_path).unwrap();
    assert_eq!(
        reconcile_visual_render_jobs(&mut reopened, &renderers, &RenderProfile::default(), 23,)
            .unwrap(),
        VisualRenderReconciliation::default()
    );
    let pages = visual_pages::list_for_source(&reopened, &spec.source_id).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].content_unit_id, None);
}

#[cfg(target_os = "windows")]
#[test]
fn office_opt_in_rejects_non_office_sources_without_persisting_state() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let structural = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
    let enhanced = RendererDescriptor {
        renderer: "ngy-office-com-enhanced".to_string(),
        version: "test-office-renderer".to_string(),
        fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
    };

    assert!(
        set_office_enhancement(
            &mut conn,
            &fixture.book.id,
            true,
            &[structural.clone(), enhanced],
            &RenderProfile::default(),
            20,
        )
        .is_err()
    );
    assert!(
        crate::db::office_enhancements::get(&conn, &fixture.book.id)
            .unwrap()
            .is_none()
    );
    let job = index_jobs::get(&conn, "visual-render:source-1")
        .unwrap()
        .unwrap();
    let (spec, _) = decode_persisted_visual_job(&job.cursor_json).unwrap();
    assert_eq!(spec.renderer, structural.renderer);
}

#[test]
fn document_import_rolls_back_every_table_on_child_failure() {
    let (_temp, mut conn) = open_database();
    let mut fixture = Fixture::new();
    let mut duplicate = fixture.chunks[0].clone();
    duplicate.id = "chunk-duplicate".to_string();
    fixture.chunks.push(duplicate);

    assert!(insert_document(&mut conn, &fixture.graph(), true).is_err());
    assert!(!books::exists(&conn, "book-1").unwrap());
    assert!(blobs::get(&conn, "objects/source-1").unwrap().is_none());
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM search_chunks_fts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fts_count, 0);
}

#[test]
fn revision_reuses_stable_child_ids_and_keeps_only_original_history() {
    let (_temp, mut conn) = open_database();
    let first = Fixture::new();
    insert_document(&mut conn, &first.graph(), true).unwrap();
    let current_spec = running_visual_job(&conn);
    let staged_blob = staged_visual_blob("old-source", 41);
    let staged_page = staged_visual_page(
        &first,
        &current_spec,
        "staged-page-old-source",
        0,
        &staged_blob.object_key,
    );
    checkpoint_visual_page(&mut conn, &current_spec, &staged_blob, &staged_page, 1, 2).unwrap();
    let second = first.revision_two();

    let unreferenced = install_document_revision(&mut conn, &second.graph(), true).unwrap();
    assert!(
        unreferenced
            .iter()
            .any(|blob| blob.object_key == staged_blob.object_key),
        "replacing a source must release its staged page objects"
    );

    let stored = books::get(&conn, "book-1").unwrap().unwrap();
    assert_eq!(stored.revision, 2);
    assert_eq!(stored.source_object_key, "objects/source-2");
    assert_eq!(
        book_sources::list_for_book(&conn, "book-1").unwrap().len(),
        2
    );
    assert!(
        content_units::list_for_source(&conn, "source-1")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        content_units::list_for_source(&conn, "source-2")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(search_chunks::count_for_book(&conn, "book-1").unwrap(), 1);
    assert!(crate::db::schema::is_current(&conn).unwrap());
}

#[test]
fn deleting_document_cascades_graph_and_reports_unreferenced_objects() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let spec = running_visual_job(&conn);
    let staged_blob = staged_visual_blob("deleted-book", 43);
    let staged_page = staged_visual_page(
        &fixture,
        &spec,
        "staged-page-deleted-book",
        0,
        &staged_blob.object_key,
    );
    checkpoint_visual_page(&mut conn, &spec, &staged_blob, &staged_page, 1, 2).unwrap();

    let mut unreferenced = delete_document(&mut conn, "book-1")
        .unwrap()
        .into_iter()
        .map(|blob| blob.object_key)
        .collect::<Vec<_>>();
    unreferenced.sort();
    assert_eq!(
        unreferenced,
        vec![
            "objects/cover-1",
            "objects/page-1",
            "objects/source-1",
            "objects/staged-deleted-book"
        ]
    );
    assert_eq!(search_chunks::count_for_book(&conn, "book-1").unwrap(), 0);
    assert!(crate::db::schema::is_current(&conn).unwrap());
}
