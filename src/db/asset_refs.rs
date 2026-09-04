use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssetRef {
    pub(crate) id: String,
    pub(crate) content_unit_id: String,
    pub(crate) asset_id: String,
    pub(crate) relation: String,
    pub(crate) ordinal: usize,
    pub(crate) locator_json: String,
    pub(crate) created_at: u64,
}

pub(crate) fn get(conn: &Connection, reference_id: &str) -> Result<Option<AssetRef>> {
    conn.query_row(
        "SELECT id, content_unit_id, asset_id, relation, ordinal, locator_json, created_at
         FROM asset_refs WHERE id = ?1",
        [reference_id],
        reference_from_row,
    )
    .optional()
    .context("无法读取资源引用")
}

pub(crate) fn list_for_unit(conn: &Connection, unit_id: &str) -> Result<Vec<AssetRef>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, content_unit_id, asset_id, relation, ordinal, locator_json, created_at
             FROM asset_refs WHERE content_unit_id = ?1 ORDER BY ordinal, id",
        )
        .context("无法准备资源引用查询")?;
    let rows = stmt
        .query_map([unit_id], reference_from_row)
        .context("无法读取资源引用")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取资源引用记录")
}

pub(crate) fn insert(conn: &Connection, reference: &AssetRef) -> Result<usize> {
    conn.execute(
        "INSERT INTO asset_refs
         (id, content_unit_id, asset_id, relation, ordinal, locator_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            reference.id,
            reference.content_unit_id,
            reference.asset_id,
            reference.relation,
            reference.ordinal as i64,
            reference.locator_json,
            reference.created_at as i64,
        ],
    )
    .context("无法写入资源引用")
}

pub(crate) fn delete(conn: &Connection, reference_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM asset_refs WHERE id = ?1", [reference_id])
        .context("无法删除资源引用")
}

fn reference_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRef> {
    Ok(AssetRef {
        id: row.get(0)?,
        content_unit_id: row.get(1)?,
        asset_id: row.get(2)?,
        relation: row.get(3)?,
        ordinal: row.get::<_, i64>(4)? as usize,
        locator_json: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
    })
}
