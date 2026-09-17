//! Tests for the persisted job store, the cursor and the bundled viewer.

use super::*;

#[test]
fn sqlite_job_store_persists_spec_and_state_transitions() {
    let temp = tempfile::tempdir().expect("temporary database directory");
    let database_path = temp.path().join("library.db");
    let connection = db::open_or_recreate(&database_path).expect("create current database");
    db::blobs::insert(
        &connection,
        &db::blobs::BlobRecord {
            object_key: "objects/test-source".to_string(),
            media_type: "application/epub+zip".to_string(),
            byte_len: 4,
            hash: "sqlite-preview-source-hash".to_string(),
            created_at: 1,
        },
    )
    .expect("insert source blob");
    db::books::insert(
        &connection,
        &db::books::BookRecord {
            id: "book-visual".to_string(),
            title: "视觉测试".to_string(),
            author: String::new(),
            language: Some("zh-CN".to_string()),
            description: None,
            format: "epub".to_string(),
            revision: 3,
            source_object_key: "objects/test-source".to_string(),
            cover_asset_id: None,
            cover_object_key: None,
            cover_mime: None,
            added_at: 1,
            updated_at: 1,
            last_spine: 0,
            group_id: None,
        },
    )
    .expect("insert test book");
    db::book_sources::insert(
        &connection,
        &db::book_sources::BookSource {
            id: "source-visual".to_string(),
            book_id: "book-visual".to_string(),
            revision: 3,
            format: "epub".to_string(),
            source_kind: "original".to_string(),
            object_key: "objects/test-source".to_string(),
            source_name: Some("test.epub".to_string()),
            created_at: 1,
        },
    )
    .expect("insert test source");
    drop(connection);

    let renderer = StructuralPngRenderer;
    let store = SqliteVisualJobStore::new(&database_path);
    let record = VisualJobRecord::queued(spec("sqlite-job", &renderer), 10);
    store.create(&record).expect("persist queued job");
    assert_eq!(store.get("sqlite-job").expect("read job"), Some(record));

    assert!(
        store
            .request_pause("sqlite-job", 11)
            .expect("request pause")
    );
    store
        .transition("sqlite-job", VisualJobState::Paused, 2, None, 12)
        .expect("persist paused state");
    let paused = store
        .get("sqlite-job")
        .expect("read paused job")
        .expect("paused job exists");
    assert_eq!(paused.state, VisualJobState::Paused);
    assert_eq!(paused.completed_pages, 2);
    assert!(!paused.pause_requested);

    assert!(
        store
            .resume("sqlite-job", 13)
            .expect("resume persisted job")
    );
    store
        .transition("sqlite-job", VisualJobState::Running, 2, None, 14)
        .expect("persist running state");
    store
        .transition(
            "sqlite-job",
            VisualJobState::Failed,
            2,
            Some("test failure"),
            15,
        )
        .expect("persist failure");
    let failed = store
        .get("sqlite-job")
        .expect("read failed job")
        .expect("failed job exists");
    assert_eq!(failed.attempts, 1);
    assert_eq!(failed.error.as_deref(), Some("test failure"));

    assert!(store.retry("sqlite-job", 16).expect("retry persisted job"));
    let retried = store
        .get("sqlite-job")
        .expect("read retried job")
        .expect("retried job exists");
    assert_eq!(retried.state, VisualJobState::Queued);
    assert_eq!(retried.completed_pages, 0);
    assert_eq!(retried.error, None);
}

#[test]
fn pdfjs_bundle_is_local_and_hardened() {
    assert!(PDFJS_VIEWER_HTML.contains("connect-src 'self'"));
    assert!(PDFJS_VIEWER_HTML.contains("worker-src 'self' blob:"));
    assert!(PDFJS_VIEWER_SCRIPT.contains("isEvalSupported: false"));
    assert!(PDFJS_VIEWER_SCRIPT.contains("enableXfa: false"));
    assert!(PDFJS_VIEWER_SCRIPT.contains("useWorkerFetch: false"));
    // The reader lays the whole document out in one scrollable column, so
    // every page is drawn on demand and released again once it is far away.
    assert!(PDFJS_VIEWER_SCRIPT.contains("IntersectionObserver("));
    assert!(PDFJS_VIEWER_SCRIPT.contains("ngy-pdf-page-changed"));
    assert!(PDFJS_VIEWER_SCRIPT.contains("ngy-pdf-selection-changed"));
    assert!(PDFJS_VIEWER_SCRIPT.contains("ngy-pdf-request-page"));
    // The compact reading preference reaches the viewer through one URL
    // parameter and one document attribute, and stale bundle assets would
    // silently ignore it.
    assert!(PDFJS_VIEWER_SCRIPT.contains("\"data-pdf-compact\""));
    assert!(PDFJS_VIEWER_HTML.contains("data-pdf-compact"));
    assert!(!PDFJS_VIEWER_SCRIPT.contains("https://"));
    let (mime, library) = bundled_pdfjs_asset("pdf.mjs").expect("local PDF.js library");
    assert_eq!(mime, "text/javascript; charset=utf-8");
    assert!(library.len() > 100_000, "PDF.js must not be a stub");
    assert!(bundled_pdfjs_asset("../pdf.mjs").is_none());
    assert!(bundled_pdfjs_asset("%2e%2e/pdf.mjs").is_none());
}

#[test]
fn persisted_cursor_round_trips_without_document_bytes() {
    let renderer = StructuralPngRenderer;
    let spec = spec("persisted-job", &renderer);
    let encoded = PersistedVisualCursor::new(spec.clone(), 7)
        .encode()
        .expect("encode cursor");
    let decoded = PersistedVisualCursor::decode(&encoded).expect("decode cursor");
    assert_eq!(decoded.spec, spec);
    assert_eq!(decoded.completed_pages, 7);
    assert!(!encoded.contains("包含 <script>"));
}
