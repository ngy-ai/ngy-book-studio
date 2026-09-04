use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) ordinal: usize,
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) model: Option<String>,
    pub(crate) created_at: u64,
}

pub(crate) fn get(conn: &Connection, message_id: &str) -> Result<Option<ChatMessage>> {
    conn.query_row(
        "SELECT id, thread_id, parent_id, ordinal, role, content, model, created_at
         FROM chat_messages WHERE id = ?1",
        [message_id],
        message_from_row,
    )
    .optional()
    .context("无法读取对话消息")
}

pub(crate) fn list_for_thread(conn: &Connection, thread_id: &str) -> Result<Vec<ChatMessage>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, thread_id, parent_id, ordinal, role, content, model, created_at
             FROM chat_messages WHERE thread_id = ?1 ORDER BY ordinal",
        )
        .context("无法准备对话消息查询")?;
    let rows = stmt
        .query_map([thread_id], message_from_row)
        .context("无法读取对话消息")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取对话消息记录")
}

pub(crate) fn insert(conn: &Connection, message: &ChatMessage) -> Result<usize> {
    conn.execute(
        "INSERT INTO chat_messages
         (id, thread_id, parent_id, ordinal, role, content, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            message.id,
            message.thread_id,
            message.parent_id,
            message.ordinal as i64,
            message.role,
            message.content,
            message.model,
            message.created_at as i64,
        ],
    )
    .context("无法写入对话消息")
}

pub(crate) fn update_content(
    conn: &Connection,
    message_id: &str,
    content: &str,
    model: Option<&str>,
) -> Result<usize> {
    conn.execute(
        "UPDATE chat_messages SET content = ?2, model = ?3 WHERE id = ?1",
        params![message_id, content, model],
    )
    .context("无法更新对话消息")
}

pub(crate) fn delete(conn: &Connection, message_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM chat_messages WHERE id = ?1", [message_id])
        .context("无法删除对话消息")
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        parent_id: row.get(2)?,
        ordinal: row.get::<_, i64>(3)? as usize,
        role: row.get(4)?,
        content: row.get(5)?,
        model: row.get(6)?,
        created_at: row.get::<_, i64>(7)? as u64,
    })
}
