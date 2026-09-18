//! Tests for persisted media authorization and atomic replacement.

use super::*;

#[test]
fn pending_media_range_is_served_on_the_background_runtime() {
    let data_dir = tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let bytes = Arc::new(b"0123456789".to_vec());
    let asset = AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, bytes.as_slice());
    let document = document_with_audio("book-a", asset.clone());
    let state = EditorWebState::new(
        "book-a".to_string(),
        "chapter-1.xhtml".to_string(),
        editor_document_shell("第一章", ""),
    );
    state
        .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
        .unwrap();
    let snapshot = state.media_snapshot();
    let expected = snapshot.assets.get(&asset.id).unwrap().clone();

    let response = runtime.block_on(async {
        spawn_editor_media(
            &services,
            snapshot,
            "book-a".to_string(),
            asset.id.clone(),
            expected,
            Some("bytes=3-6".to_string()),
        )
        .await
        .unwrap()
        .unwrap()
    });
    assert_eq!(response.status, 206);
    assert_eq!(response.body, b"3456");
    state.close_protocol();
    assert!(!state.protocol_is_open());
    drop(services);
    drop(runtime);
    close_test_data_dir(data_dir);
}

#[test]
fn pending_asset_bytes_drop_unreferenced_replacements() {
    let keep_bytes = Arc::new(b"keep".to_vec());
    let keep = AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, keep_bytes.as_slice());
    let orphan_bytes = Arc::new(b"orphan".to_vec());
    let orphan = AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, orphan_bytes.as_slice());
    let document = document_with_audio("book-a", keep.clone());
    let mut pending = HashMap::from([
        (keep.id.clone(), keep_bytes),
        (orphan.id.clone(), orphan_bytes),
    ]);

    retain_referenced_pending_assets(&mut pending, &document);
    assert_eq!(pending.len(), 1);
    assert!(pending.contains_key(&keep.id));
    assert!(!pending.contains_key(&orphan.id));
}

#[test]
fn persisted_editor_media_is_authorized_without_hydration_and_read_by_range() {
    let data_dir = tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let bytes = Arc::new(b"0123456789-persisted-audio".to_vec());
    let asset = AssetRef::from_bytes(
        AssetRole::Audio,
        "audio/ogg",
        Some("persisted.ogg".to_string()),
        bytes.as_slice(),
    );
    let stored_document = runtime.block_on(async {
        let bytes = Arc::clone(&bytes);
        let asset = asset.clone();
        services
            .spawn_library(move |library| {
                let record = library.create_book("按需媒体", "测试")?;
                let mut document = library.document(&record.id)?;
                let unit = document.units.first_mut().context("default unit")?;
                unit.document.blocks.push(Block::Audio {
                    id: "persisted-audio".to_string(),
                    asset_id: asset.id.clone(),
                    title: None,
                    caption: Vec::new(),
                });
                let metadata = EditorMediaMetadataDraft::from_inputs("音频", "按需读取")?;
                anyhow::ensure!(
                    update_media_metadata(&mut unit.document.blocks, "persisted-audio", &metadata,),
                    "audio block must accept metadata"
                );
                unit.source = serialize_source(&unit.document)?;
                document.assets.push(asset.clone());
                library.apply_document_with_assets(
                    document,
                    HashMap::from([(asset.id.clone(), bytes)]),
                )?;
                library.document(&record.id)
            })
            .await
            .unwrap()
            .unwrap()
    });

    let state = EditorWebState::new(
        stored_document.id.clone(),
        "chapter.xhtml".to_string(),
        "<html><body></body></html>".to_string(),
    );
    let search_hits = runtime.block_on(async {
        let book_id = stored_document.id.clone();
        services
            .spawn_library_read(move |library| library.search_book(&book_id, "按需读取", 10))
            .await
            .unwrap()
            .unwrap()
    });
    assert!(!search_hits.is_empty(), "media captions must enter FTS");
    state.authorize_media(&stored_document).unwrap();
    let snapshot = state.media_snapshot();
    let authorized = snapshot.assets.get(&asset.id).unwrap().clone();
    assert!(authorized.bytes.is_none());
    assert!(
        MediaService::new(snapshot)
            .serve(&stored_document.id, &asset.id, Some("bytes=2-5"))
            .is_err(),
        "persistent bytes must not be sliced from an in-memory Vec"
    );

    let response = runtime.block_on(async {
        spawn_persisted_editor_media(
            &services,
            stored_document.id.clone(),
            asset.id.clone(),
            authorized.clone(),
            Some("bytes=2-5".to_string()),
        )
        .await
        .unwrap()
        .unwrap()
    });
    assert_eq!(response.status, 206);
    let expected_content_range = format!("bytes 2-5/{}", bytes.len());
    assert_eq!(
        response.content_range.as_deref(),
        Some(expected_content_range.as_str())
    );
    assert_eq!(response.body, b"2345");

    let unsatisfiable = runtime.block_on(async {
        spawn_persisted_editor_media(
            &services,
            stored_document.id.clone(),
            asset.id.clone(),
            authorized.clone(),
            Some("bytes=100-200".to_string()),
        )
        .await
        .unwrap()
        .unwrap()
    });
    assert_eq!(unsatisfiable.status, 416);
    let expected_unsatisfiable = format!("bytes */{}", bytes.len());
    assert_eq!(
        unsatisfiable.content_range.as_deref(),
        Some(expected_unsatisfiable.as_str())
    );

    let forbidden = runtime.block_on(async {
        spawn_persisted_editor_media(
            &services,
            "another-book".to_string(),
            asset.id.clone(),
            authorized,
            None,
        )
        .await
        .unwrap()
    });
    assert!(forbidden.is_err());
    assert_eq!(Arc::strong_count(&services), 1);
    drop(services);
    drop(runtime);
    close_test_data_dir(data_dir);
}

#[test]
fn editor_media_snapshot_rejects_wrong_book_and_illegal_or_unknown_ids() {
    let bytes = Arc::new(b"owned".to_vec());
    let asset = AssetRef::from_bytes(AssetRole::Audio, "audio/mpeg", None, bytes.as_slice());
    let foreign_document = document_with_audio("book-b", asset.clone());
    let state = EditorWebState::new(
        "book-a".to_string(),
        "EPUB/Text/ch.xhtml".to_string(),
        "<html><body></body></html>".to_string(),
    );
    assert!(state.authorize_media(&foreign_document).is_err());

    let document = document_with_audio("book-a", asset.clone());
    assert!(EditorMediaAsset::checked(&asset, Arc::new(b"tampered".to_vec())).is_err());
    state
        .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
        .unwrap();
    assert!(
        MediaService::new(state.media_snapshot())
            .serve("book-b", &asset.id, None)
            .is_err()
    );
    for uri in [
        "epubeditor://content/.ngy/assets/../secret",
        "epubeditor://content/.ngy/assets/bad%2Fid",
        "epubeditor://content/.ngy/assets/unknown",
    ] {
        let response = editor_protocol_response(&state, &protocol_request(uri, None));
        assert_eq!(response.status(), 404, "{uri}");
    }
}

#[test]
fn replacing_media_atomically_revokes_the_old_preview_resource() {
    let old_bytes = Arc::new(b"old-media".to_vec());
    let old_asset =
        AssetRef::from_bytes(AssetRole::Audio, "audio/mpeg", None, old_bytes.as_slice());
    let old_document = document_with_audio("book-a", old_asset.clone());
    let state = EditorWebState::new(
        "book-a".to_string(),
        "EPUB/Text/ch.xhtml".to_string(),
        "<html><body></body></html>".to_string(),
    );
    state
        .sync_media(&old_document, Some((&old_asset, Arc::clone(&old_bytes))))
        .unwrap();

    let new_bytes = Arc::new(b"new-media".to_vec());
    let new_asset =
        AssetRef::from_bytes(AssetRole::Audio, "audio/mpeg", None, new_bytes.as_slice());
    let new_document = document_with_audio("book-a", new_asset.clone());
    state
        .sync_media(&new_document, Some((&new_asset, Arc::clone(&new_bytes))))
        .unwrap();

    let old = editor_protocol_response(
        &state,
        &protocol_request(
            format!("epubeditor://content{}", editor_asset_path(&old_asset.id)),
            None,
        ),
    );
    assert_eq!(old.status(), 404);
    let new = editor_protocol_response(
        &state,
        &protocol_request(
            format!("epubeditor://content{}", editor_asset_path(&new_asset.id)),
            None,
        ),
    );
    assert_eq!(new.status(), 200);
    assert_eq!(new.body().as_ref(), new_bytes.as_slice());
}

#[test]
fn source_is_the_only_tab_without_a_webview() {
    assert!(!EditorTab::Source.uses_webview());
    assert!(EditorTab::Preview.uses_webview());
    assert!(EditorTab::RichText.uses_webview());
}
