//! Job snapshots, inspection and structural visual jobs.

use super::*;

#[test]
pub(crate) fn translation_block_inspection_runs_off_the_callers_runtime() {
    // The task window requests the block list from a GPUI callback, and GPUI
    // runs its futures on its own executor rather than the application I/O
    // runtime. The service must therefore move the scope check and the whole
    // revision-pinned extraction, blob read included, onto its own runtime:
    // resolving the ambient Tokio handle from a GPUI task panics in a
    // function that cannot unwind, which aborts the process.
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();
    let book = runtime
        .block_on(async {
            services
                .spawn_library(|library| library.create_book("Blocks", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    // Saving a default language is the product path that enqueues the
    // whole-book translation task this window inspects.
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("zh-Hans".to_string());
    runtime
        .block_on(services.configure_provider(settings, ApiKeyUpdate::Keep))
        .unwrap();

    let job_id = {
        let conn = db::open_conn(services.database_path()).unwrap();
        let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap();
        let job_id = format!("translation:{}:zh-Hans", source.id);
        let job = db::index_jobs::get(&conn, &job_id)
            .unwrap()
            .expect("saving a default language must enqueue a translation task");
        assert_eq!(job.status, db::index_jobs::IndexJobStatus::Paused);
        assert_eq!(job.source_id.as_deref(), Some(source.id.as_str()));
        job_id
    };

    let list = block_on_without_tokio(
        services.background_job_translation_blocks(job_id.clone(), vec![book.id.clone()]),
    )
    .unwrap();
    assert_eq!(list.target_language, "zh-Hans");
    assert!(
        list.total >= 2,
        "a created book has a heading and a paragraph"
    );
    assert_eq!(list.total, list.blocks.len());
    assert_eq!(list.next_ordinal, 0);
    assert_eq!(list.blocks[0].unit_title, "第一章");

    // The read-only inspection keeps the same scope rules as the task list
    // and the logs: a task outside the current scope grants no access.
    assert!(
        block_on_without_tokio(
            services.background_job_translation_blocks(job_id, vec!["other-book".to_string()],)
        )
        .is_err()
    );
}

#[test]
pub(crate) fn structural_visual_jobs_use_the_shared_library_and_object_store() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    // This test exercises the durable visual queue, so new derivative jobs
    // must be created queued instead of using the product default.
    services
        .auto_run_background_jobs
        .store(true, Ordering::Release);
    // This deliberately bypasses AppServices::spawn_library, matching the
    // existing UI clones. The durable visual queue must still be noticed.
    let (book, document) = {
        let mut library = services.library_snapshot().unwrap();
        let book = library.create_book("Visual", "Author").unwrap();
        let document = library.document(&book.id).unwrap();
        (book, document)
    };
    let source = db::book_sources::get_revision(
        &db::open_conn(services.database_path()).unwrap(),
        &book.id,
        book.revision,
    )
    .unwrap()
    .unwrap();
    let visual_job_id = format!("visual-render:{}", source.id);
    let coordinator = services.visual_jobs().unwrap();
    let completed = services.runtime().block_on(coordinator.wait_for_state(
        &visual_job_id,
        VisualJobState::Succeeded,
        std::time::Duration::from_secs(5),
    ));
    assert_eq!(completed.unwrap().completed_pages, document.units.len());
    let pages = db::visual_pages::list_for_source(
        &db::open_conn(services.database_path()).unwrap(),
        &source.id,
    )
    .unwrap();
    assert_eq!(pages.len(), document.units.len());
    assert!(pages.iter().all(|page| {
        page.renderer == "ngy-structural-png"
            && page.document_revision == book.revision
            && page.unit_revision == book.revision
            && page.profile_id.starts_with("render-profile-")
            && page.fidelity == "structural"
    }));
    for page in &pages {
        let blob = db::blobs::get(
            &db::open_conn(services.database_path()).unwrap(),
            &page.object_key,
        )
        .unwrap()
        .expect("persisted structural page blob");
        assert_eq!(blob.media_type, "image/png");
        let key = BlobKey::parse(&page.object_key).unwrap();
        let bytes = services
            .runtime()
            .block_on(services.blob_store().get(&key))
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}

#[test]
pub(crate) fn startup_requeues_legacy_renderer_and_reclaims_its_visual_object() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
    let book = library.create_book("Legacy visual", "Author").unwrap();
    let document = library.document(&book.id).unwrap();
    let unit = document.units.first().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let source =
        db::book_sources::get_revision(&db::open_conn(&db_path).unwrap(), &book.id, book.revision)
            .unwrap()
            .unwrap();
    let runtime = library.io_runtime();
    let store = LocalBlobStore::new(temp.path().join(OBJECT_DIRECTORY)).unwrap();
    let legacy_bytes = b"<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'/>";
    let legacy_key = runtime.block_on(store.put(legacy_bytes)).unwrap();
    let legacy_blob = db::blobs::BlobRecord {
        object_key: legacy_key.to_string(),
        media_type: "image/svg+xml".to_string(),
        byte_len: legacy_bytes.len() as u64,
        hash: blake3::hash(legacy_bytes).to_hex().to_string(),
        created_at: unix_timestamp().unwrap(),
    };
    let legacy_page = db::visual_pages::VisualPage {
        id: "legacy-svg-page".to_string(),
        book_id: book.id.clone(),
        source_id: source.id.clone(),
        content_unit_id: Some(unit.id.clone()),
        page_index: 0,
        object_key: legacy_key.to_string(),
        width: 8,
        height: 8,
        render_scale: 1.0,
        renderer: "ngy-structural-svg".to_string(),
        renderer_version: "0.0.1".to_string(),
        document_revision: book.revision,
        unit_revision: unit.revision.get(),
        profile_id: crate::preview::RenderProfile::default().stable_id(),
        fidelity: "structural".to_string(),
        locator_json: serde_json::to_string(&crate::document::DocumentLocator::unit(
            &book.id, &unit.id,
        ))
        .unwrap(),
        created_at: unix_timestamp().unwrap(),
    };
    db::transactions::replace_visual_pages(
        &mut db::open_conn(&db_path).unwrap(),
        &book.id,
        &source.id,
        book.revision,
        &[legacy_blob],
        &[legacy_page],
    )
    .unwrap();
    let job_id = format!("visual-render:{}", source.id);
    let legacy_spec = crate::preview::VisualJobSpec {
        id: job_id.clone(),
        book_id: book.id.clone(),
        source_id: source.id.clone(),
        document_revision: crate::document::Revision::new(book.revision),
        renderer: "ngy-structural-svg".to_string(),
        renderer_version: "0.0.1".to_string(),
        fidelity: crate::preview::RenderFidelity::Structural,
        unit_ids: document.units.iter().map(|unit| unit.id.clone()).collect(),
        profile: crate::preview::RenderProfile::default(),
    };
    db::index_jobs::update_state(
        &db::open_conn(&db_path).unwrap(),
        &job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &crate::preview::encode_persisted_visual_job(legacy_spec, 1).unwrap(),
        None,
        unix_timestamp().unwrap(),
        Some(unix_timestamp().unwrap()),
        Some(unix_timestamp().unwrap()),
    )
    .unwrap();
    drop(library);

    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    assert!(!runtime.block_on(store.exists(&legacy_key)).unwrap());
    assert!(
        db::blobs::get(&db::open_conn(&db_path).unwrap(), legacy_key.as_str())
            .unwrap()
            .is_none()
    );
    let completed = runtime
        .block_on(services.visual_jobs().unwrap().wait_for_state(
            &job_id,
            VisualJobState::Succeeded,
            std::time::Duration::from_secs(5),
        ))
        .unwrap();
    assert_eq!(completed.spec.renderer, "ngy-structural-png");
    let pages =
        db::visual_pages::list_for_source(&db::open_conn(&db_path).unwrap(), &source.id).unwrap();
    assert!(!pages.is_empty());
    assert!(
        pages
            .iter()
            .all(|page| page.renderer == "ngy-structural-png")
    );
}

#[cfg(target_os = "windows")]
#[test]
pub(crate) fn imported_pdf_visual_job_persists_native_png_pages() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let source_path = temp.path().join("native-visual.pdf");
    fs::write(&source_path, native_pdf_fixture()).unwrap();
    // The native renderer only runs on a queued job, not on the default
    // paused one.
    services
        .auto_run_background_jobs
        .store(true, Ordering::Release);
    let mut library = services.library_snapshot().unwrap();
    let book = match library.import(&source_path).unwrap() {
        ImportOutcome::Added(book) => book,
        ImportOutcome::AlreadyExists(_) => panic!("new PDF unexpectedly deduplicated"),
    };
    let source = db::book_sources::get_revision(
        &db::open_conn(services.database_path()).unwrap(),
        &book.id,
        book.revision,
    )
    .unwrap()
    .unwrap();
    let job_id = format!("visual-render:{}", source.id);
    let completed = services
        .runtime()
        .block_on(services.visual_jobs().unwrap().wait_for_state(
            &job_id,
            VisualJobState::Succeeded,
            std::time::Duration::from_secs(10),
        ))
        .unwrap();
    assert_eq!(completed.completed_pages, 1);
    let pages = db::visual_pages::list_for_source(
        &db::open_conn(services.database_path()).unwrap(),
        &source.id,
    )
    .unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].renderer, "ngy-windows-pdf-png");
    let key = BlobKey::parse(&pages[0].object_key).unwrap();
    let png = services
        .runtime()
        .block_on(services.blob_store().get(&key))
        .unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

    let unit_id = library.document(&book.id).unwrap().units[0].id.clone();
    let updated = library
        .update_content_unit_source(
            &book.id,
            &unit_id,
            "<h1>Normalized</h1><p>Edited PDF content</p>",
        )
        .unwrap();
    let normalized_source = db::book_sources::get_revision(
        &db::open_conn(services.database_path()).unwrap(),
        &book.id,
        updated.revision,
    )
    .unwrap()
    .unwrap();
    assert_eq!(normalized_source.source_kind, "normalized");
    assert_eq!(normalized_source.format, "epub");
    let normalized_job_id = format!("visual-render:{}", normalized_source.id);
    services
        .runtime()
        .block_on(services.visual_jobs().unwrap().wait_for_state(
            &normalized_job_id,
            VisualJobState::Succeeded,
            std::time::Duration::from_secs(10),
        ))
        .unwrap();
    let normalized_pages = db::visual_pages::list_for_source(
        &db::open_conn(services.database_path()).unwrap(),
        &normalized_source.id,
    )
    .unwrap();
    assert!(!normalized_pages.is_empty());
    assert!(
        normalized_pages
            .iter()
            .all(|page| page.renderer == "ngy-structural-png")
    );
}

#[test]
pub(crate) fn structural_visual_queue_recovers_a_running_job_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
    let book = library.create_book("Restart visual", "Author").unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let source =
        db::book_sources::get_revision(&db::open_conn(&db_path).unwrap(), &book.id, book.revision)
            .unwrap()
            .unwrap();
    let job_id = format!("visual-render:{}", source.id);
    let conn = db::open_conn(&db_path).unwrap();
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
    drop(library);

    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let completed = services
        .runtime()
        .block_on(services.visual_jobs().unwrap().wait_for_state(
            &job_id,
            VisualJobState::Succeeded,
            std::time::Duration::from_secs(5),
        ));
    assert!(completed.unwrap().attempts >= 2);
}
