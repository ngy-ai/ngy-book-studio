//! Creating the schema and applying the migrations that lead to it.

use super::*;

pub(crate) fn create(conn: &mut Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("无法启用数据库外键约束")?;
    let journal_mode = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
        .context("无法启用数据库 WAL 模式")?;
    ensure!(
        journal_mode.eq_ignore_ascii_case("wal"),
        "数据库未能启用 WAL 模式"
    );
    create_schema(conn)
}

/// Installs the canonical schema without applying file-backed connection
/// policy such as WAL. Keeping the DDL in this helper lets `is_current` build
/// an in-memory reference schema from the same source of truth.
pub(crate) fn create_schema(conn: &mut Connection) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动数据库初始化事务")?;
    tx.execute_batch(
        "CREATE TABLE groups (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL,
             parent_id TEXT REFERENCES groups(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE INDEX idx_groups_parent ON groups(parent_id);
         CREATE TABLE blobs (
             object_key TEXT PRIMARY KEY,
             media_type TEXT NOT NULL,
             byte_len INTEGER NOT NULL CHECK(byte_len >= 0),
             hash TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_blobs_hash ON blobs(hash);
         CREATE TABLE books (
             id TEXT PRIMARY KEY,
             title TEXT NOT NULL,
             author TEXT NOT NULL,
             language TEXT,
             description TEXT,
             format TEXT NOT NULL,
             revision INTEGER NOT NULL CHECK(revision >= 0),
             source_object_key TEXT NOT NULL REFERENCES blobs(object_key) ON DELETE RESTRICT,
             cover_asset_id TEXT REFERENCES assets(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
             added_at INTEGER NOT NULL CHECK(added_at >= 0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
             group_id TEXT REFERENCES groups(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED
         );
         CREATE INDEX idx_books_group ON books(group_id);
         CREATE INDEX idx_books_source_object ON books(source_object_key);
         CREATE INDEX idx_books_cover_asset ON books(cover_asset_id);
         CREATE TABLE book_sources (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             revision INTEGER NOT NULL CHECK(revision >= 0),
             format TEXT NOT NULL,
             source_kind TEXT NOT NULL,
             object_key TEXT NOT NULL REFERENCES blobs(object_key) ON DELETE RESTRICT,
             source_name TEXT,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_book_sources_revision ON book_sources(book_id, revision);
         CREATE INDEX idx_book_sources_object ON book_sources(object_key);
         CREATE TABLE progress (
             book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
             source_revision INTEGER NOT NULL CHECK(source_revision >= 0),
             content_unit_id TEXT REFERENCES content_units(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
             spine_index INTEGER NOT NULL DEFAULT 0 CHECK(spine_index >= 0),
             locator_json TEXT NOT NULL DEFAULT '{}',
             anchor_json TEXT,
             fraction REAL NOT NULL DEFAULT 0.0 CHECK(fraction >= 0.0 AND fraction <= 1.0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
         );
         CREATE INDEX idx_progress_content_unit ON progress(content_unit_id);
         CREATE TABLE content_units (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL REFERENCES book_sources(id) ON DELETE CASCADE,
             parent_id TEXT REFERENCES content_units(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             kind TEXT NOT NULL,
             href TEXT,
             source_locator_json TEXT NOT NULL,
             title TEXT,
             media_type TEXT,
             source_text TEXT,
             block_json TEXT NOT NULL,
             revision INTEGER NOT NULL CHECK(revision >= 0),
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
         );
         CREATE UNIQUE INDEX idx_content_units_source_ordinal ON content_units(source_id, ordinal);
         CREATE INDEX idx_content_units_book ON content_units(book_id);
         CREATE INDEX idx_content_units_parent ON content_units(parent_id);
         CREATE INDEX idx_content_units_href ON content_units(source_id, href);
         CREATE TABLE toc_entries (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL REFERENCES book_sources(id) ON DELETE CASCADE,
             parent_id TEXT REFERENCES toc_entries(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             depth INTEGER NOT NULL CHECK(depth >= 0),
             label TEXT NOT NULL,
             href TEXT,
             content_unit_id TEXT NOT NULL REFERENCES content_units(id) ON DELETE CASCADE,
             target_block_id TEXT,
             locator_json TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_toc_entries_source_ordinal ON toc_entries(source_id, ordinal);
         CREATE INDEX idx_toc_entries_book ON toc_entries(book_id);
         CREATE INDEX idx_toc_entries_parent ON toc_entries(parent_id);
         CREATE INDEX idx_toc_entries_content_unit ON toc_entries(content_unit_id);
         CREATE TABLE assets (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL REFERENCES book_sources(id) ON DELETE CASCADE,
             object_key TEXT NOT NULL REFERENCES blobs(object_key) ON DELETE RESTRICT,
             kind TEXT NOT NULL,
             href TEXT NOT NULL,
             media_type TEXT NOT NULL,
             byte_len INTEGER NOT NULL CHECK(byte_len >= 0),
             width INTEGER CHECK(width IS NULL OR width > 0),
             height INTEGER CHECK(height IS NULL OR height > 0),
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_assets_source_href ON assets(source_id, href);
         CREATE INDEX idx_assets_book_kind ON assets(book_id, kind);
         CREATE INDEX idx_assets_object ON assets(object_key);
         CREATE TABLE asset_refs (
             id TEXT PRIMARY KEY,
             content_unit_id TEXT NOT NULL REFERENCES content_units(id) ON DELETE CASCADE,
             asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
             relation TEXT NOT NULL,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             locator_json TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_asset_refs_identity ON asset_refs(content_unit_id, asset_id, relation, ordinal);
         CREATE INDEX idx_asset_refs_asset ON asset_refs(asset_id);
         CREATE TABLE search_chunks (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL REFERENCES book_sources(id) ON DELETE CASCADE,
             content_unit_id TEXT NOT NULL REFERENCES content_units(id) ON DELETE CASCADE,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             heading TEXT NOT NULL,
             body TEXT NOT NULL,
             token_count INTEGER NOT NULL DEFAULT 0 CHECK(token_count >= 0),
             content_hash TEXT NOT NULL,
             locator_json TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_search_chunks_unit_ordinal ON search_chunks(content_unit_id, ordinal);
         CREATE INDEX idx_search_chunks_book ON search_chunks(book_id);
         CREATE INDEX idx_search_chunks_source ON search_chunks(source_id);
         CREATE INDEX idx_search_chunks_hash ON search_chunks(content_hash);
         CREATE VIRTUAL TABLE search_chunks_fts USING fts5(
             search_chunk_id UNINDEXED,
             book_id UNINDEXED,
             heading,
             body,
             tokenize = 'trigram'
         );
         CREATE TRIGGER search_chunks_ai AFTER INSERT ON search_chunks BEGIN
             INSERT INTO search_chunks_fts(search_chunk_id, book_id, heading, body)
             VALUES (new.id, new.book_id, new.heading, new.body);
         END;
         CREATE TRIGGER search_chunks_ad AFTER DELETE ON search_chunks BEGIN
             DELETE FROM search_chunks_fts WHERE search_chunk_id = old.id;
         END;
         CREATE TRIGGER search_chunks_au AFTER UPDATE ON search_chunks BEGIN
             DELETE FROM search_chunks_fts WHERE search_chunk_id = old.id;
             INSERT INTO search_chunks_fts(search_chunk_id, book_id, heading, body)
             VALUES (new.id, new.book_id, new.heading, new.body);
         END;
         CREATE TABLE embeddings (
             id TEXT PRIMARY KEY,
             search_chunk_id TEXT NOT NULL REFERENCES search_chunks(id) ON DELETE CASCADE,
             model TEXT NOT NULL,
             dimensions INTEGER NOT NULL CHECK(dimensions > 0),
             vector BLOB NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_embeddings_chunk_model ON embeddings(search_chunk_id, model);
         CREATE TABLE index_jobs (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT REFERENCES book_sources(id) ON DELETE CASCADE,
             kind TEXT NOT NULL,
             status TEXT NOT NULL CHECK(status IN ('queued', 'running', 'paused', 'succeeded', 'failed', 'cancelled')),
             pause_requested INTEGER NOT NULL DEFAULT 0 CHECK(pause_requested IN (0, 1)),
             cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0, 1)),
             attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
             cursor_json TEXT NOT NULL DEFAULT '{}',
             error TEXT,
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
             started_at INTEGER,
             finished_at INTEGER
         );
         CREATE INDEX idx_index_jobs_book_status ON index_jobs(book_id, status);
         CREATE INDEX idx_index_jobs_source ON index_jobs(source_id);
         CREATE TABLE visual_pages (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL REFERENCES book_sources(id) ON DELETE CASCADE,
             content_unit_id TEXT REFERENCES content_units(id) ON DELETE CASCADE,
             page_index INTEGER NOT NULL CHECK(page_index >= 0),
             object_key TEXT NOT NULL REFERENCES blobs(object_key) ON DELETE RESTRICT,
             width INTEGER NOT NULL CHECK(width > 0),
             height INTEGER NOT NULL CHECK(height > 0),
             render_scale REAL NOT NULL CHECK(render_scale > 0),
             renderer TEXT NOT NULL,
             renderer_version TEXT NOT NULL,
             document_revision INTEGER NOT NULL CHECK(document_revision >= 0),
             unit_revision INTEGER NOT NULL CHECK(unit_revision >= 0),
             profile_id TEXT NOT NULL,
             fidelity TEXT NOT NULL CHECK(fidelity IN ('normalized', 'structural', 'office_enhanced')),
             locator_json TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             CHECK(content_unit_id IS NOT NULL OR (
                 renderer = 'ngy-office-com-enhanced'
                 AND fidelity = 'office_enhanced'
                 AND unit_revision = 0
             ))
         );
         CREATE UNIQUE INDEX idx_visual_pages_source_page ON visual_pages(source_id, page_index);
         CREATE INDEX idx_visual_pages_book ON visual_pages(book_id);
         CREATE INDEX idx_visual_pages_object ON visual_pages(object_key);
         CREATE TABLE visual_page_staging (
             job_id TEXT NOT NULL REFERENCES index_jobs(id) ON DELETE CASCADE,
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL REFERENCES book_sources(id) ON DELETE CASCADE,
             content_unit_id TEXT REFERENCES content_units(id) ON DELETE CASCADE,
             page_index INTEGER NOT NULL CHECK(page_index >= 0),
             object_key TEXT NOT NULL REFERENCES blobs(object_key) ON DELETE RESTRICT,
             width INTEGER NOT NULL CHECK(width > 0),
             height INTEGER NOT NULL CHECK(height > 0),
             render_scale REAL NOT NULL CHECK(render_scale > 0),
             renderer TEXT NOT NULL,
             renderer_version TEXT NOT NULL,
             document_revision INTEGER NOT NULL CHECK(document_revision >= 0),
             unit_revision INTEGER NOT NULL CHECK(unit_revision >= 0),
             profile_id TEXT NOT NULL,
             fidelity TEXT NOT NULL CHECK(fidelity IN ('normalized', 'structural', 'office_enhanced')),
             locator_json TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             CHECK(content_unit_id IS NOT NULL OR (
                 renderer = 'ngy-office-com-enhanced'
                 AND fidelity = 'office_enhanced'
                 AND unit_revision = 0
             ))
         );
         CREATE UNIQUE INDEX idx_visual_page_staging_job_page
             ON visual_page_staging(job_id, page_index);
         CREATE INDEX idx_visual_page_staging_source ON visual_page_staging(source_id);
         CREATE INDEX idx_visual_page_staging_object ON visual_page_staging(object_key);
         CREATE TABLE office_enhancements (
             book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
             enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
         );
         CREATE TABLE chat_threads (
             id TEXT PRIMARY KEY,
             book_id TEXT REFERENCES books(id) ON DELETE CASCADE,
             title TEXT NOT NULL,
             scope_json TEXT NOT NULL,
             window_kind TEXT NOT NULL,
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
         );
         CREATE INDEX idx_chat_threads_book_updated ON chat_threads(book_id, updated_at);
         CREATE TABLE chat_messages (
             id TEXT PRIMARY KEY,
             thread_id TEXT NOT NULL REFERENCES chat_threads(id) ON DELETE CASCADE,
             parent_id TEXT REFERENCES chat_messages(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             role TEXT NOT NULL CHECK(role IN ('system', 'user', 'assistant', 'tool')),
             content TEXT NOT NULL,
             model TEXT,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_chat_messages_thread_ordinal ON chat_messages(thread_id, ordinal);
         CREATE INDEX idx_chat_messages_parent ON chat_messages(parent_id);
         CREATE TABLE chat_citations (
             id TEXT PRIMARY KEY,
             message_id TEXT NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
             content_unit_id TEXT REFERENCES content_units(id) ON DELETE SET NULL,
             search_chunk_id TEXT REFERENCES search_chunks(id) ON DELETE SET NULL,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             quote TEXT NOT NULL,
             document_revision INTEGER CHECK(document_revision >= 0),
             unit_revision INTEGER CHECK(unit_revision >= 0),
             locator_json TEXT,
             source_kind TEXT NOT NULL DEFAULT 'book' CHECK(source_kind IN ('book','web')),
             url TEXT,
             source_title TEXT,
             created_at INTEGER NOT NULL CHECK(created_at >= 0)
         );
         CREATE UNIQUE INDEX idx_chat_citations_message_ordinal ON chat_citations(message_id, ordinal);
         CREATE INDEX idx_chat_citations_content_unit ON chat_citations(content_unit_id);
         CREATE INDEX idx_chat_citations_search_chunk ON chat_citations(search_chunk_id);
         CREATE TABLE annotations (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             content_unit_id TEXT NOT NULL CHECK(length(content_unit_id) > 0),
             document_revision INTEGER NOT NULL CHECK(document_revision >= 0),
             unit_revision INTEGER NOT NULL CHECK(unit_revision >= 0),
             quote TEXT NOT NULL CHECK(length(CAST(quote AS BLOB)) BETWEEN 1 AND 32768),
             start_offset INTEGER NOT NULL CHECK(start_offset BETWEEN 0 AND 4294967295),
             end_offset INTEGER NOT NULL CHECK(end_offset > start_offset AND end_offset <= 4294967295),
             kind TEXT NOT NULL CHECK(kind IN ('highlight', 'wavy', 'underline', 'human_comment', 'ai_comment')),
             comment TEXT,
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= created_at),
             CHECK((kind = 'human_comment' AND comment IS NOT NULL AND length(trim(comment)) > 0 AND length(CAST(comment AS BLOB)) <= 65536)
                OR (kind = 'ai_comment' AND comment IS NOT NULL AND length(trim(comment)) > 0 AND length(CAST(comment AS BLOB)) <= 1048576)
                OR (kind NOT IN ('human_comment', 'ai_comment') AND comment IS NULL))
         );
         CREATE INDEX idx_annotations_book_unit ON annotations(book_id, content_unit_id, created_at);
         CREATE UNIQUE INDEX idx_annotations_one_mark_per_anchor
             ON annotations(book_id, content_unit_id, document_revision, unit_revision, start_offset, end_offset)
             WHERE kind IN ('highlight', 'wavy', 'underline');
         CREATE TABLE translations (
             id TEXT PRIMARY KEY,
             book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
             content_unit_id TEXT NOT NULL REFERENCES content_units(id) ON DELETE CASCADE,
             block_id TEXT NOT NULL CHECK(length(block_id) > 0),
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             document_revision INTEGER NOT NULL CHECK(document_revision >= 0),
             unit_revision INTEGER NOT NULL CHECK(unit_revision >= 0),
             target_language TEXT NOT NULL CHECK(length(target_language) > 0),
             source_language TEXT,
             model TEXT NOT NULL CHECK(length(model) > 0),
             source_text TEXT NOT NULL CHECK(length(source_text) > 0),
             translated_text TEXT NOT NULL CHECK(length(translated_text) > 0),
             manual_text TEXT CHECK(manual_text IS NULL OR length(manual_text) > 0),
             created_at INTEGER NOT NULL CHECK(created_at >= 0),
             updated_at INTEGER NOT NULL CHECK(updated_at >= created_at)
         );
         CREATE UNIQUE INDEX idx_translations_scope
             ON translations(book_id, content_unit_id, block_id, target_language);
         CREATE INDEX idx_translations_book ON translations(book_id);
         CREATE INDEX idx_translations_unit_language
             ON translations(content_unit_id, target_language);
         CREATE TABLE settings (
             key TEXT PRIMARY KEY,
             value_json TEXT NOT NULL,
             updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
         );",
    )
    .context("无法创建数据库结构")?;
    tx.pragma_update(None, "application_id", APPLICATION_ID)
        .context("无法写入数据库应用标识")?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .context("无法写入数据库结构版本")?;
    tx.commit().context("无法提交数据库初始化事务")?;
    Ok(())
}
