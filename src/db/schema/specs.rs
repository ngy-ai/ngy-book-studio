//! The table, index and trigger specs the schema is created from.

use super::*;

pub(crate) const TABLE_SPECS: &[TableSpec] = &[
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
pub(crate) const INDEX_SPECS: &[IndexSpec] = &[
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

pub(crate) const TRIGGERS: &[(&str, &str)] = &[
    ("search_chunks_ai", "search_chunks"),
    ("search_chunks_ad", "search_chunks"),
    ("search_chunks_au", "search_chunks"),
];
