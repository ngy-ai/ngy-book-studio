//! Tests for schema creation, matching and consistency checks.

use super::*;
use crate::db::{self, DatabaseOpenState};

fn assert_schema_mutation_triggers_rebuild(mutation: &str) {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&database_path).unwrap();
    conn.execute_batch(mutation).unwrap();

    assert!(!is_current(&conn).unwrap());
    drop(conn);

    let reopened = db::open_or_recreate_with_state(&database_path).unwrap();
    assert_eq!(reopened.state, DatabaseOpenState::Recreated);
    assert!(is_current(&reopened.connection).unwrap());
}

fn assert_cross_book_source_triggers_rebuild(mutation: &str) {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&database_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source-a', 'application/epub+zip', 1, 'source-a-hash', 1),
                        ('objects/source-b', 'application/epub+zip', 1, 'source-b-hash', 1);
             INSERT INTO books(id, title, author, format, revision, source_object_key,
                               added_at, updated_at)
                 VALUES ('book-a', 'Book A', '', 'epub', 1, 'objects/source-a', 1, 1),
                        ('book-b', 'Book B', '', 'epub', 1, 'objects/source-b', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source-a', 'book-a', 1, 'epub', 'original',
                         'objects/source-a', 1),
                        ('source-b', 'book-b', 1, 'epub', 'original',
                         'objects/source-b', 1);",
    )
    .unwrap();
    assert_eq!(pragma_i64(&conn, "user_version").unwrap(), SCHEMA_VERSION);
    assert!(is_current(&conn).unwrap());

    conn.execute_batch(mutation).unwrap();
    assert!(!foreign_key_violation_exists(&conn).unwrap());
    assert!(!is_current(&conn).unwrap());
    drop(conn);

    let reopened = db::open_or_recreate_with_state(&database_path).unwrap();
    assert_eq!(reopened.state, DatabaseOpenState::Recreated);
    assert!(is_current(&reopened.connection).unwrap());
    assert_eq!(
        reopened
            .connection
            .query_row("SELECT COUNT(*) FROM books", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

fn search_index_fixture() -> (tempfile::TempDir, Connection) {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&database_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'source-hash', 1);
             INSERT INTO books(id, title, author, format, revision, source_object_key,
                               added_at, updated_at)
                 VALUES ('book', 'Book', '', 'epub', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'epub', 'original', 'objects/source', 1);
             INSERT INTO content_units(
                 id, book_id, source_id, ordinal, kind,
                 source_locator_json, block_json, revision, created_at, updated_at)
                 VALUES ('unit', 'book', 'source', 0, 'chapter',
                         '{}', '{\"schema_version\":1,\"blocks\":[]}', 1, 1, 1);
             INSERT INTO search_chunks(
                 id, book_id, source_id, content_unit_id, ordinal, heading, body,
                 token_count, content_hash, locator_json, created_at)
                 VALUES ('chunk-a', 'book', 'source', 'unit', 0, 'Heading A', 'Body A',
                         2, 'hash-a', '{}', 1),
                        ('chunk-b', 'book', 'source', 'unit', 1, 'Heading B', 'Body B',
                         2, 'hash-b', '{}', 1);",
    )
    .unwrap();
    let locator = serde_json::to_string(&DocumentLocator::unit("book", "unit")).unwrap();
    conn.execute("UPDATE search_chunks SET locator_json = ?1", [&locator])
        .unwrap();
    assert!(search_index_is_synchronized(&conn).unwrap());
    (temp, conn)
}

#[test]
fn schema_has_no_inline_object_payloads() {
    assert!(BLOB_COLUMNS.iter().all(|column| column.name != "data"));
    assert!(
        BOOK_COLUMNS
            .iter()
            .all(|column| !matches!(column.name, "epub_data" | "cover_bytes"))
    );
}

#[test]
fn legacy_chapter_source_kind_rebuilds_same_version_database() {
    assert_schema_mutation_triggers_rebuild(
        "ALTER TABLE content_units ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'markdown';",
    );
}

#[test]
fn missing_annotation_index_rebuilds_same_version_database() {
    assert_schema_mutation_triggers_rebuild("DROP INDEX idx_annotations_book_unit;");
}

#[test]
fn missing_or_modified_exclusive_annotation_index_rebuilds_database() {
    assert_schema_mutation_triggers_rebuild("DROP INDEX idx_annotations_one_mark_per_anchor;");
    assert_schema_mutation_triggers_rebuild(
            "DROP INDEX idx_annotations_one_mark_per_anchor;
             CREATE UNIQUE INDEX idx_annotations_one_mark_per_anchor
                 ON annotations(book_id, content_unit_id, document_revision, unit_revision, start_offset, end_offset)
                 WHERE kind = 'highlight';",
        );
}

#[test]
fn annotation_cross_book_unit_triggers_rebuild() {
    assert_cross_book_source_triggers_rebuild(
        "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                source_locator_json, block_json, revision, created_at, updated_at)
             VALUES ('unit-b', 'book-b', 'source-b', 0, 'chapter', '{}', '{}', 1, 1, 1);
             INSERT INTO annotations(id, book_id, content_unit_id, document_revision, unit_revision,
                quote, start_offset, end_offset, kind, created_at, updated_at)
             VALUES ('note', 'book-a', 'unit-b', 1, 1, 'text', 0, 4, 'highlight', 1, 1);",
    );
}

#[test]
fn missing_translation_index_rebuilds_same_version_database() {
    assert_schema_mutation_triggers_rebuild("DROP INDEX idx_translations_scope;");
}

#[test]
fn translation_cross_book_unit_triggers_rebuild() {
    assert_cross_book_source_triggers_rebuild(
        "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                source_locator_json, block_json, revision, created_at, updated_at)
             VALUES ('unit-b', 'book-b', 'source-b', 0, 'chapter', '{}', '{}', 1, 1, 1);
             INSERT INTO translations(id, book_id, content_unit_id, block_id, ordinal,
                document_revision, unit_revision, target_language, source_language, model,
                source_text, translated_text, created_at, updated_at)
             VALUES ('tr', 'book-a', 'unit-b', 'block-1', 0, 1, 1, 'zh-Hans', 'en', 'model',
                'source', '译文', 1, 1);",
    );
}

#[test]
fn modified_trigger_definition_rebuilds_same_version_database() {
    assert_schema_mutation_triggers_rebuild(
        "DROP TRIGGER search_chunks_ai;
             CREATE TRIGGER search_chunks_ai AFTER INSERT ON search_chunks BEGIN
                 INSERT INTO search_chunks_fts(search_chunk_id, book_id, heading, body)
                 VALUES (new.id, new.book_id, new.heading, 'tampered');
             END;",
    );
}

#[test]
fn modified_constraints_rebuild_same_version_database() {
    assert_schema_mutation_triggers_rebuild(
        "DROP TABLE progress;
             CREATE TABLE progress (
                 book_id TEXT PRIMARY KEY,
                 source_revision INTEGER NOT NULL,
                 content_unit_id TEXT,
                 spine_index INTEGER NOT NULL DEFAULT 7,
                 locator_json TEXT NOT NULL DEFAULT '{}',
                 fraction REAL NOT NULL DEFAULT 0.0,
                 updated_at INTEGER NOT NULL
             );
             CREATE INDEX idx_progress_content_unit ON progress(content_unit_id);",
    );
}

#[test]
fn search_index_sync_detects_missing_row() {
    let (_temp, conn) = search_index_fixture();
    conn.execute(
        "DELETE FROM search_chunks_fts WHERE search_chunk_id = 'chunk-b'",
        [],
    )
    .unwrap();

    assert!(!search_index_is_synchronized(&conn).unwrap());
}

#[test]
fn search_index_sync_detects_duplicate_row() {
    let (_temp, conn) = search_index_fixture();
    conn.execute_batch(
        "INSERT INTO search_chunks_fts(search_chunk_id, book_id, heading, body)
                 VALUES ('chunk-a', 'book', 'Heading A', 'Body A');",
    )
    .unwrap();

    assert!(!search_index_is_synchronized(&conn).unwrap());
}

#[test]
fn search_index_sync_detects_field_mismatch() {
    let (_temp, conn) = search_index_fixture();
    conn.execute_batch(
        "DELETE FROM search_chunks_fts WHERE search_chunk_id = 'chunk-b';
             INSERT INTO search_chunks_fts(search_chunk_id, book_id, heading, body)
                 VALUES ('chunk-b', 'book', 'Changed heading', 'Body B');",
    )
    .unwrap();

    assert!(!search_index_is_synchronized(&conn).unwrap());
}

#[test]
fn search_index_sync_detects_equal_count_missing_and_duplicate_rows() {
    let (_temp, conn) = search_index_fixture();
    conn.execute_batch(
        "DELETE FROM search_chunks_fts WHERE search_chunk_id = 'chunk-b';
             INSERT INTO search_chunks_fts(search_chunk_id, book_id, heading, body)
                 VALUES ('chunk-a', 'book', 'Heading A', 'Body A');",
    )
    .unwrap();
    let (chunk_count, fts_count) = conn
        .query_row(
            "SELECT
                     (SELECT COUNT(*) FROM search_chunks),
                     (SELECT COUNT(*) FROM search_chunks_fts)",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();

    assert_eq!(chunk_count, fts_count);
    assert!(!search_index_is_synchronized(&conn).unwrap());
}

#[test]
fn preview_only_office_search_chunk_rebuilds_same_version_database() {
    let (temp, conn) = search_index_fixture();
    assert!(is_current(&conn).unwrap());
    let database_path = temp.path().join(db::DATABASE_FILE);
    let locator = serde_json::to_string(
        &DocumentLocator::unit("book", "unit").with_source(SourceLocator::office_rendered_page(1)),
    )
    .unwrap();
    conn.execute(
        "UPDATE search_chunks SET locator_json = ?2 WHERE id = ?1",
        rusqlite::params!["chunk-a", locator],
    )
    .unwrap();
    assert!(search_index_is_synchronized(&conn).unwrap());
    assert!(!is_current(&conn).unwrap());
    drop(conn);

    let reopened = db::open_or_recreate_with_state(&database_path).unwrap();
    assert_eq!(reopened.state, DatabaseOpenState::Recreated);
    assert!(is_current(&reopened.connection).unwrap());
    assert_eq!(
        reopened
            .connection
            .query_row("SELECT COUNT(*) FROM books", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn cross_book_content_unit_source_rebuilds_same_version_database() {
    assert_cross_book_source_triggers_rebuild(
        "INSERT INTO content_units(
                 id, book_id, source_id, ordinal, kind,
                 source_locator_json, block_json, revision, created_at, updated_at)
             VALUES (
                 'unit-a', 'book-a', 'source-b', 0, 'chapter',
                 '{\"type\":\"created\"}', '{\"schema_version\":1,\"blocks\":[]}',
                 1, 1, 1
             );",
    );
}

#[test]
fn cross_book_index_job_source_rebuilds_same_version_database() {
    assert_cross_book_source_triggers_rebuild(
        "INSERT INTO index_jobs(
                 id, book_id, source_id, kind, status, cursor_json, created_at, updated_at)
             VALUES (
                 'job-a', 'book-a', 'source-b', 'embedding', 'queued', '{}', 1, 1
             );",
    );
}

#[test]
fn malformed_preview_only_visual_page_rebuilds_same_version_database() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&database_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'source-hash', 1),
                        ('objects/page', 'image/png', 1, 'page-hash', 1);
             INSERT INTO books(id, title, author, format, revision, source_object_key,
                               added_at, updated_at)
                 VALUES ('book', 'book', '', 'docx', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'docx', 'original', 'objects/source', 1);
             INSERT INTO visual_pages(
                 id, book_id, source_id, content_unit_id, page_index, object_key,
                 width, height, render_scale, renderer, renderer_version,
                 document_revision, unit_revision, profile_id, fidelity,
                 locator_json, created_at)
                 VALUES ('page', 'book', 'source', NULL, 0, 'objects/page',
                         1, 1, 1.0, 'ngy-office-com-enhanced', '1', 1, 0, 'profile',
                         'office_enhanced', '{}', 1);",
    )
    .unwrap();

    assert!(!is_current(&conn).unwrap());
    drop(conn);
    let reopened = db::open_or_recreate_with_state(&database_path).unwrap();
    assert_eq!(reopened.state, DatabaseOpenState::Recreated);
    assert!(is_current(&reopened.connection).unwrap());
}

#[test]
fn extra_legacy_schema_objects_rebuild_same_version_database() {
    assert_schema_mutation_triggers_rebuild(
        "CREATE TABLE legacy_payload(value TEXT);
             CREATE INDEX legacy_payload_value ON legacy_payload(value);
             CREATE TRIGGER legacy_payload_ai AFTER INSERT ON legacy_payload BEGIN
                 SELECT new.value;
             END;",
    );
}

#[test]
fn sqlite_internal_and_fts_shadow_objects_are_allowed() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&database_path).unwrap();

    conn.execute_batch("ANALYZE").unwrap();
    let fts_shadow_count = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name GLOB 'search_chunks_fts_*'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();

    assert!(fts_shadow_count > 0);
    assert!(is_current(&conn).unwrap());
}

#[test]
fn schema_sql_normalization_ignores_layout_but_preserves_literals() {
    assert_eq!(
        normalize_schema_sql("CREATE TABLE demo (value TEXT CHECK(value = 'A B'));"),
        normalize_schema_sql(
            "create /* layout */ table demo(\n value text check ( value='A B' ) ) ;"
        )
    );
    assert_ne!(
        normalize_schema_sql("CREATE TABLE demo (value TEXT DEFAULT 'A B')"),
        normalize_schema_sql("CREATE TABLE demo (value TEXT DEFAULT 'a b')")
    );
}
