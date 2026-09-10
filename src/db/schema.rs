use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::document::{DocumentLocator, SourceLocator};

/// Identifies SQLite files owned by this application (ASCII "MOYE").
pub(super) const APPLICATION_ID: i64 = 0x4D4F_5945;
/// Development schemas are deliberately rebuilt instead of migrated.
pub(super) const SCHEMA_VERSION: i64 = 13;

#[derive(Clone, Copy)]
struct ColumnSpec {
    name: &'static str,
    data_type: &'static str,
    not_null: bool,
    primary_key: i64,
}

impl ColumnSpec {
    const fn new(
        name: &'static str,
        data_type: &'static str,
        not_null: bool,
        primary_key: i64,
    ) -> Self {
        Self {
            name,
            data_type,
            not_null,
            primary_key,
        }
    }
}

struct TableSpec {
    name: &'static str,
    columns: &'static [ColumnSpec],
}

#[derive(Clone, Copy)]
struct IndexSpec {
    table: &'static str,
    name: &'static str,
    columns: &'static [&'static str],
    unique: bool,
}

const GROUP_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("name", "TEXT", true, 0),
    ColumnSpec::new("parent_id", "TEXT", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const BLOB_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("object_key", "TEXT", false, 1),
    ColumnSpec::new("media_type", "TEXT", true, 0),
    ColumnSpec::new("byte_len", "INTEGER", true, 0),
    ColumnSpec::new("hash", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const BOOK_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("title", "TEXT", true, 0),
    ColumnSpec::new("author", "TEXT", true, 0),
    ColumnSpec::new("language", "TEXT", false, 0),
    ColumnSpec::new("description", "TEXT", false, 0),
    ColumnSpec::new("format", "TEXT", true, 0),
    ColumnSpec::new("revision", "INTEGER", true, 0),
    ColumnSpec::new("source_object_key", "TEXT", true, 0),
    ColumnSpec::new("cover_asset_id", "TEXT", false, 0),
    ColumnSpec::new("added_at", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
    ColumnSpec::new("group_id", "TEXT", false, 0),
];
const BOOK_SOURCE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("revision", "INTEGER", true, 0),
    ColumnSpec::new("format", "TEXT", true, 0),
    ColumnSpec::new("source_kind", "TEXT", true, 0),
    ColumnSpec::new("object_key", "TEXT", true, 0),
    ColumnSpec::new("source_name", "TEXT", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const PROGRESS_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("book_id", "TEXT", false, 1),
    ColumnSpec::new("source_revision", "INTEGER", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", false, 0),
    ColumnSpec::new("spine_index", "INTEGER", true, 0),
    ColumnSpec::new("locator_json", "TEXT", true, 0),
    ColumnSpec::new("fraction", "REAL", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const CONTENT_UNIT_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", true, 0),
    ColumnSpec::new("parent_id", "TEXT", false, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("kind", "TEXT", true, 0),
    ColumnSpec::new("href", "TEXT", false, 0),
    ColumnSpec::new("source_locator_json", "TEXT", true, 0),
    ColumnSpec::new("title", "TEXT", false, 0),
    ColumnSpec::new("media_type", "TEXT", false, 0),
    ColumnSpec::new("source_text", "TEXT", false, 0),
    ColumnSpec::new("block_json", "TEXT", true, 0),
    ColumnSpec::new("revision", "INTEGER", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const TOC_ENTRY_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", true, 0),
    ColumnSpec::new("parent_id", "TEXT", false, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("depth", "INTEGER", true, 0),
    ColumnSpec::new("label", "TEXT", true, 0),
    ColumnSpec::new("href", "TEXT", false, 0),
    ColumnSpec::new("content_unit_id", "TEXT", true, 0),
    ColumnSpec::new("target_block_id", "TEXT", false, 0),
    ColumnSpec::new("locator_json", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const ASSET_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", true, 0),
    ColumnSpec::new("object_key", "TEXT", true, 0),
    ColumnSpec::new("kind", "TEXT", true, 0),
    ColumnSpec::new("href", "TEXT", true, 0),
    ColumnSpec::new("media_type", "TEXT", true, 0),
    ColumnSpec::new("byte_len", "INTEGER", true, 0),
    ColumnSpec::new("width", "INTEGER", false, 0),
    ColumnSpec::new("height", "INTEGER", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const ASSET_REF_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("content_unit_id", "TEXT", true, 0),
    ColumnSpec::new("asset_id", "TEXT", true, 0),
    ColumnSpec::new("relation", "TEXT", true, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("locator_json", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const SEARCH_CHUNK_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", true, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("heading", "TEXT", true, 0),
    ColumnSpec::new("body", "TEXT", true, 0),
    ColumnSpec::new("token_count", "INTEGER", true, 0),
    ColumnSpec::new("content_hash", "TEXT", true, 0),
    ColumnSpec::new("locator_json", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const EMBEDDING_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("search_chunk_id", "TEXT", true, 0),
    ColumnSpec::new("model", "TEXT", true, 0),
    ColumnSpec::new("dimensions", "INTEGER", true, 0),
    ColumnSpec::new("vector", "BLOB", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const INDEX_JOB_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", false, 0),
    ColumnSpec::new("kind", "TEXT", true, 0),
    ColumnSpec::new("status", "TEXT", true, 0),
    ColumnSpec::new("pause_requested", "INTEGER", true, 0),
    ColumnSpec::new("cancel_requested", "INTEGER", true, 0),
    ColumnSpec::new("attempts", "INTEGER", true, 0),
    ColumnSpec::new("cursor_json", "TEXT", true, 0),
    ColumnSpec::new("error", "TEXT", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
    ColumnSpec::new("started_at", "INTEGER", false, 0),
    ColumnSpec::new("finished_at", "INTEGER", false, 0),
];
const VISUAL_PAGE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", false, 0),
    ColumnSpec::new("page_index", "INTEGER", true, 0),
    ColumnSpec::new("object_key", "TEXT", true, 0),
    ColumnSpec::new("width", "INTEGER", true, 0),
    ColumnSpec::new("height", "INTEGER", true, 0),
    ColumnSpec::new("render_scale", "REAL", true, 0),
    ColumnSpec::new("renderer", "TEXT", true, 0),
    ColumnSpec::new("renderer_version", "TEXT", true, 0),
    ColumnSpec::new("document_revision", "INTEGER", true, 0),
    ColumnSpec::new("unit_revision", "INTEGER", true, 0),
    ColumnSpec::new("profile_id", "TEXT", true, 0),
    ColumnSpec::new("fidelity", "TEXT", true, 0),
    ColumnSpec::new("locator_json", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const VISUAL_PAGE_STAGING_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("job_id", "TEXT", true, 0),
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("source_id", "TEXT", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", false, 0),
    ColumnSpec::new("page_index", "INTEGER", true, 0),
    ColumnSpec::new("object_key", "TEXT", true, 0),
    ColumnSpec::new("width", "INTEGER", true, 0),
    ColumnSpec::new("height", "INTEGER", true, 0),
    ColumnSpec::new("render_scale", "REAL", true, 0),
    ColumnSpec::new("renderer", "TEXT", true, 0),
    ColumnSpec::new("renderer_version", "TEXT", true, 0),
    ColumnSpec::new("document_revision", "INTEGER", true, 0),
    ColumnSpec::new("unit_revision", "INTEGER", true, 0),
    ColumnSpec::new("profile_id", "TEXT", true, 0),
    ColumnSpec::new("fidelity", "TEXT", true, 0),
    ColumnSpec::new("locator_json", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const OFFICE_ENHANCEMENT_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("book_id", "TEXT", false, 1),
    ColumnSpec::new("enabled", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const CHAT_THREAD_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", false, 0),
    ColumnSpec::new("title", "TEXT", true, 0),
    ColumnSpec::new("scope_json", "TEXT", true, 0),
    ColumnSpec::new("window_kind", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const CHAT_MESSAGE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("thread_id", "TEXT", true, 0),
    ColumnSpec::new("parent_id", "TEXT", false, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("role", "TEXT", true, 0),
    ColumnSpec::new("content", "TEXT", true, 0),
    ColumnSpec::new("model", "TEXT", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const CHAT_CITATION_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("message_id", "TEXT", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", false, 0),
    ColumnSpec::new("search_chunk_id", "TEXT", false, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("quote", "TEXT", true, 0),
    ColumnSpec::new("document_revision", "INTEGER", false, 0),
    ColumnSpec::new("unit_revision", "INTEGER", false, 0),
    ColumnSpec::new("locator_json", "TEXT", false, 0),
    ColumnSpec::new("source_kind", "TEXT", true, 0),
    ColumnSpec::new("url", "TEXT", false, 0),
    ColumnSpec::new("source_title", "TEXT", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
];
const ANNOTATION_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", true, 0),
    ColumnSpec::new("document_revision", "INTEGER", true, 0),
    ColumnSpec::new("unit_revision", "INTEGER", true, 0),
    ColumnSpec::new("quote", "TEXT", true, 0),
    ColumnSpec::new("start_offset", "INTEGER", true, 0),
    ColumnSpec::new("end_offset", "INTEGER", true, 0),
    ColumnSpec::new("kind", "TEXT", true, 0),
    ColumnSpec::new("comment", "TEXT", false, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const TRANSLATION_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("id", "TEXT", false, 1),
    ColumnSpec::new("book_id", "TEXT", true, 0),
    ColumnSpec::new("content_unit_id", "TEXT", true, 0),
    ColumnSpec::new("block_id", "TEXT", true, 0),
    ColumnSpec::new("ordinal", "INTEGER", true, 0),
    ColumnSpec::new("document_revision", "INTEGER", true, 0),
    ColumnSpec::new("unit_revision", "INTEGER", true, 0),
    ColumnSpec::new("target_language", "TEXT", true, 0),
    ColumnSpec::new("source_language", "TEXT", false, 0),
    ColumnSpec::new("model", "TEXT", true, 0),
    ColumnSpec::new("source_text", "TEXT", true, 0),
    ColumnSpec::new("translated_text", "TEXT", true, 0),
    ColumnSpec::new("created_at", "INTEGER", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const SETTING_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("key", "TEXT", false, 1),
    ColumnSpec::new("value_json", "TEXT", true, 0),
    ColumnSpec::new("updated_at", "INTEGER", true, 0),
];
const SEARCH_FTS_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::new("search_chunk_id", "", false, 0),
    ColumnSpec::new("book_id", "", false, 0),
    ColumnSpec::new("heading", "", false, 0),
    ColumnSpec::new("body", "", false, 0),
];

const TABLE_SPECS: &[TableSpec] = &[
    TableSpec {
        name: "annotations",
        columns: ANNOTATION_COLUMNS,
    },
    TableSpec {
        name: "groups",
        columns: GROUP_COLUMNS,
    },
    TableSpec {
        name: "blobs",
        columns: BLOB_COLUMNS,
    },
    TableSpec {
        name: "books",
        columns: BOOK_COLUMNS,
    },
    TableSpec {
        name: "book_sources",
        columns: BOOK_SOURCE_COLUMNS,
    },
    TableSpec {
        name: "progress",
        columns: PROGRESS_COLUMNS,
    },
    TableSpec {
        name: "content_units",
        columns: CONTENT_UNIT_COLUMNS,
    },
    TableSpec {
        name: "toc_entries",
        columns: TOC_ENTRY_COLUMNS,
    },
    TableSpec {
        name: "assets",
        columns: ASSET_COLUMNS,
    },
    TableSpec {
        name: "asset_refs",
        columns: ASSET_REF_COLUMNS,
    },
    TableSpec {
        name: "search_chunks",
        columns: SEARCH_CHUNK_COLUMNS,
    },
    TableSpec {
        name: "embeddings",
        columns: EMBEDDING_COLUMNS,
    },
    TableSpec {
        name: "index_jobs",
        columns: INDEX_JOB_COLUMNS,
    },
    TableSpec {
        name: "visual_pages",
        columns: VISUAL_PAGE_COLUMNS,
    },
    TableSpec {
        name: "visual_page_staging",
        columns: VISUAL_PAGE_STAGING_COLUMNS,
    },
    TableSpec {
        name: "office_enhancements",
        columns: OFFICE_ENHANCEMENT_COLUMNS,
    },
    TableSpec {
        name: "chat_threads",
        columns: CHAT_THREAD_COLUMNS,
    },
    TableSpec {
        name: "chat_messages",
        columns: CHAT_MESSAGE_COLUMNS,
    },
    TableSpec {
        name: "chat_citations",
        columns: CHAT_CITATION_COLUMNS,
    },
    TableSpec {
        name: "translations",
        columns: TRANSLATION_COLUMNS,
    },
    TableSpec {
        name: "settings",
        columns: SETTING_COLUMNS,
    },
    TableSpec {
        name: "search_chunks_fts",
        columns: SEARCH_FTS_COLUMNS,
    },
];

// These specifications describe full-table indexes. The partial
// idx_annotations_one_mark_per_anchor index, including its predicate, is
// checked by schema_objects_match_canonical against the complete canonical DDL.
const INDEX_SPECS: &[IndexSpec] = &[
    IndexSpec {
        table: "annotations",
        name: "idx_annotations_book_unit",
        columns: &["book_id", "content_unit_id", "created_at"],
        unique: false,
    },
    IndexSpec {
        table: "groups",
        name: "idx_groups_parent",
        columns: &["parent_id"],
        unique: false,
    },
    IndexSpec {
        table: "blobs",
        name: "idx_blobs_hash",
        columns: &["hash"],
        unique: true,
    },
    IndexSpec {
        table: "books",
        name: "idx_books_group",
        columns: &["group_id"],
        unique: false,
    },
    IndexSpec {
        table: "books",
        name: "idx_books_source_object",
        columns: &["source_object_key"],
        unique: false,
    },
    IndexSpec {
        table: "books",
        name: "idx_books_cover_asset",
        columns: &["cover_asset_id"],
        unique: false,
    },
    IndexSpec {
        table: "book_sources",
        name: "idx_book_sources_revision",
        columns: &["book_id", "revision"],
        unique: true,
    },
    IndexSpec {
        table: "book_sources",
        name: "idx_book_sources_object",
        columns: &["object_key"],
        unique: false,
    },
    IndexSpec {
        table: "progress",
        name: "idx_progress_content_unit",
        columns: &["content_unit_id"],
        unique: false,
    },
    IndexSpec {
        table: "content_units",
        name: "idx_content_units_source_ordinal",
        columns: &["source_id", "ordinal"],
        unique: true,
    },
    IndexSpec {
        table: "content_units",
        name: "idx_content_units_book",
        columns: &["book_id"],
        unique: false,
    },
    IndexSpec {
        table: "content_units",
        name: "idx_content_units_parent",
        columns: &["parent_id"],
        unique: false,
    },
    IndexSpec {
        table: "content_units",
        name: "idx_content_units_href",
        columns: &["source_id", "href"],
        unique: false,
    },
    IndexSpec {
        table: "toc_entries",
        name: "idx_toc_entries_source_ordinal",
        columns: &["source_id", "ordinal"],
        unique: true,
    },
    IndexSpec {
        table: "toc_entries",
        name: "idx_toc_entries_book",
        columns: &["book_id"],
        unique: false,
    },
    IndexSpec {
        table: "toc_entries",
        name: "idx_toc_entries_parent",
        columns: &["parent_id"],
        unique: false,
    },
    IndexSpec {
        table: "toc_entries",
        name: "idx_toc_entries_content_unit",
        columns: &["content_unit_id"],
        unique: false,
    },
    IndexSpec {
        table: "assets",
        name: "idx_assets_source_href",
        columns: &["source_id", "href"],
        unique: true,
    },
    IndexSpec {
        table: "assets",
        name: "idx_assets_book_kind",
        columns: &["book_id", "kind"],
        unique: false,
    },
    IndexSpec {
        table: "assets",
        name: "idx_assets_object",
        columns: &["object_key"],
        unique: false,
    },
    IndexSpec {
        table: "asset_refs",
        name: "idx_asset_refs_identity",
        columns: &["content_unit_id", "asset_id", "relation", "ordinal"],
        unique: true,
    },
    IndexSpec {
        table: "asset_refs",
        name: "idx_asset_refs_asset",
        columns: &["asset_id"],
        unique: false,
    },
    IndexSpec {
        table: "search_chunks",
        name: "idx_search_chunks_unit_ordinal",
        columns: &["content_unit_id", "ordinal"],
        unique: true,
    },
    IndexSpec {
        table: "search_chunks",
        name: "idx_search_chunks_book",
        columns: &["book_id"],
        unique: false,
    },
    IndexSpec {
        table: "search_chunks",
        name: "idx_search_chunks_source",
        columns: &["source_id"],
        unique: false,
    },
    IndexSpec {
        table: "search_chunks",
        name: "idx_search_chunks_hash",
        columns: &["content_hash"],
        unique: false,
    },
    IndexSpec {
        table: "embeddings",
        name: "idx_embeddings_chunk_model",
        columns: &["search_chunk_id", "model"],
        unique: true,
    },
    IndexSpec {
        table: "index_jobs",
        name: "idx_index_jobs_book_status",
        columns: &["book_id", "status"],
        unique: false,
    },
    IndexSpec {
        table: "index_jobs",
        name: "idx_index_jobs_source",
        columns: &["source_id"],
        unique: false,
    },
    IndexSpec {
        table: "visual_pages",
        name: "idx_visual_pages_source_page",
        columns: &["source_id", "page_index"],
        unique: true,
    },
    IndexSpec {
        table: "visual_pages",
        name: "idx_visual_pages_book",
        columns: &["book_id"],
        unique: false,
    },
    IndexSpec {
        table: "visual_pages",
        name: "idx_visual_pages_object",
        columns: &["object_key"],
        unique: false,
    },
    IndexSpec {
        table: "visual_page_staging",
        name: "idx_visual_page_staging_job_page",
        columns: &["job_id", "page_index"],
        unique: true,
    },
    IndexSpec {
        table: "visual_page_staging",
        name: "idx_visual_page_staging_source",
        columns: &["source_id"],
        unique: false,
    },
    IndexSpec {
        table: "visual_page_staging",
        name: "idx_visual_page_staging_object",
        columns: &["object_key"],
        unique: false,
    },
    IndexSpec {
        table: "chat_threads",
        name: "idx_chat_threads_book_updated",
        columns: &["book_id", "updated_at"],
        unique: false,
    },
    IndexSpec {
        table: "chat_messages",
        name: "idx_chat_messages_thread_ordinal",
        columns: &["thread_id", "ordinal"],
        unique: true,
    },
    IndexSpec {
        table: "chat_messages",
        name: "idx_chat_messages_parent",
        columns: &["parent_id"],
        unique: false,
    },
    IndexSpec {
        table: "chat_citations",
        name: "idx_chat_citations_message_ordinal",
        columns: &["message_id", "ordinal"],
        unique: true,
    },
    IndexSpec {
        table: "chat_citations",
        name: "idx_chat_citations_content_unit",
        columns: &["content_unit_id"],
        unique: false,
    },
    IndexSpec {
        table: "chat_citations",
        name: "idx_chat_citations_search_chunk",
        columns: &["search_chunk_id"],
        unique: false,
    },
    IndexSpec {
        table: "translations",
        name: "idx_translations_scope",
        columns: &["book_id", "content_unit_id", "block_id", "target_language"],
        unique: true,
    },
    IndexSpec {
        table: "translations",
        name: "idx_translations_book",
        columns: &["book_id"],
        unique: false,
    },
    IndexSpec {
        table: "translations",
        name: "idx_translations_unit_language",
        columns: &["content_unit_id", "target_language"],
        unique: false,
    },
];

const TRIGGERS: &[(&str, &str)] = &[
    ("search_chunks_ai", "search_chunks"),
    ("search_chunks_ad", "search_chunks"),
    ("search_chunks_au", "search_chunks"),
];

pub(super) fn create(conn: &mut Connection) -> Result<()> {
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
fn create_schema(conn: &mut Connection) -> Result<()> {
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
                 renderer = 'moye-office-com-enhanced'
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
                 renderer = 'moye-office-com-enhanced'
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

pub(super) fn is_current(conn: &Connection) -> Result<bool> {
    if pragma_i64(conn, "application_id")? != APPLICATION_ID
        || pragma_i64(conn, "user_version")? != SCHEMA_VERSION
    {
        return Ok(false);
    }
    if !schema_objects_match_canonical(conn)? {
        return Ok(false);
    }
    for table in TABLE_SPECS {
        if !object_exists(conn, "table", table.name, table.name)?
            || !table_matches(conn, table.name, table.columns)?
        {
            return Ok(false);
        }
    }
    for index in INDEX_SPECS {
        if !index_matches(conn, index)? {
            return Ok(false);
        }
    }
    for (trigger, table) in TRIGGERS {
        if !object_exists(conn, "trigger", trigger, table)? {
            return Ok(false);
        }
    }
    if !is_fts5_search_table(conn)? {
        return Ok(false);
    }
    let quick_check = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .context("无法检查数据库完整性")?;
    if quick_check != "ok" || foreign_key_violation_exists(conn)? {
        return Ok(false);
    }
    Ok(!group_cycle_exists(conn)?
        && current_sources_are_valid(conn)?
        && document_relations_are_valid(conn)?
        && search_chunk_locators_are_valid(conn)?
        && search_index_is_synchronized(conn)?)
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaObjectDefinition {
    object_type: String,
    name: String,
    table_name: String,
    sql_tokens: Option<Vec<String>>,
}

/// Compares every application-owned schema object with a reference database
/// generated by `create_schema`. This catches constraint, foreign-key,
/// default-value and trigger-body drift that PRAGMA `table_info` cannot see,
/// as well as unexpected legacy objects. SQLite's reserved `sqlite_*` objects
/// are ignored; FTS5 shadow objects are not reserved and therefore participate
/// in the exact reference set automatically.
fn schema_objects_match_canonical(conn: &Connection) -> Result<bool> {
    let mut expected = Connection::open_in_memory().context("无法创建数据库结构校验基准")?;
    expected
        .pragma_update(None, "foreign_keys", "ON")
        .context("无法启用结构校验基准的外键约束")?;
    create_schema(&mut expected).context("无法创建数据库结构校验基准")?;
    Ok(schema_object_definitions(conn)? == schema_object_definitions(&expected)?)
}

fn schema_object_definitions(conn: &Connection) -> Result<Vec<SchemaObjectDefinition>> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, tbl_name, sql
             FROM sqlite_schema
             WHERE name NOT GLOB 'sqlite_*'
             ORDER BY type, name, tbl_name",
        )
        .context("无法准备数据库对象定义检查")?;
    stmt.query_map([], |row| {
        let sql = row.get::<_, Option<String>>(3)?;
        Ok(SchemaObjectDefinition {
            object_type: row.get::<_, String>(0)?.to_ascii_lowercase(),
            name: row.get::<_, String>(1)?.to_ascii_lowercase(),
            table_name: row.get::<_, String>(2)?.to_ascii_lowercase(),
            sql_tokens: sql.map(|sql| normalize_schema_sql(&sql)),
        })
    })
    .context("无法读取数据库对象定义")?
    .collect::<std::result::Result<Vec<_>, _>>()
    .context("无法读取数据库对象定义")
}

/// Tokenizes the stored DDL while preserving quoted values and token
/// boundaries. SQLite keeps the original spelling in `sqlite_schema`, so
/// keyword casing, comments and insignificant whitespace must not decide
/// compatibility. Keeping tokens separate also avoids treating `a TEXT` and
/// `at EXT` as the same definition after whitespace removal.
fn normalize_schema_sql(sql: &str) -> Vec<String> {
    let characters = sql.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut position = 0;

    while position < characters.len() {
        let character = characters[position];
        if character.is_whitespace() {
            position += 1;
            continue;
        }
        if character == '-' && characters.get(position + 1) == Some(&'-') {
            position += 2;
            while position < characters.len() && !matches!(characters[position], '\n' | '\r') {
                position += 1;
            }
            continue;
        }
        if character == '/' && characters.get(position + 1) == Some(&'*') {
            position += 2;
            while position + 1 < characters.len()
                && !(characters[position] == '*' && characters[position + 1] == '/')
            {
                position += 1;
            }
            position = (position + 2).min(characters.len());
            continue;
        }

        if matches!(character, '\'' | '"' | '`' | '[') {
            let quote_end = if character == '[' { ']' } else { character };
            let mut token = String::from(character);
            position += 1;
            while position < characters.len() {
                let quoted_character = characters[position];
                token.push(quoted_character);
                position += 1;
                if quoted_character == quote_end {
                    if quote_end != ']' && characters.get(position) == Some(&quote_end) {
                        token.push(characters[position]);
                        position += 1;
                    } else {
                        break;
                    }
                }
            }
            tokens.push(token);
            continue;
        }

        if is_schema_identifier_start(character) {
            let start = position;
            position += 1;
            while position < characters.len() && is_schema_identifier_continue(characters[position])
            {
                position += 1;
            }
            tokens.push(
                characters[start..position]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase(),
            );
            continue;
        }

        if character.is_ascii_digit()
            || (character == '.'
                && characters
                    .get(position + 1)
                    .is_some_and(char::is_ascii_digit))
        {
            let start = position;
            if character == '0'
                && characters
                    .get(position + 1)
                    .is_some_and(|next| matches!(next, 'x' | 'X'))
            {
                position += 2;
                while position < characters.len()
                    && (characters[position].is_ascii_hexdigit() || characters[position] == '_')
                {
                    position += 1;
                }
            } else {
                while position < characters.len()
                    && (characters[position].is_ascii_digit() || characters[position] == '_')
                {
                    position += 1;
                }
                if characters.get(position) == Some(&'.') {
                    position += 1;
                    while position < characters.len()
                        && (characters[position].is_ascii_digit() || characters[position] == '_')
                    {
                        position += 1;
                    }
                }
                if characters
                    .get(position)
                    .is_some_and(|next| matches!(next, 'e' | 'E'))
                {
                    position += 1;
                    if characters
                        .get(position)
                        .is_some_and(|next| matches!(next, '+' | '-'))
                    {
                        position += 1;
                    }
                    while position < characters.len()
                        && (characters[position].is_ascii_digit() || characters[position] == '_')
                    {
                        position += 1;
                    }
                }
            }
            tokens.push(
                characters[start..position]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase(),
            );
            continue;
        }

        let operator_width = if position + 2 < characters.len()
            && matches!(
                (
                    characters[position],
                    characters[position + 1],
                    characters[position + 2]
                ),
                ('-', '>', '>')
            ) {
            3
        } else if position + 1 < characters.len()
            && matches!(
                (characters[position], characters[position + 1]),
                ('|', '|')
                    | ('<', '<')
                    | ('>', '>')
                    | ('<', '=')
                    | ('>', '=')
                    | ('=', '=')
                    | ('!', '=')
                    | ('<', '>')
                    | ('-', '>')
            )
        {
            2
        } else {
            1
        };
        tokens.push(
            characters[position..position + operator_width]
                .iter()
                .collect(),
        );
        position += operator_width;
    }

    while tokens.last().is_some_and(|token| token == ";") {
        tokens.pop();
    }
    tokens
}

fn is_schema_identifier_start(character: char) -> bool {
    character == '_' || character == '$' || character.is_alphabetic()
}

fn is_schema_identifier_continue(character: char) -> bool {
    is_schema_identifier_start(character) || character.is_ascii_digit()
}

fn pragma_i64(conn: &Connection, name: &str) -> Result<i64> {
    conn.pragma_query_value(None, name, |row| row.get(0))
        .with_context(|| format!("无法读取数据库 {name}"))
}

fn object_exists(conn: &Connection, object_type: &str, name: &str, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2 AND tbl_name = ?3",
        (object_type, name, table),
        |_| Ok(()),
    )
    .optional()
    .context("无法检查数据库对象")
    .map(|value| value.is_some())
}

fn table_matches(conn: &Connection, table: &str, expected: &[ColumnSpec]) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("无法检查数据表 {table}"))?;
    let actual = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .with_context(|| format!("无法读取数据表 {table} 的字段"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法读取数据表 {table} 的字段"))?;
    Ok(actual.len() == expected.len()
        && actual.iter().zip(expected).all(
            |((name, data_type, not_null, primary_key), expected)| {
                name == expected.name
                    && data_type.eq_ignore_ascii_case(expected.data_type)
                    && *not_null == expected.not_null
                    && *primary_key == expected.primary_key
            },
        ))
}

fn index_matches(conn: &Connection, expected: &IndexSpec) -> Result<bool> {
    let mut index_list = conn
        .prepare(&format!("PRAGMA index_list({})", expected.table))
        .with_context(|| format!("无法检查数据表 {} 的索引", expected.table))?;
    let indexes = index_list
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .with_context(|| format!("无法读取数据表 {} 的索引", expected.table))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法读取数据表 {} 的索引", expected.table))?;
    let Some((_, unique, origin, partial)) =
        indexes.iter().find(|(name, ..)| name == expected.name)
    else {
        return Ok(false);
    };
    if *unique != expected.unique || origin != "c" || *partial {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_info({})", expected.name))
        .with_context(|| format!("无法检查数据库索引 {}", expected.name))?;
    let actual = stmt
        .query_map([], |row| row.get::<_, String>(2))
        .with_context(|| format!("无法读取数据库索引 {}", expected.name))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法读取数据库索引 {}", expected.name))?;
    Ok(actual
        .iter()
        .map(String::as_str)
        .eq(expected.columns.iter().copied()))
}

fn is_fts5_search_table(conn: &Connection) -> Result<bool> {
    let sql = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'search_chunks_fts'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("无法检查全文搜索表")?;
    Ok(sql.is_some_and(|sql| {
        let normalized = sql.to_ascii_lowercase();
        normalized.contains("virtual table search_chunks_fts using fts5")
            && normalized.contains("tokenize = 'trigram'")
    }))
}

fn foreign_key_violation_exists(conn: &Connection) -> Result<bool> {
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .context("无法准备数据库外键检查")?;
    let mut rows = stmt.query([]).context("无法检查数据库外键")?;
    Ok(rows.next().context("无法读取数据库外键检查结果")?.is_some())
}

fn group_cycle_exists(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "WITH RECURSIVE ancestors(origin, current) AS (
             SELECT id, parent_id FROM groups WHERE parent_id IS NOT NULL
             UNION
             SELECT ancestors.origin, groups.parent_id
             FROM ancestors JOIN groups ON groups.id = ancestors.current
             WHERE groups.parent_id IS NOT NULL
         )
         SELECT EXISTS(SELECT 1 FROM ancestors WHERE origin = current)",
        [],
        |row| row.get(0),
    )
    .context("无法检查分组循环")
}

fn current_sources_are_valid(conn: &Connection) -> Result<bool> {
    let invalid = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM books b
                 WHERE (
                     SELECT COUNT(*) FROM book_sources s
                     WHERE s.book_id = b.id
                       AND s.revision = b.revision
                       AND s.format = b.format
                       AND s.object_key = b.source_object_key
                 ) <> 1
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("无法检查图书当前来源")?;
    Ok(!invalid)
}

fn document_relations_are_valid(conn: &Connection) -> Result<bool> {
    let invalid = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM books b JOIN assets a ON a.id = b.cover_asset_id
                 WHERE a.book_id <> b.id OR a.kind <> 'cover'
                 UNION ALL
                 SELECT 1 FROM progress p JOIN content_units u ON u.id = p.content_unit_id
                 WHERE u.book_id <> p.book_id
                 UNION ALL
                 SELECT 1 FROM annotations a JOIN books b ON b.id = a.book_id
                 LEFT JOIN content_units u ON u.id = a.content_unit_id
                 WHERE a.document_revision > b.revision
                    OR (u.id IS NOT NULL AND u.book_id <> a.book_id)
                    OR (a.document_revision = b.revision AND (
                        u.id IS NULL OR a.unit_revision <> u.revision
                    ))
                 UNION ALL
                 SELECT 1 FROM content_units u
                 LEFT JOIN book_sources s ON s.id = u.source_id
                 WHERE s.id IS NULL OR s.book_id <> u.book_id
                 UNION ALL
                 SELECT 1 FROM toc_entries t
                 LEFT JOIN book_sources s ON s.id = t.source_id
                 WHERE s.id IS NULL OR s.book_id <> t.book_id
                 UNION ALL
                 SELECT 1 FROM assets a
                 LEFT JOIN book_sources s ON s.id = a.source_id
                 WHERE s.id IS NULL OR s.book_id <> a.book_id
                 UNION ALL
                 SELECT 1 FROM search_chunks c
                 LEFT JOIN book_sources s ON s.id = c.source_id
                 WHERE s.id IS NULL OR s.book_id <> c.book_id
                 UNION ALL
                 SELECT 1 FROM index_jobs j
                 LEFT JOIN book_sources s ON s.id = j.source_id
                 WHERE j.source_id IS NOT NULL
                   AND (s.id IS NULL OR s.book_id <> j.book_id)
                 UNION ALL
                 SELECT 1 FROM content_units child JOIN content_units parent ON parent.id = child.parent_id
                 WHERE parent.book_id <> child.book_id OR parent.source_id <> child.source_id
                 UNION ALL
                 SELECT 1 FROM toc_entries child JOIN toc_entries parent ON parent.id = child.parent_id
                 WHERE parent.book_id <> child.book_id OR parent.source_id <> child.source_id
                 UNION ALL
                 SELECT 1 FROM toc_entries t JOIN content_units u ON u.id = t.content_unit_id
                 WHERE u.book_id <> t.book_id OR u.source_id <> t.source_id
                 UNION ALL
                 SELECT 1 FROM asset_refs r
                 JOIN content_units u ON u.id = r.content_unit_id
                 JOIN assets a ON a.id = r.asset_id
                 WHERE u.book_id <> a.book_id OR u.source_id <> a.source_id
                 UNION ALL
                 SELECT 1 FROM search_chunks c JOIN content_units u ON u.id = c.content_unit_id
                 WHERE u.book_id <> c.book_id OR u.source_id <> c.source_id
                 UNION ALL
                 SELECT 1 FROM visual_pages p
                 LEFT JOIN content_units u ON u.id = p.content_unit_id
                 LEFT JOIN book_sources s ON s.id = p.source_id
                 WHERE s.id IS NULL OR s.book_id <> p.book_id
                    OR p.document_revision <> s.revision
                    OR (p.content_unit_id IS NOT NULL AND (
                        u.id IS NULL
                        OR u.book_id <> p.book_id OR u.source_id <> p.source_id
                        OR p.unit_revision <> u.revision
                    ))
                    OR (p.content_unit_id IS NULL AND (
                        p.renderer <> 'moye-office-com-enhanced'
                        OR p.fidelity <> 'office_enhanced'
                        OR s.format NOT IN ('doc', 'docx', 'xlsx')
                        OR p.unit_revision <> 0
                        OR CASE WHEN json_valid(p.locator_json) THEN
                            COALESCE(json_extract(p.locator_json, '$.book_id'), '') <> p.book_id
                            OR COALESCE(json_extract(p.locator_json, '$.unit_id'), '') = ''
                            OR EXISTS(
                                SELECT 1 FROM content_units preview_unit
                                WHERE preview_unit.id = json_extract(p.locator_json, '$.unit_id')
                            )
                            OR COALESCE(json_extract(p.locator_json, '$.source.type'), '')
                                <> 'office_rendered_page'
                            OR COALESCE(json_extract(p.locator_json, '$.source.page'), 0)
                                <> p.page_index + 1
                            OR COALESCE(json_type(p.locator_json, '$.block_id'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.text_range'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.region'), 'null') <> 'null'
                        ELSE 1 END
                    ))
                 UNION ALL
                 SELECT 1 FROM visual_page_staging p
                 LEFT JOIN index_jobs j ON j.id = p.job_id
                 LEFT JOIN content_units u ON u.id = p.content_unit_id
                 LEFT JOIN book_sources s ON s.id = p.source_id
                 WHERE j.id IS NULL OR j.source_id IS NULL OR s.id IS NULL
                    OR s.book_id <> p.book_id
                    OR j.kind <> 'visual_render'
                    OR j.book_id <> p.book_id OR j.source_id <> p.source_id
                    OR p.document_revision <> s.revision
                    OR (p.content_unit_id IS NOT NULL AND (
                        u.id IS NULL
                        OR u.book_id <> p.book_id OR u.source_id <> p.source_id
                        OR p.unit_revision <> u.revision
                    ))
                    OR (p.content_unit_id IS NULL AND (
                        p.renderer <> 'moye-office-com-enhanced'
                        OR p.fidelity <> 'office_enhanced'
                        OR s.format NOT IN ('doc', 'docx', 'xlsx')
                        OR p.unit_revision <> 0
                        OR CASE WHEN json_valid(p.locator_json) THEN
                            COALESCE(json_extract(p.locator_json, '$.book_id'), '') <> p.book_id
                            OR COALESCE(json_extract(p.locator_json, '$.unit_id'), '') = ''
                            OR EXISTS(
                                SELECT 1 FROM content_units preview_unit
                                WHERE preview_unit.id = json_extract(p.locator_json, '$.unit_id')
                            )
                            OR COALESCE(json_extract(p.locator_json, '$.source.type'), '')
                                <> 'office_rendered_page'
                            OR COALESCE(json_extract(p.locator_json, '$.source.page'), 0)
                                <> p.page_index + 1
                            OR COALESCE(json_type(p.locator_json, '$.block_id'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.text_range'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.region'), 'null') <> 'null'
                        ELSE 1 END
                    ))
                 UNION ALL
                 SELECT 1 FROM translations t
                 LEFT JOIN books b ON b.id = t.book_id
                 LEFT JOIN content_units u ON u.id = t.content_unit_id
                 WHERE b.id IS NULL OR u.id IS NULL
                    OR u.book_id <> t.book_id
                    OR t.document_revision > b.revision
                    OR (t.document_revision = b.revision AND t.unit_revision <> u.revision)
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("无法检查统一文档关系")?;
    Ok(!invalid)
}

fn search_chunk_locators_are_valid(conn: &Connection) -> Result<bool> {
    let mut statement = conn
        .prepare("SELECT book_id, content_unit_id, locator_json FROM search_chunks")
        .context("无法准备搜索分块定位检查")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("无法检查搜索分块定位")?;
    for row in rows {
        let (book_id, content_unit_id, locator_json) = row.context("无法读取搜索分块定位")?;
        let Ok(locator) = serde_json::from_str::<DocumentLocator>(&locator_json) else {
            return Ok(false);
        };
        if locator.validate().is_err()
            || locator.book_id != book_id
            || locator.unit_id != content_unit_id
            || matches!(
                locator.source.as_ref(),
                Some(SourceLocator::OfficeRenderedPage { .. })
            )
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn search_index_is_synchronized(conn: &Connection) -> Result<bool> {
    // FTS5 UNINDEXED columns cannot make a per-chunk lookup efficient. Compare
    // the two complete row sets instead; the count check preserves duplicate
    // detection because EXCEPT intentionally removes duplicates.
    conn.query_row(
        "SELECT
                 (SELECT COUNT(*) FROM search_chunks)
                     = (SELECT COUNT(*) FROM search_chunks_fts)
                 AND NOT EXISTS(
                     SELECT id, book_id, heading, body FROM search_chunks
                     EXCEPT
                     SELECT search_chunk_id, book_id, heading, body FROM search_chunks_fts
                 )
                 AND NOT EXISTS(
                     SELECT search_chunk_id, book_id, heading, body FROM search_chunks_fts
                     EXCEPT
                     SELECT id, book_id, heading, body FROM search_chunks
                 )",
        [],
        |row| row.get::<_, bool>(0),
    )
    .context("无法检查全文索引同步状态")
}

#[cfg(test)]
mod tests {
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
            &DocumentLocator::unit("book", "unit")
                .with_source(SourceLocator::office_rendered_page(1)),
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
                         1, 1, 1.0, 'moye-office-com-enhanced', '1', 1, 0, 'profile',
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
}
