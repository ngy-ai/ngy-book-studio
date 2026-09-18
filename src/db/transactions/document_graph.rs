//! Writing a document graph: blobs, children, content units and the
//! derivative jobs that follow, plus its validation.

use super::*;

pub(crate) fn insert_blob_metadata(conn: &Connection, graph: &DocumentGraph<'_>) -> Result<()> {
    blobs::ensure_present(conn, graph.source_blob)?;
    for blob in graph.additional_blobs {
        blobs::ensure_present(conn, blob)?;
    }
    Ok(())
}

pub(crate) fn insert_document_children(conn: &Connection, graph: &DocumentGraph<'_>) -> Result<()> {
    for unit in graph.content_units {
        content_units::insert(conn, unit)?;
    }
    insert_document_children_after_units(conn, graph)
}

/// Repoints every unit that survived the edit at the new source revision and
/// inserts the units the edit added. A unit keeps its row identity, its
/// `created_at` and — when its content did not change — its own revision, which
/// is what keeps notes and译文 anchored to it valid.
pub(crate) fn sync_content_units(
    conn: &Connection,
    graph: &DocumentGraph<'_>,
    previous_units: &[ContentUnit],
) -> Result<()> {
    let previous = previous_units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<HashSet<_>>();
    for unit in graph.content_units {
        if previous.contains(unit.id.as_str()) {
            ensure!(
                content_units::repoint_for_revision(conn, unit)? == 1,
                "内容单元版本更新失败"
            );
        } else {
            content_units::insert(conn, unit)?;
        }
    }
    Ok(())
}

pub(crate) fn insert_document_children_after_units(
    conn: &Connection,
    graph: &DocumentGraph<'_>,
) -> Result<()> {
    for entry in graph.toc_entries {
        toc_entries::insert(conn, entry)?;
    }
    for asset in graph.assets {
        assets::insert(conn, asset)?;
    }
    for reference in graph.asset_refs {
        asset_refs::insert(conn, reference)?;
    }
    for chunk in graph.search_chunks {
        search_chunks::insert(conn, chunk)?;
    }
    for page in graph.visual_pages {
        visual_pages::upsert(conn, page)?;
    }
    if let Some(reading_progress) = graph.progress {
        progress::upsert(conn, reading_progress)?;
    }
    Ok(())
}

/// Persists embedding and visual-understanding work in the same transaction as
/// the canonical units and FTS chunks. A visible revision therefore always has
/// durable intent to build its derived indexes after a restart. When automatic
/// execution is disabled, new work starts paused and remains available for an
/// explicit resume from the background-task UI.
pub(crate) fn enqueue_derivative_jobs(
    conn: &Connection,
    graph: &DocumentGraph<'_>,
    auto_run_background_jobs: bool,
) -> Result<()> {
    index_jobs::cancel_superseded_for_book(
        conn,
        &graph.book.id,
        &graph.source.id,
        graph.book.updated_at,
    )?;
    let base_cursor = serde_json::json!({
        "schema_version": 1,
        "book_id": graph.book.id,
        "source_id": graph.source.id,
        "revision": graph.book.revision,
        "next_ordinal": 0,
    });
    let visual_job_id = format!("visual-render:{}", graph.source.id);
    let visual_spec = visual_job_spec(graph, visual_job_id.clone());
    let initial_status = if auto_run_background_jobs {
        IndexJobStatus::Queued
    } else {
        IndexJobStatus::Paused
    };
    index_jobs::insert(
        conn,
        &IndexJob {
            id: visual_job_id,
            book_id: graph.book.id.clone(),
            source_id: Some(graph.source.id.clone()),
            kind: "visual_render".to_string(),
            status: initial_status,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: encode_persisted_visual_job(visual_spec, 0)?,
            error: None,
            created_at: graph.book.updated_at,
            updated_at: graph.book.updated_at,
            started_at: None,
            finished_at: None,
        },
    )?;
    for kind in ["embedding", "vision"] {
        let mut cursor = base_cursor.clone();
        cursor["kind"] = serde_json::Value::String(kind.to_string());
        index_jobs::insert(
            conn,
            &IndexJob {
                id: format!("{kind}:{}", graph.source.id),
                book_id: graph.book.id.clone(),
                source_id: Some(graph.source.id.clone()),
                kind: kind.to_string(),
                status: initial_status,
                pause_requested: false,
                cancel_requested: false,
                attempts: 0,
                cursor_json: serde_json::to_string(&cursor)
                    .context("无法序列化派生索引任务游标")?,
                error: None,
                created_at: graph.book.updated_at,
                updated_at: graph.book.updated_at,
                started_at: None,
                finished_at: None,
            },
        )?;
    }
    Ok(())
}

pub(crate) fn visual_job_spec(graph: &DocumentGraph<'_>, job_id: String) -> VisualJobSpec {
    let unit_ids = graph
        .content_units
        .iter()
        .map(|unit| unit.id.clone())
        .collect();
    let profile = RenderProfile::default();
    #[cfg(target_os = "windows")]
    if graph.source.source_kind == "original" && graph.source.format == "pdf" {
        return VisualJobSpec::from_renderer(
            job_id,
            graph.book.id.clone(),
            graph.source.id.clone(),
            Revision::new(graph.book.revision),
            unit_ids,
            profile,
            &WindowsPdfRenderer,
        );
    }
    if graph.source.source_kind == "original" && graph.source.format == "djvu" {
        return VisualJobSpec::from_renderer(
            job_id,
            graph.book.id.clone(),
            graph.source.id.clone(),
            Revision::new(graph.book.revision),
            unit_ids,
            profile,
            &DjvuPngRenderer,
        );
    }

    VisualJobSpec::from_renderer(
        job_id,
        graph.book.id.clone(),
        graph.source.id.clone(),
        Revision::new(graph.book.revision),
        unit_ids,
        profile,
        &StructuralPngRenderer,
    )
}

pub(crate) fn validate_document_graph(graph: &DocumentGraph<'_>) -> Result<()> {
    let book_id = graph.book.id.as_str();
    let source_id = graph.source.id.as_str();
    ensure!(graph.source.book_id == book_id, "图书来源与图书不匹配");
    ensure!(
        graph.source.revision == graph.book.revision
            && graph.source.format == graph.book.format
            && graph.source.object_key == graph.book.source_object_key,
        "当前来源版本与图书目录记录不匹配"
    );
    ensure!(
        graph.source_blob.object_key == graph.source.object_key,
        "来源对象元数据与图书来源不匹配"
    );
    ensure!(
        graph
            .content_units
            .iter()
            .all(|unit| unit.book_id == book_id && unit.source_id == source_id),
        "内容单元不属于当前文档来源"
    );
    ensure!(
        graph
            .toc_entries
            .iter()
            .all(|entry| entry.book_id == book_id && entry.source_id == source_id),
        "目录项不属于当前文档来源"
    );
    ensure!(
        graph
            .assets
            .iter()
            .all(|asset| asset.book_id == book_id && asset.source_id == source_id),
        "资源不属于当前文档来源"
    );
    ensure!(
        graph
            .search_chunks
            .iter()
            .all(|chunk| chunk.book_id == book_id && chunk.source_id == source_id),
        "搜索分块不属于当前文档来源"
    );
    ensure!(
        graph
            .visual_pages
            .iter()
            .all(|page| page.book_id == book_id && page.source_id == source_id),
        "可视页面不属于当前文档来源"
    );
    if let Some(reading_progress) = graph.progress {
        ensure!(
            reading_progress.book_id == book_id,
            "阅读进度不属于当前图书"
        );
    }

    let unit_ids = graph
        .content_units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<HashSet<_>>();
    let asset_ids = graph
        .assets
        .iter()
        .map(|asset| asset.id.as_str())
        .collect::<HashSet<_>>();
    let toc_ids = graph
        .toc_entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<HashSet<_>>();
    ensure!(
        graph.content_units.iter().all(|unit| unit
            .parent_id
            .as_deref()
            .is_none_or(|parent| unit_ids.contains(parent))),
        "内容单元父节点不在当前来源中"
    );
    ensure!(
        graph
            .toc_entries
            .iter()
            .all(|entry| unit_ids.contains(entry.content_unit_id.as_str())),
        "目录内容单元不在当前来源中"
    );
    ensure!(
        graph.toc_entries.iter().all(|entry| entry
            .parent_id
            .as_deref()
            .is_none_or(|parent| toc_ids.contains(parent))),
        "目录父节点不在当前来源中"
    );
    ensure!(
        graph.content_units.iter().all(|unit| {
            !unit.source_locator_json.trim().is_empty() && !unit.block_json.trim().is_empty()
        }),
        "内容单元 locator 或 block JSON 不能为空"
    );
    ensure!(
        graph
            .toc_entries
            .iter()
            .all(|entry| !entry.locator_json.trim().is_empty()
                && entry
                    .target_block_id
                    .as_deref()
                    .is_none_or(|block| !block.trim().is_empty())),
        "目录 locator JSON 或块标识无效"
    );
    ensure!(
        graph.asset_refs.iter().all(|reference| unit_ids
            .contains(reference.content_unit_id.as_str())
            && asset_ids.contains(reference.asset_id.as_str())),
        "资源引用不在当前来源中"
    );
    ensure!(
        graph
            .search_chunks
            .iter()
            .all(|chunk| unit_ids.contains(chunk.content_unit_id.as_str())),
        "搜索分块目标不在当前来源中"
    );
    ensure!(
        graph.visual_pages.iter().all(|page| page
            .content_unit_id
            .as_deref()
            .is_none_or(|unit| unit_ids.contains(unit))),
        "可视页面目标不在当前来源中"
    );
    if let Some(cover_id) = graph.book.cover_asset_id.as_deref() {
        let cover = graph
            .assets
            .iter()
            .find(|asset| asset.id == cover_id)
            .context("封面资源不在当前来源中")?;
        ensure!(cover.kind == "cover", "图书封面资源类型错误");
        ensure!(
            graph.book.cover_object_key.as_deref() == Some(cover.object_key.as_str())
                && graph.book.cover_mime.as_deref() == Some(cover.media_type.as_str()),
            "图书封面投影与资源不匹配"
        );
    } else {
        ensure!(
            graph.book.cover_object_key.is_none() && graph.book.cover_mime.is_none(),
            "无封面资源时不能保留封面对象投影"
        );
    }
    Ok(())
}
