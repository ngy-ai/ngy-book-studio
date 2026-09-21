//! Tests for manual translation, display overrides and reader zoom.

use super::*;

/// 手工译文优先于机器译文，并且不受机器行的模型 / 端点身份影响：读者自己写下的
/// 文字不能因为换模型或换端点而消失，机器行仍然留在它背后作为「恢复机器译文」
/// 的来源。身份不匹配的机器行照旧不显示。
#[test]
fn a_manual_translation_is_shown_independently_of_the_machine_identity() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let settings = ProviderSettings {
        base_url: "https://models.example.test/v1".to_string(),
        chat_model: "chat-test".to_string(),
        // 译文行按翻译模型过滤，所以这里必须与上面写入行的模型一致。
        translation_model: "chat-test".to_string(),
        default_language: Some("zh-Hans".to_string()),
        remote_content_confirmed: true,
        confirmed_remote_endpoint: "https://models.example.test/v1/".to_string(),
        auto_run_background_jobs: false,
        ..Default::default()
    };
    let identity = translation_execution_identity(&settings).unwrap();
    let (book_id, unit_id, document_revision, unit_revision) = services
        .runtime()
        .block_on(async {
            services
                .configure_providers(settings.clone(), BTreeMap::new())
                .await?;
            let outcome = services
                .spawn_library(|library| Ok(library.create_book("Manual book", "Author")?.id))
                .await
                .context("library test worker stopped")?;
            let book_id = outcome?;
            let conn = db::open_conn(services.database_path())?;
            let book = db::books::get(&conn, &book_id)?.context("the created book disappeared")?;
            let source = db::book_sources::list_for_book(&conn, &book_id)?
                .into_iter()
                .next()
                .context("the created book has no source")?;
            let unit = db::content_units::list_for_source(&conn, &source.id)?
                .into_iter()
                .next()
                .context("the created book has no chapter")?;
            Ok::<_, anyhow::Error>((book_id, unit.id.clone(), book.revision, unit.revision))
        })
        .unwrap();

    let stored = |identity: &str, block_id: &str, translated: String| {
        serde_json::to_string(&crate::ai::translation::StoredTranslation {
            execution_identity: identity.to_string(),
            segments: vec![crate::ai::translation::TranslationSegment {
                source: format!("source-{block_id}"),
                translated,
            }],
        })
        .unwrap()
    };
    let write =
        |block_id: &str, ordinal: u64, model: &str, row_identity: &str, manual: Option<String>| {
            let conn = db::open_conn(&services.db_path).unwrap();
            db::translations::upsert(
                &conn,
                &db::translations::NewTranslation {
                    book_id: book_id.clone(),
                    content_unit_id: unit_id.clone(),
                    block_id: block_id.to_string(),
                    ordinal,
                    document_revision,
                    unit_revision,
                    target_language: "zh-Hans".to_string(),
                    source_language: None,
                    model: model.to_string(),
                    source_text: format!("source-{block_id}"),
                    translated_text: stored(row_identity, block_id, format!("machine-{block_id}")),
                    created_at: 10,
                    updated_at: 10,
                },
            )
            .unwrap();
            if let Some(manual) = manual {
                db::translations::set_manual_text(
                    &conn,
                    &book_id,
                    &unit_id,
                    block_id,
                    "zh-Hans",
                    document_revision,
                    unit_revision,
                    Some(&manual),
                    11,
                )
                .unwrap();
            }
        };
    let stale = format!(
        "{}:stale",
        crate::ai::translation::EXECUTION_IDENTITY_PROTOCOL
    );

    // 同一模型同一身份：手工与机器各行其是。
    write(
        "block-a",
        0,
        "chat-test",
        &identity,
        Some(stored(&identity, "block-a", "manual-a".into())),
    );
    write("block-b", 1, "chat-test", &identity, None);
    // 换了模型：手工译文照旧显示，机器译文按既有规则不显示。
    write(
        "block-c",
        2,
        "other-model",
        &identity,
        Some(stored(&identity, "block-c", "manual-c".into())),
    );
    write("block-d", 3, "other-model", &identity, None);
    // 同名模型换了端点（身份不同）：同上。
    write(
        "block-e",
        4,
        "chat-test",
        &stale,
        Some(stored(&stale, "block-e", "manual-e".into())),
    );
    write("block-f", 5, "chat-test", &stale, None);
    // 手工译文不可解析时退回机器译文，不丢整章。
    write(
        "block-g",
        6,
        "chat-test",
        &identity,
        Some("not json".to_string()),
    );
    // 章节版本已经变化的手工译文同样不显示。
    {
        let conn = db::open_conn(&services.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: book_id.clone(),
                content_unit_id: unit_id.clone(),
                block_id: "block-h".to_string(),
                ordinal: 7,
                document_revision,
                unit_revision: unit_revision + 1,
                target_language: "zh-Hans".to_string(),
                source_language: None,
                model: "chat-test".to_string(),
                source_text: "source-block-h".to_string(),
                translated_text: stored(&identity, "block-h", "machine-block-h".into()),
                created_at: 10,
                updated_at: 10,
            },
        )
        .unwrap();
    }

    let blocks = block_on_without_tokio(
        services.translation_blocks_for_unit(book_id.clone(), unit_id.clone()),
    )
    .unwrap();
    let shape = blocks
        .iter()
        .map(|block| {
            (
                block.key.clone(),
                block.manual,
                block
                    .segments
                    .first()
                    .map(|segment| segment.translated.clone()),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shape,
        vec![
            ("block-a".to_string(), true, Some("manual-a".to_string())),
            (
                "block-b".to_string(),
                false,
                Some("machine-block-b".to_string())
            ),
            ("block-c".to_string(), true, Some("manual-c".to_string())),
            ("block-e".to_string(), true, Some("manual-e".to_string())),
            (
                "block-g".to_string(),
                false,
                Some("machine-block-g".to_string())
            ),
        ],
        "a manual edit outlives the model identity; a stale machine row still does not"
    );
    assert_eq!(
        db::translations::list_for_unit(
            &db::open_conn(&services.db_path).unwrap(),
            &unit_id,
            "zh-Hans",
        )
        .unwrap()
        .iter()
        .filter(|row| row.manual_text.is_some())
        .count(),
        4,
        "displaying a manual edit never rewrites the rows behind it"
    );

    // 恢复机器译文只清掉手工列，机器文本原样返回。
    block_on_without_tokio(services.set_manual_translation(
        book_id.clone(),
        unit_id.clone(),
        "block-e".to_string(),
        None,
    ))
    .unwrap();
    let blocks = block_on_without_tokio(
        services.translation_blocks_for_unit(book_id.clone(), unit_id.clone()),
    )
    .unwrap();
    assert!(
        blocks.iter().all(|block| block.key != "block-e"),
        "the machine text of the wrong identity is not resurrected by a restore"
    );
    block_on_without_tokio(services.set_manual_translation(
        book_id.clone(),
        unit_id.clone(),
        "block-a".to_string(),
        None,
    ))
    .unwrap();
    let blocks = block_on_without_tokio(
        services.translation_blocks_for_unit(book_id.clone(), unit_id.clone()),
    )
    .unwrap();
    assert_eq!(
        blocks
            .iter()
            .find(|block| block.key == "block-a")
            .map(|block| block.manual),
        Some(false),
        "restoring falls back to the machine text of the current identity"
    );
}

#[test]
fn per_book_translation_display_override_beats_the_global_preference_and_leaves_with_the_book() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let book_id = services
        .runtime()
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Preference book", "Author")?.id))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    // A book the reading window never switched follows the global default.
    assert_eq!(
        block_on_without_tokio(services.translation_display_override(book_id.clone())).unwrap(),
        None
    );
    block_on_without_tokio(services.set_translation_display_override(
        book_id.clone(),
        Some(TranslationDisplayMode::Bilingual),
    ))
    .unwrap();
    assert_eq!(
        block_on_without_tokio(services.translation_display_override(book_id.clone())).unwrap(),
        Some(TranslationDisplayMode::Bilingual)
    );

    // An unreadable row means "no choice yet": the global preference still
    // applies, and a chapter never fails to open because of it.
    let key = db::settings::translation_display_book_key(&book_id);
    db::settings::upsert(
        &db::open_conn(&services.db_path).unwrap(),
        &db::settings::Setting {
            key: key.clone(),
            value_json: "{\"display_mode\":\"side-by-side\"}".to_string(),
            updated_at: 1,
        },
    )
    .unwrap();
    assert_eq!(
        block_on_without_tokio(services.translation_display_override(book_id.clone())).unwrap(),
        None
    );

    block_on_without_tokio(services.set_translation_display_override(
        book_id.clone(),
        Some(TranslationDisplayMode::OriginalOnly),
    ))
    .unwrap();
    assert_eq!(
        block_on_without_tokio(services.translation_display_override(book_id.clone())).unwrap(),
        Some(TranslationDisplayMode::OriginalOnly)
    );

    // "跟随全局" in the reading window is the same row being removed, so the
    // next open follows the global preference again.
    block_on_without_tokio(services.set_translation_display_override(book_id.clone(), None))
        .unwrap();
    assert!(
        db::settings::get(&db::open_conn(&services.db_path).unwrap(), &key)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        block_on_without_tokio(services.translation_display_override(book_id.clone())).unwrap(),
        None
    );

    // Deleting the book removes the row of a book that still carries one.
    block_on_without_tokio(services.set_translation_display_override(
        book_id.clone(),
        Some(TranslationDisplayMode::Bilingual),
    ))
    .unwrap();
    let removal = book_id.clone();
    services
        .runtime()
        .block_on(async {
            services
                .spawn_library(move |library| library.remove_book(&removal))
                .await
                .context("library mutation worker stopped")?
        })
        .unwrap();
    assert!(
        db::settings::get(&db::open_conn(&services.db_path).unwrap(), &key)
            .unwrap()
            .is_none(),
        "a deleted book must not leave its display preference behind"
    );
}

/// The public surface names and the list `delete_document` sweeps are two
/// copies of the same set; a new surface must not be able to leave rows
/// behind just because nobody extended the sweep.
#[test]
fn reader_zoom_surfaces_match_the_swept_keys() {
    for surface in [
        ReaderZoomSurface::Text,
        ReaderZoomSurface::Pdf,
        ReaderZoomSurface::Page,
    ] {
        assert!(
            db::settings::READER_ZOOM_SURFACES.contains(&surface.name()),
            "{} is missing from the keys a deleted book sweeps",
            surface.name()
        );
    }
    assert_eq!(
        db::settings::READER_ZOOM_SURFACES.len(),
        3,
        "every swept key needs a ReaderZoomSurface that can write it"
    );
}

#[test]
fn per_book_reader_zoom_is_stored_per_surface_and_leaves_with_the_book() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let book_id = services
        .runtime()
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Zoom book", "Author")?.id))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    // A book whose page size was never changed keeps the reader's default.
    assert_eq!(
        block_on_without_tokio(services.reader_zoom(book_id.clone(), ReaderZoomSurface::Text))
            .unwrap(),
        None
    );

    // The surfaces never share a value: an Office book is read as chapters
    // and as page images, and each window keeps its own page size.
    block_on_without_tokio(services.set_reader_zoom(book_id.clone(), ReaderZoomSurface::Text, 22))
        .unwrap();
    block_on_without_tokio(services.set_reader_zoom(
        book_id.clone(),
        ReaderZoomSurface::Page,
        1_750,
    ))
    .unwrap();
    assert_eq!(
        block_on_without_tokio(services.reader_zoom(book_id.clone(), ReaderZoomSurface::Text))
            .unwrap(),
        Some(22)
    );
    assert_eq!(
        block_on_without_tokio(services.reader_zoom(book_id.clone(), ReaderZoomSurface::Page))
            .unwrap(),
        Some(1_750)
    );
    assert_eq!(
        block_on_without_tokio(services.reader_zoom(book_id.clone(), ReaderZoomSurface::Pdf))
            .unwrap(),
        None
    );

    // An unreadable row means "no choice yet" and never a failed open.
    let key = db::settings::reader_zoom_book_key("pdf", &book_id);
    db::settings::upsert(
        &db::open_conn(&services.db_path).unwrap(),
        &db::settings::Setting {
            key: key.clone(),
            value_json: "{\"value\":\"wide\"}".to_string(),
            updated_at: 1,
        },
    )
    .unwrap();
    assert_eq!(
        block_on_without_tokio(services.reader_zoom(book_id.clone(), ReaderZoomSurface::Pdf))
            .unwrap(),
        None
    );

    // Deleting the book takes every surface's row with it.
    block_on_without_tokio(services.set_reader_zoom(
        book_id.clone(),
        ReaderZoomSurface::Pdf,
        1_250,
    ))
    .unwrap();
    let removal = book_id.clone();
    services
        .runtime()
        .block_on(async {
            services
                .spawn_library(move |library| library.remove_book(&removal))
                .await
                .context("library mutation worker stopped")?
        })
        .unwrap();
    for surface in db::settings::READER_ZOOM_SURFACES {
        assert!(
            db::settings::get(
                &db::open_conn(&services.db_path).unwrap(),
                &db::settings::reader_zoom_book_key(surface, &book_id)
            )
            .unwrap()
            .is_none(),
            "a deleted book must not leave its {surface} page size behind"
        );
    }
}

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
