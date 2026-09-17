//! Tests for background job projections and the visual job pipeline.

use super::*;

#[test]
fn web_search_stays_off_until_enabled_and_confirmed() {
    let mut settings = ProviderSettings::default();
    assert!(settings.web_search_config(None).unwrap().is_none());

    // A loopback endpoint needs no acknowledgement.
    settings.web_search_enabled = true;
    settings.web_search_url_template =
        "http://127.0.0.1:8080/search?q={query}&format=json".to_string();
    assert!(settings.validate().is_ok());

    // The key comes from the caller (credential store) and is not persisted.
    let config = settings
        .web_search_config(Some("credential-only".to_string()))
        .unwrap()
        .expect("loopback web search config");
    assert_eq!(config.api_key.as_deref(), Some("credential-only"));

    // A remote endpoint is rejected until it is explicitly confirmed.
    settings.web_search_url_template = "https://api.example.test/search?q={query}".to_string();
    assert!(settings.validate().is_err());

    settings.web_search_confirmed_remote_endpoint =
        normalize_web_endpoint(&settings.web_search_url_template).unwrap();
    settings.web_search_remote_confirmed = true;
    assert!(settings.validate().is_ok());

    // Changing the endpoint invalidates the acknowledgement again.
    settings.web_search_url_template = "https://other.example.test/search?q={query}".to_string();
    assert!(settings.validate().is_err());
}

#[test]
fn remote_confirmation_is_bound_to_the_exact_canonical_endpoint() {
    let mut settings = ProviderSettings {
        base_url: "https://models-a.example.test/v1".to_string(),
        chat_model: "chat-test".to_string(),
        embedding_model: "embed-test".to_string(),
        vision_model: "vision-test".to_string(),
        remote_content_confirmed: true,
        allow_insecure_remote_http: false,
        confirmed_remote_endpoint: "https://models-a.example.test/v1/".to_string(),
        request_timeout_secs: 30,
        ..Default::default()
    };
    settings.validate().unwrap();

    settings.base_url = "https://models-b.example.test/v1".to_string();
    assert!(settings.validate().is_err());

    settings.confirmed_remote_endpoint = "https://models-b.example.test/v1/".to_string();
    settings.validate().unwrap();
}

#[test]
fn persisted_provider_contract_does_not_accept_legacy_unbound_confirmation() {
    let legacy = serde_json::json!({
        "base_url": "https://legacy-remote.example.test/v1",
        "chat_model": "chat-test",
        "embedding_model": "embed-test",
        "vision_model": "vision-test",
        "remote_content_confirmed": true,
        "allow_insecure_remote_http": false,
        "request_timeout_secs": 30
    });
    assert!(serde_json::from_value::<ProviderSettings>(legacy).is_err());
}

#[test]
fn background_job_snapshots_are_scoped_deduplicated_and_include_progress() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();
    let book = runtime
        .block_on(async {
            services
                .spawn_library(|library| library.create_book("Tasks", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    let jobs = runtime
        .block_on(services.background_jobs_for_books(vec![book.id.clone(), book.id.clone()]))
        .unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(
        jobs.iter().map(|job| job.kind.as_str()).collect::<Vec<_>>(),
        vec!["visual_render", "vision", "embedding"]
    );
    let visual = jobs.iter().find(|job| job.kind == "visual_render").unwrap();
    assert_eq!(visual.progress.total, None);
    assert!(
        runtime
            .block_on(services.background_jobs_for_books(Vec::new()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn background_job_snapshots_use_actual_rendered_pages_instead_of_content_unit_count() {
    use db::index_jobs::{IndexJob, IndexJobStatus};

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for status in [
        IndexJobStatus::Queued,
        IndexJobStatus::Running,
        IndexJobStatus::Paused,
        IndexJobStatus::Failed,
        IndexJobStatus::Cancelled,
        IndexJobStatus::Succeeded,
    ] {
        let job = IndexJob {
            id: "visual-render:three-chapters".into(),
            book_id: "book".into(),
            source_id: Some("source".into()),
            kind: "visual_render".into(),
            status,
            pause_requested: false,
            cancel_requested: false,
            attempts: 1,
            cursor_json: serde_json::json!({
                "completed_pages": 7,
                "spec": { "unit_ids": ["chapter-1", "chapter-2", "chapter-3"] }
            })
            .to_string(),
            error: None,
            created_at: 1,
            updated_at: 2,
            started_at: Some(1),
            finished_at: (status == IndexJobStatus::Succeeded).then_some(2),
        };
        let snapshot = background_job_snapshot(&conn, job).unwrap();
        assert_eq!(snapshot.progress.completed, 7, "{status:?}");
        assert_eq!(
            snapshot.progress.total,
            (status == IndexJobStatus::Succeeded).then_some(7),
            "{status:?} must not treat three chapters as three pages"
        );
    }
}

#[test]
fn configuring_a_default_language_enqueues_translation_for_existing_books() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();

    // This book is created with translation switched off (the shipped
    // default is Simplified Chinese), so nothing is queued for it yet.
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    runtime
        .block_on(services.configure_provider(settings, ApiKeyUpdate::Keep))
        .unwrap();

    let book = runtime
        .block_on(async {
            services
                .spawn_library(|library| library.create_book("Translated", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    // Translation is off: nothing is queued.
    let jobs = runtime
        .block_on(services.background_jobs_for_books(vec![book.id.clone()]))
        .unwrap();
    assert!(jobs.iter().all(|job| job.kind != "translation"));

    // Saving a target language enqueues the books that were imported while
    // translation was off.
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("zh-Hans".to_string());
    runtime
        .block_on(services.configure_provider(settings, ApiKeyUpdate::Keep))
        .unwrap();

    let jobs = runtime
        .block_on(services.background_jobs_for_books(vec![book.id.clone()]))
        .unwrap();
    let translation = jobs
        .iter()
        .find(|job| job.kind == "translation")
        .expect("saving a default language must enqueue a translation job");
    assert_eq!(
        translation.status,
        BackgroundJobStatus::Paused,
        "auto-run is disabled by default, so the new task starts paused"
    );

    // The durable job survives a restart and is reconciled again on startup.
    drop(services);
    let reopened =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let jobs = reopened
        .runtime()
        .block_on(reopened.background_jobs_for_books(vec![book.id.clone()]))
        .unwrap();
    assert!(
        jobs.iter().any(|job| job.kind == "translation"),
        "startup reconciliation keeps one translation job per book and language"
    );
}

#[test]
fn translation_block_inspection_runs_off_the_callers_runtime() {
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
fn structural_visual_jobs_use_the_shared_library_and_object_store() {
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
fn startup_requeues_legacy_renderer_and_reclaims_its_visual_object() {
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
fn imported_pdf_visual_job_persists_native_png_pages() {
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
fn structural_visual_queue_recovers_a_running_job_after_restart() {
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

#[test]
fn replacing_visual_pages_reclaims_only_superseded_objects() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
    let book = library.create_book("Visual replacement", "Author").unwrap();
    let document = library.document(&book.id).unwrap();
    let unit = document.units.first().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let source =
        db::book_sources::get_revision(&db::open_conn(&db_path).unwrap(), &book.id, book.revision)
            .unwrap()
            .unwrap();
    drop(library);

    let runtime = IoRuntime::default();
    let store = LocalBlobStore::new(temp.path().join(OBJECT_DIRECTORY)).unwrap();
    let old_bytes = b"old visual page";
    let reused_bytes = b"reused visual page";
    let shared_bytes = b"shared visual page";
    let replacement_bytes = b"replacement visual page";
    let old_key = runtime.block_on(store.put(old_bytes)).unwrap();
    let reused_key = runtime.block_on(store.put(reused_bytes)).unwrap();
    let shared_key = runtime.block_on(store.put(shared_bytes)).unwrap();
    let replacement_key = runtime.block_on(store.put(replacement_bytes)).unwrap();
    let now = unix_timestamp().unwrap();
    let blob = |key: &BlobKey, bytes: &[u8]| db::blobs::BlobRecord {
        object_key: key.to_string(),
        media_type: "image/png".to_string(),
        byte_len: bytes.len() as u64,
        hash: blake3::hash(bytes).to_hex().to_string(),
        created_at: now,
    };
    let old_blob = blob(&old_key, old_bytes);
    let reused_blob = blob(&reused_key, reused_bytes);
    let shared_blob = blob(&shared_key, shared_bytes);
    let replacement_blob = blob(&replacement_key, replacement_bytes);
    let page = |id: &str, index: usize, key: &BlobKey| db::visual_pages::VisualPage {
        id: id.to_string(),
        book_id: book.id.clone(),
        source_id: source.id.clone(),
        content_unit_id: Some(unit.id.clone()),
        page_index: index,
        object_key: key.to_string(),
        width: 800,
        height: 1_200,
        render_scale: 1.0,
        renderer: "test-png".to_string(),
        renderer_version: "1".to_string(),
        document_revision: book.revision,
        unit_revision: unit.revision.get(),
        profile_id: "test-profile".to_string(),
        fidelity: "structural".to_string(),
        locator_json: serde_json::to_string(&crate::document::DocumentLocator::unit(
            &book.id, &unit.id,
        ))
        .unwrap(),
        created_at: now,
    };

    let initially_unreferenced = db::transactions::replace_visual_pages(
        &mut db::open_conn(&db_path).unwrap(),
        &book.id,
        &source.id,
        book.revision,
        &[old_blob.clone(), reused_blob.clone(), shared_blob.clone()],
        &[
            page("old-page", 0, &old_key),
            page("reused-page-old", 1, &reused_key),
            page("shared-page-old", 2, &shared_key),
        ],
    )
    .unwrap();
    assert!(initially_unreferenced.is_empty());

    let superseded = db::transactions::replace_visual_pages(
        &mut db::open_conn(&db_path).unwrap(),
        &book.id,
        &source.id,
        book.revision,
        &[shared_blob.clone(), replacement_blob.clone()],
        &[
            page("shared-page-current", 0, &shared_key),
            page("replacement-page", 1, &replacement_key),
        ],
    )
    .unwrap();
    assert_eq!(superseded.len(), 2);
    assert!(superseded.contains(&old_blob));
    assert!(superseded.contains(&reused_blob));

    // Republish one of the stale candidates before its delayed collector
    // runs. A content-addressed key can be reused by another page without
    // writing different bytes, so the collector must consult current DB
    // references instead of trusting `superseded`.
    let no_longer_unreferenced = db::transactions::replace_visual_pages(
        &mut db::open_conn(&db_path).unwrap(),
        &book.id,
        &source.id,
        book.revision,
        &[
            shared_blob.clone(),
            replacement_blob.clone(),
            reused_blob.clone(),
        ],
        &[
            page("shared-page-current", 0, &shared_key),
            page("replacement-page", 1, &replacement_key),
            page("reused-page-current", 2, &reused_key),
        ],
    )
    .unwrap();
    assert!(no_longer_unreferenced.is_empty());

    let blob_publication = BlobPublicationLock::for_store(&store).unwrap();
    runtime.block_on(reclaim_unreferenced_blobs(
        &db_path,
        &store,
        &blob_publication,
        superseded,
        "视觉页面对象",
    ));

    assert!(!runtime.block_on(store.exists(&old_key)).unwrap());
    assert!(runtime.block_on(store.exists(&reused_key)).unwrap());
    assert!(runtime.block_on(store.exists(&shared_key)).unwrap());
    assert!(runtime.block_on(store.exists(&replacement_key)).unwrap());
    let conn = db::open_conn(&db_path).unwrap();
    assert!(db::blobs::get(&conn, old_key.as_str()).unwrap().is_none());
    assert!(
        db::blobs::get(&conn, reused_key.as_str())
            .unwrap()
            .is_some()
    );
    assert!(
        db::blobs::get(&conn, shared_key.as_str())
            .unwrap()
            .is_some()
    );
    assert!(
        db::blobs::get(&conn, replacement_key.as_str())
            .unwrap()
            .is_some()
    );
}

#[cfg(target_os = "windows")]
fn native_pdf_fixture() -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("Native PDF visual")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}
