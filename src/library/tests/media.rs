//! Tests for covers, media blocks and object reclamation.

use super::*;

#[test]
fn canonical_cover_is_stored_outside_sqlite() {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 0]),
    ))
    .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("封面图书", "作者").unwrap();
    let cover = CoverDraft::from_bytes(png.clone()).unwrap();
    library.set_cover(&book.id, cover).unwrap();
    assert_eq!(
        library.cover_bytes(&book.id).unwrap().unwrap().as_ref(),
        &png
    );
    library.clear_cover(&book.id).unwrap();
    assert!(library.cover_bytes(&book.id).unwrap().is_none());
}

#[test]
fn committed_import_succeeds_when_cover_cache_hydration_fails() {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 0]),
    ))
    .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("提交后封面失败", "作者").unwrap();
    let record = library
        .set_cover(&book.id, CoverDraft::from_bytes(png).unwrap())
        .unwrap();
    let cover_key = BlobKey::parse(record.cover_object_key.as_deref().unwrap()).unwrap();
    assert!(library.cover_bytes_cached(&record.id).is_some());

    // Simulate corruption discovered only after the SQLite import
    // transaction has committed. The database row and derivative jobs are
    // already durable at this point, so finalizing the import may only
    // degrade the optional in-memory cover projection.
    library
        .io
        .block_on(library.blob_store.delete(&cover_key))
        .unwrap();
    library.books.retain(|candidate| candidate.id != record.id);

    let outcome = library.finish_committed_import(record.clone());

    assert!(matches!(outcome, ImportOutcome::Added(added) if added.id == record.id));
    assert!(library.cover_bytes_cached(&record.id).is_none());
    assert_eq!(library.book_record(&record.id).unwrap().id, record.id);
    let conn = db::open_conn(library.database_path()).unwrap();
    assert!(
        !db::index_jobs::list_for_book(&conn, &record.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn committed_editor_save_succeeds_when_cover_cache_hydration_fails() {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 0]),
    ))
    .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(data_dir.clone()).unwrap();
    let created = library.create_book("提交后保存", "作者").unwrap();
    let covered = library
        .set_cover(&created.id, CoverDraft::from_bytes(png.clone()).unwrap())
        .unwrap();
    let unit_id = library.document(&created.id).unwrap().units[0].id.clone();
    let cover_key = BlobKey::parse(covered.cover_object_key.as_deref().unwrap()).unwrap();

    // The normalized source, canonical document and cover object are
    // already durable when apply_document_with_assets refreshes this
    // optional UI cache. Simulate a transient read failure at exactly that
    // post-commit boundary.
    library.fail_next_cover_cache_hydration = true;
    let updated = library
        .update_content_unit_source(
            &created.id,
            &unit_id,
            "<h1>已保存标题</h1><p>已提交的正文</p>",
        )
        .expect("a cover cache failure must not report the committed save as failed");

    assert_eq!(updated.revision, covered.revision + 1);
    assert_eq!(
        library.book_record(&created.id).unwrap().revision,
        updated.revision
    );
    assert_eq!(
        library
            .books()
            .iter()
            .find(|record| record.id == created.id)
            .unwrap()
            .revision,
        updated.revision
    );
    assert!(library.cover_bytes_cached(&created.id).is_none());
    assert!(
        library
            .io
            .block_on(library.blob_store.exists(&cover_key))
            .unwrap(),
        "the cache failure must not damage the durable cover object"
    );
    assert!(
        library.document(&created.id).unwrap().units[0]
            .source
            .contains("已提交的正文")
    );

    drop(library);
    let reopened = LibraryStore::load_from(data_dir).unwrap();
    assert_eq!(
        reopened.book_record(&created.id).unwrap().revision,
        updated.revision
    );
    assert!(
        reopened.document(&created.id).unwrap().units[0]
            .source
            .contains("已提交的正文")
    );
    assert_eq!(
        reopened.cover_bytes(&created.id).unwrap().unwrap().as_ref(),
        &png
    );
}

#[test]
fn cover_file_is_rejected_by_metadata_before_full_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("oversized-cover.png");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_COVER_BYTES + 1).unwrap();
    drop(file);

    let error = CoverDraft::read(&path).unwrap_err();
    assert!(error.to_string().contains("32 MiB 安全上限"));
}

#[test]
fn startup_gc_collects_an_object_left_before_database_commit() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    let library = LibraryStore::load_from(data_dir.clone()).unwrap();
    let key = library
        .io
        .block_on(library.blob_store.put(b"uncommitted-object"))
        .unwrap();
    assert!(
        library
            .io
            .block_on(library.blob_store.exists(&key))
            .unwrap()
    );
    drop(library);

    let reopened = LibraryStore::load_from(data_dir).unwrap();
    assert!(
        !reopened
            .io
            .block_on(reopened.blob_store.exists(&key))
            .unwrap()
    );
}

#[test]
fn removing_a_book_unlinks_first_and_reclaims_objects_in_background() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(data_dir.clone()).unwrap();
    let book = library.create_book("待删除图书", "作者").unwrap();
    let source_key = BlobKey::parse(&book.source_object_key).unwrap();

    library.remove_book(&book.id).unwrap();
    assert!(library.book_record(&book.id).is_err());
    drop(library);

    let reopened = LibraryStore::load_from(data_dir).unwrap();
    assert!(
        !reopened
            .io
            .block_on(reopened.blob_store.exists(&source_key))
            .unwrap()
    );
    let conn = db::open_conn(reopened.database_path()).unwrap();
    assert!(
        db::blobs::get(&conn, source_key.as_str())
            .unwrap()
            .is_none()
    );
}

#[test]
fn concurrent_reuse_cannot_be_deleted_by_a_stale_gc_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(data_dir).unwrap();
    let book = library.create_book("对象复用", "作者").unwrap();
    let source_key = BlobKey::parse(&book.source_object_key).unwrap();
    let source_bytes = library.source_bytes(&book.id).unwrap();
    let imported = ImportedBook {
        document: library.document(&book.id).unwrap(),
        assets: Vec::new(),
    };

    // Capture exactly the stale snapshot an unlinking transaction hands to
    // the asynchronous collector, without starting that collector yet.
    let candidates = db::transactions::delete_document(
        &mut db::open_conn(library.database_path()).unwrap(),
        &book.id,
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].object_key, source_key.as_str());

    let publication = library.blob_publication_lock();
    let publication_guard = library.io.block_on(publication.acquire());
    let publisher = library.clone();
    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    publication.observe_next_acquire(attempted_tx);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let publish_thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = publisher.persist_document(
            imported,
            &source_bytes,
            "application/epub+zip",
            None,
            "created",
            None,
        );
        finished_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    attempted_rx
        .recv_timeout(BACKGROUND_PUBLICATION_TIMEOUT)
        .expect("publisher reached the publication gate");
    assert!(
        matches!(
            finished_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "publisher must wait while reclamation owns the publication gate"
    );

    // Delete the old unreferenced instance while publication is excluded.
    // Once the gate is released, the publisher must recreate the same hash
    // before committing its new SQLite reference.
    library.io.block_on(reclaim_unreferenced_blob_while_locked(
        library.database_path(),
        &library.blob_store,
        &candidates[0],
        "测试对象",
    ));
    assert!(
        !library
            .io
            .block_on(library.blob_store.exists(&source_key))
            .unwrap()
    );
    drop(publication_guard);
    let republished = finished_rx
        .recv_timeout(BACKGROUND_PUBLICATION_TIMEOUT)
        .unwrap()
        .unwrap();
    publish_thread.join().unwrap();
    assert_eq!(republished.source_object_key, source_key.as_str());
    assert!(
        library
            .io
            .block_on(library.blob_store.exists(&source_key))
            .unwrap()
    );
    assert!(
        !db::blobs::is_unreferenced(
            &db::open_conn(library.database_path()).unwrap(),
            source_key.as_str(),
        )
        .unwrap()
    );

    // A delayed collector may still hold the pre-publication candidate.
    // Its fresh database check must retain both the bytes and metadata.
    library.io.block_on(reclaim_unreferenced_blobs(
        library.database_path(),
        &library.blob_store,
        &publication,
        candidates,
        "测试对象",
    ));
    assert!(
        library
            .io
            .block_on(library.blob_store.exists(&source_key))
            .unwrap()
    );
    assert!(
        db::blobs::get(
            &db::open_conn(library.database_path()).unwrap(),
            source_key.as_str(),
        )
        .unwrap()
        .is_some()
    );
}
