use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisualPage {
    pub(crate) id: String,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) content_unit_id: Option<String>,
    pub(crate) page_index: usize,
    pub(crate) object_key: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) render_scale: f64,
    pub(crate) renderer: String,
    pub(crate) renderer_version: String,
    pub(crate) document_revision: u64,
    pub(crate) unit_revision: u64,
    pub(crate) profile_id: String,
    pub(crate) fidelity: String,
    pub(crate) locator_json: String,
    pub(crate) created_at: u64,
}

const SELECT: &str = "SELECT id, book_id, source_id, content_unit_id, page_index,
    object_key, width, height, render_scale, renderer, renderer_version,
    document_revision, unit_revision, profile_id, fidelity, locator_json, created_at
    FROM visual_pages";

pub(crate) fn get(conn: &Connection, page_id: &str) -> Result<Option<VisualPage>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), [page_id], page_from_row)
        .optional()
        .context("无法读取可视页面")
}

pub(crate) fn list_for_source(conn: &Connection, source_id: &str) -> Result<Vec<VisualPage>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE source_id = ?1 ORDER BY page_index"
        ))
        .context("无法准备可视页面查询")?;
    let rows = stmt
        .query_map([source_id], page_from_row)
        .context("无法读取可视页面")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取可视页面记录")
}

pub(crate) fn list_batch_for_source(
    conn: &Connection,
    source_id: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<VisualPage>> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT} WHERE source_id = ?1 ORDER BY page_index LIMIT ?2 OFFSET ?3"
        ))
        .context("无法准备可视页面批次查询")?;
    let rows = stmt
        .query_map(
            params![source_id, limit as i64, offset as i64],
            page_from_row,
        )
        .context("无法读取可视页面批次")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取可视页面批次记录")
}

pub(crate) fn count_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM visual_pages WHERE source_id = ?1",
            [source_id],
            |row| row.get::<_, i64>(0),
        )
        .context("无法统计来源可视页面")?;
    usize::try_from(count).context("来源可视页面数量超出支持范围")
}

pub(crate) fn upsert(conn: &Connection, page: &VisualPage) -> Result<usize> {
    conn.execute(
        "INSERT INTO visual_pages
         (id, book_id, source_id, content_unit_id, page_index, object_key, width,
          height, render_scale, renderer, renderer_version, document_revision,
          unit_revision, profile_id, fidelity, locator_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17)
         ON CONFLICT(source_id, page_index) DO UPDATE SET
             id = excluded.id,
             content_unit_id = excluded.content_unit_id,
             object_key = excluded.object_key,
             width = excluded.width,
             height = excluded.height,
             render_scale = excluded.render_scale,
             renderer = excluded.renderer,
             renderer_version = excluded.renderer_version,
             document_revision = excluded.document_revision,
             unit_revision = excluded.unit_revision,
             profile_id = excluded.profile_id,
             fidelity = excluded.fidelity,
             locator_json = excluded.locator_json,
             created_at = excluded.created_at",
        params![
            page.id,
            page.book_id,
            page.source_id,
            page.content_unit_id,
            page.page_index as i64,
            page.object_key,
            page.width,
            page.height,
            page.render_scale,
            page.renderer,
            page.renderer_version,
            page.document_revision as i64,
            page.unit_revision as i64,
            page.profile_id,
            page.fidelity,
            page.locator_json,
            page.created_at as i64,
        ],
    )
    .context("无法写入可视页面")
}

pub(crate) fn delete_for_source(conn: &Connection, source_id: &str) -> Result<usize> {
    conn.execute("DELETE FROM visual_pages WHERE source_id = ?1", [source_id])
        .context("无法删除来源可视页面")
}

fn page_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VisualPage> {
    Ok(VisualPage {
        id: row.get(0)?,
        book_id: row.get(1)?,
        source_id: row.get(2)?,
        content_unit_id: row.get(3)?,
        page_index: row.get::<_, i64>(4)? as usize,
        object_key: row.get(5)?,
        width: row.get(6)?,
        height: row.get(7)?,
        render_scale: row.get(8)?,
        renderer: row.get(9)?,
        renderer_version: row.get(10)?,
        document_revision: row.get::<_, i64>(11)? as u64,
        unit_revision: row.get::<_, i64>(12)? as u64,
        profile_id: row.get(13)?,
        fidelity: row.get(14)?,
        locator_json: row.get(15)?,
        created_at: row.get::<_, i64>(16)? as u64,
    })
}
