use anyhow::{Context as _, Result};
use rusqlite::{Connection, params_from_iter, types::Value};

const SEARCHABLE_LOCATOR_FILTER: &str = "CASE WHEN json_valid(c.locator_json)
    THEN COALESCE(json_extract(c.locator_json, '$.source.type'), '')
         <> 'office_rendered_page'
    ELSE 0
END";

/// One full-text search match. A `None` location identifies a book-title or
/// author match; content matches carry a content-unit ordinal and href.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub book_id: String,
    pub book_title: String,
    pub author: String,
    pub spine_index: Option<usize>,
    pub chapter_title: Option<String>,
    pub href: Option<String>,
    pub snippet: String,
    /// Lower values are more relevant. Metadata compatibility matches use
    /// `-1.0`; content matches use SQLite FTS5's BM25 rank or `0.0` for short
    /// literal queries.
    pub relevance: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchChapter {
    pub(crate) spine_index: usize,
    pub(crate) href: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

pub(crate) fn search(
    conn: &Connection,
    book_id: Option<&str>,
    terms: &[String],
    limit: i64,
) -> Result<Vec<SearchHit>> {
    if terms.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }

    let mut hits = if book_id.is_none() {
        search_metadata(conn, terms, limit)?
    } else {
        Vec::new()
    };
    let remaining = limit.saturating_sub(hits.len() as i64);
    if remaining > 0 {
        if terms.iter().all(|term| term.chars().count() >= 3) {
            hits.extend(search_fts(conn, book_id, terms, remaining)?);
        } else {
            hits.extend(search_short_query(conn, book_id, terms, remaining)?);
        }
    }
    Ok(hits)
}

fn search_metadata(conn: &Connection, terms: &[String], limit: i64) -> Result<Vec<SearchHit>> {
    let values = terms
        .iter()
        .cloned()
        .map(Value::Text)
        .chain(std::iter::once(Value::Integer(limit)))
        .collect::<Vec<_>>();
    let filters = (1..=terms.len())
        .map(|parameter| {
            format!(
                "(instr(lower(title), lower(?{parameter})) > 0 OR \
                 instr(lower(author), lower(?{parameter})) > 0)"
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "SELECT id, title, author FROM books WHERE {filters}
         ORDER BY added_at DESC, id LIMIT ?{}",
        values.len()
    );
    let mut stmt = conn.prepare(&sql).context("无法准备图书元数据搜索")?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            let title = row.get::<_, String>(1)?;
            let author = row.get::<_, String>(2)?;
            Ok(SearchHit {
                book_id: row.get(0)?,
                book_title: title.clone(),
                author: author.clone(),
                spine_index: None,
                chapter_title: None,
                href: None,
                snippet: format!("{title} · {author}"),
                relevance: -1.0,
            })
        })
        .context("图书元数据搜索失败")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取图书元数据搜索结果")
}

fn search_fts(
    conn: &Connection,
    book_id: Option<&str>,
    terms: &[String],
    limit: i64,
) -> Result<Vec<SearchHit>> {
    let fts_query = terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let mut values = vec![Value::Text(fts_query)];
    let mut filters = vec![
        "search_chunks_fts MATCH ?1".to_string(),
        SEARCHABLE_LOCATOR_FILTER.to_string(),
    ];
    if let Some(book_id) = book_id {
        values.push(Value::Text(book_id.to_string()));
        filters.push(format!("c.book_id = ?{}", values.len()));
    }
    values.push(Value::Integer(limit));
    let sql = format!(
        "SELECT c.book_id, b.title, b.author, u.ordinal, u.title, u.href,
                snippet(search_chunks_fts, 3, '', '', '…', 24),
                bm25(search_chunks_fts, 0.0, 0.0, 6.0, 1.0) AS relevance
         FROM search_chunks_fts
         JOIN search_chunks c ON c.id = search_chunks_fts.search_chunk_id
         JOIN books b ON b.id = c.book_id
         JOIN content_units u ON u.id = c.content_unit_id
         WHERE {}
         ORDER BY relevance ASC, b.added_at DESC, c.book_id, u.ordinal, c.ordinal
         LIMIT ?{}",
        filters.join(" AND "),
        values.len()
    );
    query_content_hits(conn, &sql, &values)
}

fn search_short_query(
    conn: &Connection,
    book_id: Option<&str>,
    terms: &[String],
    limit: i64,
) -> Result<Vec<SearchHit>> {
    let mut values = Vec::with_capacity(terms.len() + 2);
    let mut filters = Vec::with_capacity(terms.len() + 2);
    filters.push(SEARCHABLE_LOCATOR_FILTER.to_string());
    for term in terms {
        values.push(Value::Text(term.clone()));
        let parameter = values.len();
        filters.push(format!(
            "(instr(lower(c.heading), lower(?{parameter})) > 0 OR \
              instr(lower(c.body), lower(?{parameter})) > 0)"
        ));
    }
    if let Some(book_id) = book_id {
        values.push(Value::Text(book_id.to_string()));
        filters.push(format!("c.book_id = ?{}", values.len()));
    }
    values.push(Value::Integer(limit));
    let first_term = 1;
    let sql = format!(
        "SELECT c.book_id, b.title, b.author, u.ordinal, u.title, u.href,
                CASE WHEN instr(lower(c.body), lower(?{first_term})) > 0
                     THEN substr(c.body, max(1, instr(lower(c.body), lower(?{first_term})) - 80), 240)
                     ELSE substr(c.body, 1, 240) END,
                0.0 AS relevance
         FROM search_chunks c
         JOIN books b ON b.id = c.book_id
         JOIN content_units u ON u.id = c.content_unit_id
         WHERE {}
         ORDER BY b.added_at DESC, c.book_id, u.ordinal, c.ordinal
         LIMIT ?{}",
        filters.join(" AND "),
        values.len()
    );
    query_content_hits(conn, &sql, &values)
}

fn query_content_hits(conn: &Connection, sql: &str, values: &[Value]) -> Result<Vec<SearchHit>> {
    let mut stmt = conn.prepare(sql).context("无法准备全文搜索")?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            let chapter_title = row.get::<_, Option<String>>(4)?;
            let excerpt = row.get::<_, String>(6)?;
            let book_title = row.get::<_, String>(1)?;
            let author = row.get::<_, String>(2)?;
            let snippet = collapse_search_whitespace(&excerpt);
            let snippet = if snippet.is_empty() {
                chapter_title
                    .clone()
                    .unwrap_or_else(|| format!("{book_title} · {author}"))
            } else {
                snippet
            };
            Ok(SearchHit {
                book_id: row.get(0)?,
                book_title,
                author,
                spine_index: Some(row.get::<_, i64>(3)? as usize),
                chapter_title,
                href: row.get(5)?,
                snippet,
                relevance: row.get(7)?,
            })
        })
        .context("全文搜索失败")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取全文搜索结果")
}

fn collapse_search_whitespace(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() || character.is_control() {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(' ');
            }
            output.push(character);
            pending_space = false;
        }
    }
    output
}
