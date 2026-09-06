use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatCitation {
    pub(crate) id: String,
    pub(crate) message_id: String,
    pub(crate) content_unit_id: Option<String>,
    pub(crate) search_chunk_id: Option<String>,
    pub(crate) ordinal: usize,
    pub(crate) quote: String,
    pub(crate) document_revision: u64,
    pub(crate) unit_revision: u64,
    pub(crate) locator_json: Option<String>,
    pub(crate) source_kind: String,
    pub(crate) url: Option<String>,
    pub(crate) source_title: Option<String>,
    pub(crate) created_at: u64,
}

pub(crate) fn get(conn: &Connection, citation_id: &str) -> Result<Option<ChatCitation>> {
    conn.query_row(
        "SELECT id, message_id, content_unit_id, search_chunk_id, ordinal, quote,
         document_revision, unit_revision, locator_json, source_kind, url, source_title,
         created_at
         FROM chat_citations WHERE id = ?1",
        [citation_id],
        citation_from_row,
    )
    .optional()
    .context("无法读取对话引用")
}

pub(crate) fn list_for_message(conn: &Connection, message_id: &str) -> Result<Vec<ChatCitation>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, message_id, content_unit_id, search_chunk_id, ordinal, quote,
             document_revision, unit_revision, locator_json, source_kind, url, source_title,
             created_at FROM chat_citations
             WHERE message_id = ?1 ORDER BY ordinal",
        )
        .context("无法准备对话引用查询")?;
    let rows = stmt
        .query_map([message_id], citation_from_row)
        .context("无法读取对话引用")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取对话引用记录")
}

pub(crate) fn insert(conn: &Connection, citation: &ChatCitation) -> Result<usize> {
    conn.execute(
        "INSERT INTO chat_citations
         (id, message_id, content_unit_id, search_chunk_id, ordinal, quote,
          document_revision, unit_revision, locator_json, source_kind, url, source_title,
          created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            citation.id,
            citation.message_id,
            citation.content_unit_id,
            citation.search_chunk_id,
            citation.ordinal as i64,
            citation.quote,
            citation.document_revision as i64,
            citation.unit_revision as i64,
            citation.locator_json,
            citation.source_kind,
            citation.url,
            citation.source_title,
            citation.created_at as i64,
        ],
    )
    .context("无法写入对话引用")
}

pub(crate) fn delete_for_message(conn: &Connection, message_id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM chat_citations WHERE message_id = ?1",
        [message_id],
    )
    .context("无法删除对话引用")
}

fn citation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatCitation> {
    Ok(ChatCitation {
        id: row.get(0)?,
        message_id: row.get(1)?,
        content_unit_id: row.get(2)?,
        search_chunk_id: row.get(3)?,
        ordinal: row.get::<_, i64>(4)? as usize,
        quote: row.get(5)?,
        document_revision: row.get::<_, i64>(6)? as u64,
        unit_revision: row.get::<_, i64>(7)? as u64,
        locator_json: row.get(8)?,
        source_kind: row.get(9)?,
        url: row.get(10)?,
        source_title: row.get(11)?,
        created_at: row.get::<_, i64>(12)? as u64,
    })
}
