use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// A node of the hierarchical bookshelf. `parent_id == None` means a top-level
/// group; the library service enforces the maximum nesting depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookGroup {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub created_at: u64,
}

pub(crate) fn list(conn: &Connection) -> Result<Vec<BookGroup>> {
    let mut stmt = conn
        .prepare("SELECT id, name, parent_id, created_at FROM groups")
        .context("无法准备分组查询")?;
    let rows = stmt.query_map([], group_from_row).context("无法读取分组")?;
    let mut groups = Vec::new();
    for row in rows {
        groups.push(row.context("无法读取分组记录")?);
    }
    Ok(groups)
}

pub(crate) fn parent_id(conn: &Connection, group_id: &str) -> Result<Option<Option<String>>> {
    conn.query_row(
        "SELECT parent_id FROM groups WHERE id = ?1",
        [group_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .context("无法读取分组")
}

pub(crate) fn depth(conn: &Connection, group_id: &str, max_depth: usize) -> Result<Option<usize>> {
    let depth = conn
        .query_row(
            "WITH RECURSIVE ancestors(id, parent_id, depth) AS ( \
               SELECT id, parent_id, 1 FROM groups WHERE id = ?1 \
               UNION \
               SELECT groups.id, groups.parent_id, ancestors.depth + 1 \
               FROM groups JOIN ancestors ON groups.id = ancestors.parent_id \
               WHERE ancestors.depth <= ?2 \
             ) \
             SELECT MAX(depth) FROM ancestors",
            params![group_id, max_depth as i64],
            |row| row.get::<_, Option<i64>>(0),
        )
        .context("无法读取分组层级")?;
    Ok(depth.map(|depth| depth as usize))
}

pub(crate) fn sibling_name_exists(
    conn: &Connection,
    excluded_id: Option<&str>,
    name: &str,
    parent_id: Option<&str>,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS( \
           SELECT 1 FROM groups \
           WHERE name = ?1 AND parent_id IS ?2 \
             AND (?3 IS NULL OR id <> ?3) \
         )",
        params![name, parent_id, excluded_id],
        |row| row.get(0),
    )
    .context("无法检查同名分组")
}

pub(crate) fn subtree_ids(conn: &Connection, group_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "WITH RECURSIVE subtree(id) AS ( \
               SELECT id FROM groups WHERE id = ?1 \
               UNION SELECT groups.id FROM groups JOIN subtree ON groups.parent_id = subtree.id \
             ) \
             SELECT id FROM subtree",
        )
        .context("无法准备分组子树查询")?;
    let rows = stmt
        .query_map([group_id], |row| row.get::<_, String>(0))
        .context("无法读取分组子树")?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.context("无法读取分组子树记录")?);
    }
    Ok(ids)
}

pub(crate) fn insert(conn: &Connection, group: &BookGroup) -> Result<usize> {
    conn.execute(
        "INSERT INTO groups (id, name, parent_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            &group.id,
            &group.name,
            group.parent_id.as_deref(),
            group.created_at as i64,
        ],
    )
    .context("无法创建分组")
}

pub(crate) fn rename(conn: &Connection, group_id: &str, name: &str) -> Result<usize> {
    conn.execute(
        "UPDATE groups SET name = ?1 WHERE id = ?2",
        params![name, group_id],
    )
    .context("无法重命名分组")
}

pub(crate) fn delete_subtree(conn: &Connection, root_group_id: &str) -> Result<usize> {
    conn.execute(
        "WITH RECURSIVE subtree(id) AS ( \
           SELECT id FROM groups WHERE id = ?1 \
           UNION SELECT groups.id FROM groups JOIN subtree ON groups.parent_id = subtree.id \
         ) \
         DELETE FROM groups WHERE id IN (SELECT id FROM subtree)",
        [root_group_id],
    )
    .context("无法删除分组")
}

fn group_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BookGroup> {
    Ok(BookGroup {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        created_at: row.get::<_, i64>(3)? as u64,
    })
}
