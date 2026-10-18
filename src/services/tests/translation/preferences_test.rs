use super::*;

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
