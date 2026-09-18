//! Consistency checks run against an existing database.

use super::*;

pub(crate) fn is_fts5_search_table(conn: &Connection) -> Result<bool> {
    let sql = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'search_chunks_fts'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("无法检查全文搜索表")?;
    Ok(sql.is_some_and(|sql| {
        let normalized = sql.to_ascii_lowercase();
        normalized.contains("virtual table search_chunks_fts using fts5")
            && normalized.contains("tokenize = 'trigram'")
    }))
}

pub(crate) fn foreign_key_violation_exists(conn: &Connection) -> Result<bool> {
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .context("无法准备数据库外键检查")?;
    let mut rows = stmt.query([]).context("无法检查数据库外键")?;
    Ok(rows.next().context("无法读取数据库外键检查结果")?.is_some())
}

pub(crate) fn group_cycle_exists(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "WITH RECURSIVE ancestors(origin, current) AS (
             SELECT id, parent_id FROM groups WHERE parent_id IS NOT NULL
             UNION
             SELECT ancestors.origin, groups.parent_id
             FROM ancestors JOIN groups ON groups.id = ancestors.current
             WHERE groups.parent_id IS NOT NULL
         )
         SELECT EXISTS(SELECT 1 FROM ancestors WHERE origin = current)",
        [],
        |row| row.get(0),
    )
    .context("无法检查分组循环")
}

pub(crate) fn current_sources_are_valid(conn: &Connection) -> Result<bool> {
    let invalid = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM books b
                 WHERE (
                     SELECT COUNT(*) FROM book_sources s
                     WHERE s.book_id = b.id
                       AND s.revision = b.revision
                       AND s.format = b.format
                       AND s.object_key = b.source_object_key
                 ) <> 1
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("无法检查图书当前来源")?;
    Ok(!invalid)
}

pub(crate) fn document_relations_are_valid(conn: &Connection) -> Result<bool> {
    let invalid = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM books b JOIN assets a ON a.id = b.cover_asset_id
                 WHERE a.book_id <> b.id OR a.kind <> 'cover'
                 UNION ALL
                 SELECT 1 FROM progress p JOIN content_units u ON u.id = p.content_unit_id
                 WHERE u.book_id <> p.book_id
                 UNION ALL
                 SELECT 1 FROM annotations a JOIN books b ON b.id = a.book_id
                 LEFT JOIN content_units u ON u.id = a.content_unit_id
                 WHERE a.document_revision > b.revision
                    OR (u.id IS NOT NULL AND u.book_id <> a.book_id)
                    OR (a.document_revision = b.revision AND (
                        u.id IS NULL OR a.unit_revision <> u.revision
                    ))
                 UNION ALL
                 SELECT 1 FROM content_units u
                 LEFT JOIN book_sources s ON s.id = u.source_id
                 WHERE s.id IS NULL OR s.book_id <> u.book_id
                 UNION ALL
                 SELECT 1 FROM toc_entries t
                 LEFT JOIN book_sources s ON s.id = t.source_id
                 WHERE s.id IS NULL OR s.book_id <> t.book_id
                 UNION ALL
                 SELECT 1 FROM assets a
                 LEFT JOIN book_sources s ON s.id = a.source_id
                 WHERE s.id IS NULL OR s.book_id <> a.book_id
                 UNION ALL
                 SELECT 1 FROM search_chunks c
                 LEFT JOIN book_sources s ON s.id = c.source_id
                 WHERE s.id IS NULL OR s.book_id <> c.book_id
                 UNION ALL
                 SELECT 1 FROM index_jobs j
                 LEFT JOIN book_sources s ON s.id = j.source_id
                 WHERE j.source_id IS NOT NULL
                   AND (s.id IS NULL OR s.book_id <> j.book_id)
                 UNION ALL
                 SELECT 1 FROM content_units child JOIN content_units parent ON parent.id = child.parent_id
                 WHERE parent.book_id <> child.book_id OR parent.source_id <> child.source_id
                 UNION ALL
                 SELECT 1 FROM toc_entries child JOIN toc_entries parent ON parent.id = child.parent_id
                 WHERE parent.book_id <> child.book_id OR parent.source_id <> child.source_id
                 UNION ALL
                 SELECT 1 FROM toc_entries t JOIN content_units u ON u.id = t.content_unit_id
                 WHERE u.book_id <> t.book_id OR u.source_id <> t.source_id
                 UNION ALL
                 SELECT 1 FROM asset_refs r
                 JOIN content_units u ON u.id = r.content_unit_id
                 JOIN assets a ON a.id = r.asset_id
                 WHERE u.book_id <> a.book_id OR u.source_id <> a.source_id
                 UNION ALL
                 SELECT 1 FROM search_chunks c JOIN content_units u ON u.id = c.content_unit_id
                 WHERE u.book_id <> c.book_id OR u.source_id <> c.source_id
                 UNION ALL
                 SELECT 1 FROM visual_pages p
                 LEFT JOIN content_units u ON u.id = p.content_unit_id
                 LEFT JOIN book_sources s ON s.id = p.source_id
                 WHERE s.id IS NULL OR s.book_id <> p.book_id
                    OR p.document_revision <> s.revision
                    OR (p.content_unit_id IS NOT NULL AND (
                        u.id IS NULL
                        OR u.book_id <> p.book_id OR u.source_id <> p.source_id
                        OR p.unit_revision <> u.revision
                    ))
                    OR (p.content_unit_id IS NULL AND (
                        p.renderer <> 'ngy-office-com-enhanced'
                        OR p.fidelity <> 'office_enhanced'
                        OR s.format NOT IN ('doc', 'docx', 'xlsx')
                        OR p.unit_revision <> 0
                        OR CASE WHEN json_valid(p.locator_json) THEN
                            COALESCE(json_extract(p.locator_json, '$.book_id'), '') <> p.book_id
                            OR COALESCE(json_extract(p.locator_json, '$.unit_id'), '') = ''
                            OR EXISTS(
                                SELECT 1 FROM content_units preview_unit
                                WHERE preview_unit.id = json_extract(p.locator_json, '$.unit_id')
                            )
                            OR COALESCE(json_extract(p.locator_json, '$.source.type'), '')
                                <> 'office_rendered_page'
                            OR COALESCE(json_extract(p.locator_json, '$.source.page'), 0)
                                <> p.page_index + 1
                            OR COALESCE(json_type(p.locator_json, '$.block_id'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.text_range'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.region'), 'null') <> 'null'
                        ELSE 1 END
                    ))
                 UNION ALL
                 SELECT 1 FROM visual_page_staging p
                 LEFT JOIN index_jobs j ON j.id = p.job_id
                 LEFT JOIN content_units u ON u.id = p.content_unit_id
                 LEFT JOIN book_sources s ON s.id = p.source_id
                 WHERE j.id IS NULL OR j.source_id IS NULL OR s.id IS NULL
                    OR s.book_id <> p.book_id
                    OR j.kind <> 'visual_render'
                    OR j.book_id <> p.book_id OR j.source_id <> p.source_id
                    OR p.document_revision <> s.revision
                    OR (p.content_unit_id IS NOT NULL AND (
                        u.id IS NULL
                        OR u.book_id <> p.book_id OR u.source_id <> p.source_id
                        OR p.unit_revision <> u.revision
                    ))
                    OR (p.content_unit_id IS NULL AND (
                        p.renderer <> 'ngy-office-com-enhanced'
                        OR p.fidelity <> 'office_enhanced'
                        OR s.format NOT IN ('doc', 'docx', 'xlsx')
                        OR p.unit_revision <> 0
                        OR CASE WHEN json_valid(p.locator_json) THEN
                            COALESCE(json_extract(p.locator_json, '$.book_id'), '') <> p.book_id
                            OR COALESCE(json_extract(p.locator_json, '$.unit_id'), '') = ''
                            OR EXISTS(
                                SELECT 1 FROM content_units preview_unit
                                WHERE preview_unit.id = json_extract(p.locator_json, '$.unit_id')
                            )
                            OR COALESCE(json_extract(p.locator_json, '$.source.type'), '')
                                <> 'office_rendered_page'
                            OR COALESCE(json_extract(p.locator_json, '$.source.page'), 0)
                                <> p.page_index + 1
                            OR COALESCE(json_type(p.locator_json, '$.block_id'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.text_range'), 'null') <> 'null'
                            OR COALESCE(json_type(p.locator_json, '$.region'), 'null') <> 'null'
                        ELSE 1 END
                    ))
                 UNION ALL
                 SELECT 1 FROM translations t
                 LEFT JOIN books b ON b.id = t.book_id
                 LEFT JOIN content_units u ON u.id = t.content_unit_id
                 WHERE b.id IS NULL OR u.id IS NULL
                    OR u.book_id <> t.book_id
                    OR t.document_revision > b.revision
                    OR (t.document_revision = b.revision AND t.unit_revision <> u.revision)
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("无法检查统一文档关系")?;
    Ok(!invalid)
}

pub(crate) fn search_chunk_locators_are_valid(conn: &Connection) -> Result<bool> {
    let mut statement = conn
        .prepare("SELECT book_id, content_unit_id, locator_json FROM search_chunks")
        .context("无法准备搜索分块定位检查")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("无法检查搜索分块定位")?;
    for row in rows {
        let (book_id, content_unit_id, locator_json) = row.context("无法读取搜索分块定位")?;
        let Ok(locator) = serde_json::from_str::<DocumentLocator>(&locator_json) else {
            return Ok(false);
        };
        if locator.validate().is_err()
            || locator.book_id != book_id
            || locator.unit_id != content_unit_id
            || matches!(
                locator.source.as_ref(),
                Some(SourceLocator::OfficeRenderedPage { .. })
            )
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn search_index_is_synchronized(conn: &Connection) -> Result<bool> {
    // FTS5 UNINDEXED columns cannot make a per-chunk lookup efficient. Compare
    // the two complete row sets instead; the count check preserves duplicate
    // detection because EXCEPT intentionally removes duplicates.
    conn.query_row(
        "SELECT
                 (SELECT COUNT(*) FROM search_chunks)
                     = (SELECT COUNT(*) FROM search_chunks_fts)
                 AND NOT EXISTS(
                     SELECT id, book_id, heading, body FROM search_chunks
                     EXCEPT
                     SELECT search_chunk_id, book_id, heading, body FROM search_chunks_fts
                 )
                 AND NOT EXISTS(
                     SELECT search_chunk_id, book_id, heading, body FROM search_chunks_fts
                     EXCEPT
                     SELECT id, book_id, heading, body FROM search_chunks
                 )",
        [],
        |row| row.get::<_, bool>(0),
    )
    .context("无法检查全文索引同步状态")
}
