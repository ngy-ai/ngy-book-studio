use super::*;

#[cfg(target_os = "windows")]
#[test]
fn office_opt_in_replaces_persisted_visual_pages_through_app_services() {
    use office_oxide::{DocumentFormat, create::create_from_markdown_to_writer};

    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let source: Arc<dyn VisualDocumentSource> = Arc::new(LibraryVisualDocumentSource {
        library: Arc::clone(&services.library),
    });
    let sink: Arc<dyn VisualPageSink> = Arc::new(LocalVisualPageSink {
        db_path: services.db_path.clone(),
        blobs: Arc::clone(&services.blobs),
        blob_publication: services.blob_publication.clone(),
        indexing: Arc::clone(&services.indexing),
    });
    let (started_tx, started_rx) = async_channel::bounded(1);
    let (release_tx, release_rx) = async_channel::bounded(1);
    services
        .replace_visual_renderers(
            source,
            sink,
            vec![
                Arc::new(StructuralPngRenderer),
                Arc::new(OfficeEnhancedRenderer::new(Arc::new(
                    GatedFakeOfficeSlides {
                        started: started_tx,
                        release: release_rx,
                    },
                ))),
            ],
        )
        .unwrap();

    let mut pptx = Cursor::new(Vec::new());
    create_from_markdown_to_writer(
        "# First\n\nOfficeEnhancedOne\n\n---\n\n# Second\n\nOfficeEnhancedTwo",
        DocumentFormat::Pptx,
        &mut pptx,
    )
    .unwrap();
    let source_path = temp.path().join("enhanced.pptx");
    std::fs::write(&source_path, pptx.into_inner()).unwrap();
    let runtime = services.runtime();
    let record = runtime
        .block_on(async {
            services
                .spawn_library(move |library| match library.import(&source_path)? {
                    ImportOutcome::Added(record) | ImportOutcome::AlreadyExists(record) => {
                        Ok(record)
                    }
                })
                .await
                .context("Office test import worker stopped")?
        })
        .unwrap();
    let source_id = db::book_sources::get_revision(
        &db::open_conn(&services.db_path).unwrap(),
        &record.id,
        record.revision,
    )
    .unwrap()
    .unwrap()
    .id;
    let job_id = format!("visual-render:{source_id}");

    let submitted_job_id = runtime
        .block_on(services.set_office_enhancement_enabled(record.id.clone(), true))
        .unwrap();
    assert_eq!(submitted_job_id, job_id);
    assert!(
        runtime
            .block_on(services.office_enhancement_enabled(record.id.clone()))
            .unwrap()
    );
    let coordinator = services.visual_jobs().unwrap();
    let running = runtime
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(5), started_rx.recv()).await??;
            let running = coordinator.status(&job_id).await?.unwrap();
            assert_eq!(running.state, VisualJobState::Running);
            assert!(!coordinator.recover(&job_id).await?);
            // Deterministically place the caller's post-commit notification
            // after worker takeover, without a timing-based sleep.
            coordinator.schedule_committed(&running.spec).await?;
            release_tx.send(()).await?;
            Ok::<_, anyhow::Error>(running)
        })
        .unwrap();
    let succeeded = runtime
        .block_on(services.visual_jobs().unwrap().wait_for_state(
            &job_id,
            VisualJobState::Succeeded,
            Duration::from_secs(5),
        ))
        .unwrap();
    assert_eq!(succeeded.spec, running.spec);
    assert_eq!(succeeded.attempts, 1);
    assert!(!runtime.block_on(coordinator.recover(&job_id)).unwrap());
    runtime
        .block_on(coordinator.schedule_committed(&running.spec))
        .expect("a completed committed Office job is still successfully submitted");
    assert_eq!(
        runtime
            .block_on(coordinator.status(&job_id))
            .unwrap()
            .unwrap(),
        succeeded,
        "late confirmation must not reset the completed job"
    );
    let pages =
        db::visual_pages::list_for_source(&db::open_conn(&services.db_path).unwrap(), &source_id)
            .unwrap();
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().all(|page| {
        page.renderer == "ngy-office-com-enhanced"
            && page.fidelity == "office_enhanced"
            && serde_json::from_str::<crate::document::DocumentLocator>(&page.locator_json)
                .unwrap()
                .source
                .is_some()
    }));
    let loaded_pages = runtime
        .block_on(services.load_office_enhanced_pages(record.id.clone()))
        .unwrap();
    assert_eq!(loaded_pages.len(), 2);
    assert_eq!(loaded_pages[0].file_name, "Page00001.png");
    assert_eq!(loaded_pages[1].file_name, "Page00002.png");
    assert!(loaded_pages.iter().all(|page| {
        page.media_type == "image/png"
            && !page.bytes.is_empty()
            && page
                .content_unit_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
            && page.locator.source.is_some()
    }));

    let submitted_job_id = runtime
        .block_on(services.set_office_enhancement_enabled(record.id.clone(), false))
        .unwrap();
    assert_eq!(submitted_job_id, job_id);
    assert!(
        !runtime
            .block_on(services.office_enhancement_enabled(record.id.clone()))
            .unwrap()
    );
    runtime
        .block_on(services.visual_jobs().unwrap().wait_for_state(
            &job_id,
            VisualJobState::Succeeded,
            Duration::from_secs(5),
        ))
        .unwrap();
    let pages =
        db::visual_pages::list_for_source(&db::open_conn(&services.db_path).unwrap(), &source_id)
            .unwrap();
    assert!(!pages.is_empty());
    assert!(pages.iter().all(|page| page.fidelity == "structural"));
    assert!(
        runtime
            .block_on(services.load_office_enhanced_pages(record.id))
            .is_err()
    );
}
