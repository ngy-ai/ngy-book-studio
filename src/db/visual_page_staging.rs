use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};

use super::visual_pages::VisualPage;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StagedVisualPage {
    pub(crate) job_id: String,
    pub(crate) page: VisualPage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StagingPrefixStats {
    pub(crate) page_count: usize,
    pub(crate) min_page_index: Option<usize>,
    pub(crate) max_page_index: Option<usize>,
    pub(crate) total_bytes: u64,
}

impl StagingPrefixStats {
    pub(crate) fn is_contiguous_prefix(self, expected_pages: usize) -> bool {
        self.page_count == expected_pages
            && match expected_pages {
                0 => self.min_page_index.is_none() && self.max_page_index.is_none(),
                count => {
                    self.min_page_index == Some(0) && self.max_page_index == count.checked_sub(1)
                }
            }
    }
}

const SELECT: &str = "SELECT job_id, id, book_id, source_id, content_unit_id, page_index,
    object_key, width, height, render_scale, renderer, renderer_version,
    document_revision, unit_revision, profile_id, fidelity, locator_json, created_at
    FROM visual_page_staging";

pub(crate) fn list_for_job(conn: &Connection, job_id: &str) -> Result<Vec<StagedVisualPage>> {
    let mut stmt = conn
        .prepare(&format!("{SELECT} WHERE job_id = ?1 ORDER BY page_index"))
        .context("无法准备视觉页面断点查询")?;
    let rows = stmt
        .query_map([job_id], staged_page_from_row)
        .context("无法读取视觉页面断点")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取视觉页面断点记录")
}

pub(crate) fn count_for_job(conn: &Connection, job_id: &str) -> Result<usize> {
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM visual_page_staging WHERE job_id = ?1",
            [job_id],
            |row| row.get::<_, i64>(0),
        )
        .context("无法统计视觉页面断点")?;
    usize::try_from(count).context("视觉页面断点数量超出支持范围")
}

/// Returns the complete durable-prefix summary with one aggregate query. The
/// LEFT JOIN keeps a corrupt/missing blob row visible as a count mismatch
/// instead of silently omitting its staging page.
pub(crate) fn prefix_stats(conn: &Connection, job_id: &str) -> Result<StagingPrefixStats> {
    let (page_count, blob_count, min_page_index, max_page_index, total_bytes) = conn
        .query_row(
            "SELECT COUNT(p.page_index), COUNT(b.object_key), MIN(p.page_index),
                    MAX(p.page_index), COALESCE(SUM(b.byte_len), 0)
             FROM visual_page_staging p
             LEFT JOIN blobs b ON b.object_key = p.object_key
             WHERE p.job_id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .context("无法汇总视觉页面断点")?;
    anyhow::ensure!(page_count == blob_count, "视觉页面断点对象元数据缺失");
    anyhow::ensure!(
        page_count >= 0 && total_bytes >= 0,
        "视觉页面断点汇总包含负数"
    );
    let convert_index = |value: Option<i64>| -> Result<Option<usize>> {
        value
            .map(|value| usize::try_from(value).context("视觉页面断点序号超出支持范围"))
            .transpose()
    };
    Ok(StagingPrefixStats {
        page_count: usize::try_from(page_count).context("视觉页面断点数量超出支持范围")?,
        min_page_index: convert_index(min_page_index)?,
        max_page_index: convert_index(max_page_index)?,
        total_bytes: total_bytes as u64,
    })
}

pub(crate) fn insert(conn: &Connection, job_id: &str, page: &VisualPage) -> Result<usize> {
    conn.execute(
        "INSERT INTO visual_page_staging
         (job_id, id, book_id, source_id, content_unit_id, page_index, object_key,
          width, height, render_scale, renderer, renderer_version, document_revision,
          unit_revision, profile_id, fidelity, locator_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18)",
        params![
            job_id,
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
    .context("无法写入视觉页面断点")
}

pub(crate) fn delete_for_job(conn: &Connection, job_id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM visual_page_staging WHERE job_id = ?1",
        [job_id],
    )
    .context("无法删除视觉页面断点")
}

fn staged_page_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StagedVisualPage> {
    Ok(StagedVisualPage {
        job_id: row.get(0)?,
        page: VisualPage {
            id: row.get(1)?,
            book_id: row.get(2)?,
            source_id: row.get(3)?,
            content_unit_id: row.get(4)?,
            page_index: row.get::<_, i64>(5)? as usize,
            object_key: row.get(6)?,
            width: row.get(7)?,
            height: row.get(8)?,
            render_scale: row.get(9)?,
            renderer: row.get(10)?,
            renderer_version: row.get(11)?,
            document_revision: row.get::<_, i64>(12)? as u64,
            unit_revision: row.get::<_, i64>(13)? as u64,
            profile_id: row.get(14)?,
            fidelity: row.get(15)?,
            locator_json: row.get(16)?,
            created_at: row.get::<_, i64>(17)? as u64,
        },
    })
}
