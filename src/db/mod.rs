pub(crate) mod annotations;
pub(crate) mod asset_refs;
pub(crate) mod assets;
pub(crate) mod blobs;
pub(crate) mod book_search;
pub(crate) mod book_sources;
pub(crate) mod books;
pub(crate) mod chat_citations;
pub(crate) mod chat_messages;
pub(crate) mod chat_threads;
pub(crate) mod connection;
pub(crate) mod content_units;
pub(crate) mod embeddings;
pub(crate) mod groups;
pub(crate) mod index_jobs;
pub(crate) mod office_enhancements;
pub(crate) mod progress;
pub(crate) mod schema;
pub(crate) mod search_chunks;
pub(crate) mod settings;
pub(crate) mod toc_entries;
pub(crate) mod transactions;
pub(crate) mod visual_page_staging;
pub(crate) mod visual_pages;

#[cfg(test)]
pub(crate) use connection::open_or_recreate;
pub(crate) use connection::{
    DATABASE_FILE, DatabaseOpenState, discard_opened_database, open_conn,
    open_or_recreate_with_state,
};
