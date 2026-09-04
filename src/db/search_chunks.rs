use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchChunk {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) content_unit_id: String,
    pub(crate) ordinal: usize,
    pub(crate) heading: String,
    pub(crate) body: String,
    pub(crate) token_count: usize,
    pub(crate) content_hash: String,
    pub(crate) locator_json: String,
    pub(crate) created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchChunkDocument {
    pub(crate) chunk: SearchChunk,
    pub(crate) book_title: String,
    pub(crate) unit_title: String,
    pub(crate) document_revision: u64,
    pub(crate) unit_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KeywordMatch {
    pub(crate) document: SearchChunkDocument,
    /// Raw FTS5 BM25 value. Lower values rank first.
    pub(crate) rank: f64,
}

const SELECT: &str = "SELECT id, book_id, source_id, content_unit_id, ordinal, heading,
    body, token_count, content_hash, locator_json, created_at FROM search_chunks";

pub(crate) fn get(conn: &Connection, chunk_id: &str) -> Result<Option<SearchChunk>> {
    conn.query_row(
        &format!("{SELECT} WHERE id = ?1"),
        [chunk_id],
        chunk_from_row,
    )
    .optional()
    .context("无法读取搜索分块")
}

pub(crate) fn list_for_source(conn: &Connection, source_id: &str) -> Result<Vec<SearchChunk>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE source_id = ?1 ORDER BY content_unit_id, ordinal"
        ))
        .context("无法准备搜索分块查询")?;
    let rows = stmt
        .query_map([source_id], chunk_from_row)
        .context("无法读取搜索分块")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取搜索分块记录")
}

pub(crate) fn list_batch_for_source(
    conn: &Connection,
    source_id: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<SearchChunk>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE source_id = ?1
             ORDER BY content_unit_id, ordinal, id LIMIT ?2 OFFSET ?3"
        ))
        .context("无法准备搜索分块批次查询")?;
    let rows = stmt
        .query_map(
            params![source_id, limit as i64, offset as i64],
            chunk_from_row,
        )
        .context("无法读取搜索分块批次")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取搜索分块批次记录")
}

pub(crate) fn insert(conn: &Connection, chunk: &SearchChunk) -> Result<usize> {
    conn.execute(
        "INSERT INTO search_chunks
         (id, book_id, source_id, content_unit_id, ordinal, heading, body,
          token_count, content_hash, locator_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            chunk.id,
            chunk.book_id,
            chunk.source_id,
            chunk.content_unit_id,
            chunk.ordinal as i64,
            chunk.heading,
            chunk.body,
            chunk.token_count as i64,
            chunk.content_hash,
            chunk.locator_json,
            chunk.created_at as i64,
        ],
    )
    .context("无法写入搜索分块")
}

/// Inserts or replaces one stable derived chunk. Canonical chunks continue to
/// use [`insert`], so accidentally reusing their IDs remains a hard error.
pub(crate) fn upsert_derived(conn: &Connection, chunk: &SearchChunk) -> Result<usize> {
    conn.execute(
        "INSERT INTO search_chunks
         (id, book_id, source_id, content_unit_id, ordinal, heading, body,
          token_count, content_hash, locator_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
             book_id = excluded.book_id,
             source_id = excluded.source_id,
             content_unit_id = excluded.content_unit_id,
             ordinal = excluded.ordinal,
             heading = excluded.heading,
             body = excluded.body,
             token_count = excluded.token_count,
             content_hash = excluded.content_hash,
             locator_json = excluded.locator_json,
             created_at = excluded.created_at",
        params![
            chunk.id,
            chunk.book_id,
            chunk.source_id,
            chunk.content_unit_id,
            chunk.ordinal as i64,
            chunk.heading,
            chunk.body,
            chunk.token_count as i64,
            chunk.content_hash,
            chunk.locator_json,
            chunk.created_at as i64,
        ],
    )
    .context("无法写入视觉搜索分块")
}

pub(crate) fn update_text(
    conn: &Connection,
    chunk_id: &str,
    heading: &str,
    body: &str,
    token_count: usize,
    content_hash: &str,
    locator_json: &str,
) -> Result<usize> {
    conn.execute(
        "UPDATE search_chunks SET heading = ?2, body = ?3, token_count = ?4,
         content_hash = ?5, locator_json = ?6 WHERE id = ?1",
        params![
            chunk_id,
            heading,
            body,
            token_count as i64,
            content_hash,
            locator_json,
        ],
    )
    .context("无法更新搜索分块")
}

pub(crate) fn delete_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM search_chunks WHERE source_id = ?1",
        [source_id],
    )
    .context("无法删除来源搜索分块")
}

pub(crate) fn delete_stale_visual_derived(
    conn: &Connection,
    source_id: &str,
    first_stale_ordinal: usize,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM search_chunks
         WHERE source_id = ?1 AND id LIKE 'vision-chunk-%' ESCAPE '\\'
           AND ordinal >= ?2",
        params![source_id, first_stale_ordinal as i64],
    )
    .context("无法删除过期视觉搜索分块")
}

/// Searches FTS5 within an explicit book allow-list. The allow-list is part of
/// the SQL predicate, and an empty list deliberately produces no results.
pub(crate) fn search_fts_scoped(
    conn: &Connection,
    query: &str,
    allowed_book_ids: &[String],
    limit: usize,
) -> Result<Vec<KeywordMatch>> {
    if allowed_book_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let Some(fts_query) = fts_query(query) else {
        return Ok(Vec::new());
    };

    let mut values = vec![Value::Text(fts_query)];
    let scope_parameters = allowed_book_ids
        .iter()
        .map(|book_id| {
            values.push(Value::Text(book_id.clone()));
            format!("?{}", values.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    values.push(Value::Integer(limit as i64));
    let limit_parameter = values.len();
    let sql = format!(
        "SELECT c.id, c.book_id, c.source_id, c.content_unit_id, c.ordinal,
                c.heading, c.body, c.token_count, c.content_hash,
                c.locator_json, c.created_at, b.title,
                COALESCE(u.title, c.heading), b.revision, u.revision,
                bm25(search_chunks_fts, 0.0, 0.0, 6.0, 1.0) AS rank
         FROM search_chunks_fts
         JOIN search_chunks c ON c.id = search_chunks_fts.search_chunk_id
         JOIN books b ON b.id = c.book_id
         JOIN book_sources s
           ON s.id = c.source_id AND s.book_id = c.book_id AND s.revision = b.revision
         JOIN content_units u
           ON u.id = c.content_unit_id
          AND u.book_id = c.book_id
          AND u.source_id = c.source_id
         WHERE search_chunks_fts MATCH ?1
           AND c.book_id IN ({scope_parameters})
           AND CASE WHEN json_valid(c.locator_json)
                    THEN COALESCE(json_extract(c.locator_json, '$.source.type'), '')
                         <> 'office_rendered_page'
                    ELSE 0
               END
         ORDER BY rank ASC, c.book_id ASC, c.content_unit_id ASC, c.ordinal ASC
         LIMIT ?{limit_parameter}"
    );
    let mut statement = conn.prepare(&sql).context("无法准备限定范围的全文搜索")?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), keyword_match_from_row)
        .context("限定范围的全文搜索失败")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取限定范围的全文搜索结果")
}

/// Loads chunk metadata for vector hits while enforcing both the hit IDs and
/// the caller's book allow-list in SQLite.
pub(crate) fn get_documents_scoped(
    conn: &Connection,
    chunk_ids: &[String],
    allowed_book_ids: &[String],
) -> Result<Vec<SearchChunkDocument>> {
    if chunk_ids.is_empty() || allowed_book_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut values = Vec::with_capacity(chunk_ids.len() + allowed_book_ids.len());
    let chunk_parameters = chunk_ids
        .iter()
        .map(|chunk_id| {
            values.push(Value::Text(chunk_id.clone()));
            format!("?{}", values.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let scope_parameters = allowed_book_ids
        .iter()
        .map(|book_id| {
            values.push(Value::Text(book_id.clone()));
            format!("?{}", values.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT c.id, c.book_id, c.source_id, c.content_unit_id, c.ordinal,
                c.heading, c.body, c.token_count, c.content_hash,
                c.locator_json, c.created_at, b.title,
                COALESCE(u.title, c.heading), b.revision, u.revision
         FROM search_chunks c
         JOIN books b ON b.id = c.book_id
         JOIN book_sources s
           ON s.id = c.source_id AND s.book_id = c.book_id AND s.revision = b.revision
         JOIN content_units u
           ON u.id = c.content_unit_id
          AND u.book_id = c.book_id
          AND u.source_id = c.source_id
         WHERE c.id IN ({chunk_parameters})
           AND c.book_id IN ({scope_parameters})
           AND CASE WHEN json_valid(c.locator_json)
                    THEN COALESCE(json_extract(c.locator_json, '$.source.type'), '')
                         <> 'office_rendered_page'
                    ELSE 0
               END"
    );
    let mut statement = conn
        .prepare(&sql)
        .context("无法准备限定范围的搜索分块查询")?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), document_from_row)
        .context("无法读取限定范围的搜索分块")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取限定范围的搜索分块记录")
}

#[cfg(test)]
pub(crate) fn count_for_book(conn: &Connection, book_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM search_chunks WHERE book_id = ?1",
        [book_id],
        |row| row.get(0),
    )
    .context("无法统计搜索分块")
}

pub(crate) fn count_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM search_chunks WHERE source_id = ?1",
            [source_id],
            |row| row.get::<_, i64>(0),
        )
        .context("无法统计来源搜索分块")?;
    usize::try_from(count).context("来源搜索分块数量超出支持范围")
}

fn chunk_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchChunk> {
    Ok(SearchChunk {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        content_unit_id: row.get(3)?,
        ordinal: row.get::<_, i64>(4)? as usize,
        heading: row.get(5)?,
        body: row.get(6)?,
        token_count: row.get::<_, i64>(7)? as usize,
        content_hash: row.get(8)?,
        locator_json: row.get(9)?,
        created_at: row.get::<_, i64>(10)? as u64,
    })
}

fn document_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchChunkDocument> {
    Ok(SearchChunkDocument {
        chunk: chunk_from_row(row)?,
        book_title: row.get(11)?,
        unit_title: row.get(12)?,
        document_revision: row.get::<_, i64>(13)? as u64,
        unit_revision: row.get::<_, i64>(14)? as u64,
    })
}

fn keyword_match_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KeywordMatch> {
    Ok(KeywordMatch {
        document: document_from_row(row)?,
        rank: row.get(15)?,
    })
}

fn fts_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}
