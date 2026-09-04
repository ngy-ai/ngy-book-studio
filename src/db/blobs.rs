use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

/// Metadata for an object owned by `LocalBlobStore`. Object bytes never live in
/// SQLite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlobRecord {
    pub(crate) object_key: String,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) hash: String,
    pub(crate) created_at: u64,
}

pub(crate) fn get(conn: &Connection, object_key: &str) -> Result<Option<BlobRecord>> {
    conn.query_row(
        "SELECT object_key, media_type, byte_len, hash, created_at FROM blobs WHERE object_key = ?1",
        [object_key],
        blob_from_row,
    )
    .optional()
    .context("无法读取对象元数据")
}

pub(crate) fn get_by_hash(conn: &Connection, hash: &str) -> Result<Option<BlobRecord>> {
    conn.query_row(
        "SELECT object_key, media_type, byte_len, hash, created_at FROM blobs WHERE hash = ?1",
        [hash],
        blob_from_row,
    )
    .optional()
    .context("无法按摘要读取对象元数据")
}

pub(crate) fn list(conn: &Connection) -> Result<Vec<BlobRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT object_key, media_type, byte_len, hash, created_at
             FROM blobs ORDER BY object_key",
        )
        .context("无法准备对象元数据列表查询")?;
    let rows = stmt
        .query_map([], blob_from_row)
        .context("无法读取对象元数据列表")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取对象元数据列表")
}

pub(crate) fn insert(conn: &Connection, blob: &BlobRecord) -> Result<usize> {
    conn.execute(
        "INSERT INTO blobs (object_key, media_type, byte_len, hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            blob.object_key,
            blob.media_type,
            blob.byte_len as i64,
            blob.hash,
            blob.created_at as i64,
        ],
    )
    .context("无法写入对象元数据")
}

/// Reuses an existing content-addressed object only when all immutable metadata
/// agrees with the caller.
pub(crate) fn ensure_present(conn: &Connection, blob: &BlobRecord) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO blobs (object_key, media_type, byte_len, hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            blob.object_key,
            blob.media_type,
            blob.byte_len as i64,
            blob.hash,
            blob.created_at as i64,
        ],
    )
    .context("无法登记对象元数据")?;
    let stored = get(conn, &blob.object_key)?.context("对象元数据写入后丢失")?;
    ensure!(
        stored.media_type == blob.media_type
            && stored.byte_len == blob.byte_len
            && stored.hash == blob.hash,
        "对象键已对应不同内容：{}",
        blob.object_key
    );
    Ok(())
}

pub(crate) fn delete(conn: &Connection, object_key: &str) -> Result<usize> {
    conn.execute("DELETE FROM blobs WHERE object_key = ?1", [object_key])
        .context("无法删除对象元数据")
}

/// Re-checks whether a registered object currently has no durable reference.
/// Reclaimers call this immediately before deleting bytes; a candidate list
/// returned by an earlier transaction is intentionally not treated as current
/// truth.
pub(crate) fn is_unreferenced(conn: &Connection, object_key: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM blobs b
             WHERE b.object_key = ?1
               AND NOT EXISTS (SELECT 1 FROM books WHERE source_object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM book_sources WHERE object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM assets WHERE object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM visual_pages WHERE object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM visual_page_staging WHERE object_key = b.object_key)
         )",
        [object_key],
        |row| row.get::<_, bool>(0),
    )
    .context("无法确认对象是否仍未被引用")
}

/// Removes metadata only if the object is still unreferenced at execution
/// time. Publication/GC serialization prevents a conforming publisher from
/// racing the preceding byte deletion; this predicate remains a final defense
/// against unrelated database writers.
pub(crate) fn delete_if_unreferenced(conn: &Connection, object_key: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM blobs
         WHERE object_key = ?1
           AND NOT EXISTS (SELECT 1 FROM books WHERE source_object_key = blobs.object_key)
           AND NOT EXISTS (SELECT 1 FROM book_sources WHERE object_key = blobs.object_key)
           AND NOT EXISTS (SELECT 1 FROM assets WHERE object_key = blobs.object_key)
           AND NOT EXISTS (SELECT 1 FROM visual_pages WHERE object_key = blobs.object_key)
           AND NOT EXISTS (SELECT 1 FROM visual_page_staging WHERE object_key = blobs.object_key)",
        [object_key],
    )
    .context("无法安全删除未引用对象元数据")
}

pub(crate) fn list_unreferenced(conn: &Connection) -> Result<Vec<BlobRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT object_key, media_type, byte_len, hash, created_at
             FROM blobs b
             WHERE NOT EXISTS (SELECT 1 FROM books WHERE source_object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM book_sources WHERE object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM assets WHERE object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM visual_pages WHERE object_key = b.object_key)
               AND NOT EXISTS (SELECT 1 FROM visual_page_staging WHERE object_key = b.object_key)
             ORDER BY object_key",
        )
        .context("无法准备未引用对象查询")?;
    let rows = stmt
        .query_map([], blob_from_row)
        .context("无法读取未引用对象")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取未引用对象记录")
}

fn blob_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BlobRecord> {
    Ok(BlobRecord {
        object_key: row.get(0)?,
        media_type: row.get(1)?,
        byte_len: row.get::<_, i64>(2)? as u64,
        hash: row.get(3)?,
        created_at: row.get::<_, i64>(4)? as u64,
    })
}
