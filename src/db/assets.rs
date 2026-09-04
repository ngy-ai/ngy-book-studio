use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asset {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) object_key: String,
    pub(crate) kind: String,
    pub(crate) href: String,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) created_at: u64,
}

const SELECT: &str = "SELECT id, book_id, source_id, object_key, kind, href, media_type,
    byte_len, width, height, created_at FROM assets";

pub(crate) fn get(conn: &Connection, asset_id: &str) -> Result<Option<Asset>> {
    conn.query_row(
        &format!("{SELECT} WHERE id = ?1"),
        [asset_id],
        asset_from_row,
    )
    .optional()
    .context("无法读取资源")
}

pub(crate) fn list_for_source(conn: &Connection, source_id: &str) -> Result<Vec<Asset>> {
    let mut stmt = conn
        .prepare(&format!("{SELECT} WHERE source_id = ?1 ORDER BY href"))
        .context("无法准备资源查询")?;
    let rows = stmt
        .query_map([source_id], asset_from_row)
        .context("无法读取资源")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取资源记录")
}

pub(crate) fn insert(conn: &Connection, asset: &Asset) -> Result<usize> {
    conn.execute(
        "INSERT INTO assets
         (id, book_id, source_id, object_key, kind, href, media_type, byte_len, width, height, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            asset.id,
            asset.book_id,
            asset.source_id,
            asset.object_key,
            asset.kind,
            asset.href,
            asset.media_type,
            asset.byte_len as i64,
            asset.width,
            asset.height,
            asset.created_at as i64,
        ],
    )
    .context("无法写入资源")
}

pub(crate) fn update_object(
    conn: &Connection,
    asset_id: &str,
    object_key: &str,
    media_type: &str,
    byte_len: u64,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<usize> {
    conn.execute(
        "UPDATE assets SET object_key = ?2, media_type = ?3, byte_len = ?4,
         width = ?5, height = ?6 WHERE id = ?1",
        params![
            asset_id,
            object_key,
            media_type,
            byte_len as i64,
            width,
            height
        ],
    )
    .context("无法更新资源对象")
}

pub(crate) fn delete_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM assets WHERE source_id = ?1", [source_id])
        .context("无法删除来源资源")
}

fn asset_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    Ok(Asset {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        object_key: row.get(3)?,
        kind: row.get(4)?,
        href: row.get(5)?,
        media_type: row.get(6)?,
        byte_len: row.get::<_, i64>(7)? as u64,
        width: row.get(8)?,
        height: row.get(9)?,
        created_at: row.get::<_, i64>(10)? as u64,
    })
}
