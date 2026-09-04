use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatThread {
    pub(crate) id: String,
    pub(crate) book_id: Option<String>,
    pub(crate) title: String,
    pub(crate) scope_json: String,
    pub(crate) window_kind: String,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
}

pub(crate) fn get(conn: &Connection, thread_id: &str) -> Result<Option<ChatThread>> {
    conn.query_row(
        "SELECT id, book_id, title, scope_json, window_kind, created_at, updated_at
         FROM chat_threads WHERE id = ?1",
        [thread_id],
        thread_from_row,
    )
    .optional()
    .context("无法读取对话")
}

pub(crate) fn list_for_book(conn: &Connection, book_id: Option<&str>) -> Result<Vec<ChatThread>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, book_id, title, scope_json, window_kind, created_at, updated_at
             FROM chat_threads WHERE book_id IS ?1 ORDER BY updated_at DESC, id",
        )
        .context("无法准备对话查询")?;
    let rows = stmt
        .query_map([book_id], thread_from_row)
        .context("无法读取对话")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取对话记录")
}

pub(crate) fn insert(conn: &Connection, thread: &ChatThread) -> Result<usize> {
    conn.execute(
        "INSERT INTO chat_threads
         (id, book_id, title, scope_json, window_kind, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            thread.id,
            thread.book_id,
            thread.title,
            thread.scope_json,
            thread.window_kind,
            thread.created_at as i64,
            thread.updated_at as i64,
        ],
    )
    .context("无法写入对话")
}

pub(crate) fn update_context(
    conn: &Connection,
    thread_id: &str,
    title: &str,
    scope_json: &str,
    window_kind: &str,
    updated_at: u64,
) -> Result<usize> {
    conn.execute(
        "UPDATE chat_threads SET title = ?2, scope_json = ?3, window_kind = ?4,
         updated_at = ?5 WHERE id = ?1",
        params![thread_id, title, scope_json, window_kind, updated_at as i64],
    )
    .context("无法更新对话")
}

pub(crate) fn touch(conn: &Connection, thread_id: &str, updated_at: u64) -> Result<usize> {
    conn.execute(
        "UPDATE chat_threads SET updated_at = ?2 WHERE id = ?1",
        params![thread_id, updated_at as i64],
    )
    .context("无法更新对话时间")
}

pub(crate) fn delete(conn: &Connection, thread_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM chat_threads WHERE id = ?1", [thread_id])
        .context("无法删除对话")
}

fn thread_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatThread> {
    Ok(ChatThread {
        id: row.get(0)?,
        book_id: row.get(1)?,
        title: row.get(2)?,
        scope_json: row.get(3)?,
        window_kind: row.get(4)?,
        created_at: row.get::<_, i64>(5)? as u64,
        updated_at: row.get::<_, i64>(6)? as u64,
    })
}
