//! Replacing and reclaiming visual pages.

use super::*;

#[test]
pub(crate) fn replacing_visual_pages_reclaims_only_superseded_objects() {
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
