//! Shared fixtures for the chat repository tests.

mod delete_turn_test;

mod persist_test;

mod truncate_test;

pub(crate) use super::*;
pub(crate) use crate::{
    db::{books, content_units, search_chunks},
    document::{BlockDocument, SourceLocator},
};

pub(crate) fn repository() -> (tempfile::TempDir, IoRuntime, ChatRepository) {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    db::open_or_recreate(&db_path).unwrap();
    let runtime = IoRuntime::new(1).unwrap();
    let repository = ChatRepository::new(db_path, runtime.clone());
    (temp, runtime, repository)
}

pub(crate) fn insert_book_graph(db_path: &Path) {
    let conn = db::open_conn(db_path).unwrap();
    let source_key = format!("blake3/00/{}", "0".repeat(64));
    db::blobs::insert(
        &conn,
        &db::blobs::BlobRecord {
            object_key: source_key.clone(),
            media_type: "application/epub+zip".to_string(),
            byte_len: 1,
            hash: "0".repeat(64),
            created_at: 1,
        },
    )
    .unwrap();
    books::insert(
        &conn,
        &books::BookRecord {
            id: "book-1".to_string(),
            title: "Book".to_string(),
            author: "Author".to_string(),
            language: None,
            description: None,
            format: "epub".to_string(),
            revision: 1,
            source_object_key: source_key.clone(),
            cover_asset_id: None,
            cover_object_key: None,
            cover_mime: None,
            added_at: 1,
            updated_at: 1,
            group_id: None,
            last_spine: 0,
        },
    )
    .unwrap();
    db::book_sources::insert(
        &conn,
        &db::book_sources::BookSource {
            id: "source-1".to_string(),
            book_id: "book-1".to_string(),
            revision: 1,
            format: "epub".to_string(),
            source_kind: "original".to_string(),
            object_key: source_key,
            source_name: None,
            created_at: 1,
        },
    )
    .unwrap();
    content_units::insert(
        &conn,
        &content_units::ContentUnit {
            id: "unit-1".to_string(),
            book_id: "book-1".to_string(),
            source_id: "source-1".to_string(),
            parent_id: None,
            ordinal: 0,
            kind: "chapter".to_string(),
            href: Some("chapter-1.html".to_string()),
            source_locator_json: serde_json::to_string(&SourceLocator::Created).unwrap(),
            title: Some("Chapter".to_string()),
            media_type: Some("text/html".to_string()),
            source_text: Some("<p>text</p>".to_string()),
            block_json: serde_json::to_string(&BlockDocument::default()).unwrap(),
            revision: 1,
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();
    search_chunks::insert(
        &conn,
        &search_chunks::SearchChunk {
            id: "chunk-1".to_string(),
            book_id: "book-1".to_string(),
            source_id: "source-1".to_string(),
            content_unit_id: "unit-1".to_string(),
            ordinal: 0,
            heading: "Chapter".to_string(),
            body: "quoted text".to_string(),
            token_count: 2,
            locator_json: serde_json::to_string(&DocumentLocator::unit("book-1", "unit-1"))
                .unwrap(),
            content_hash: blake3::hash(b"quoted text").to_hex().to_string(),
            created_at: 1,
        },
    )
    .unwrap();
}
