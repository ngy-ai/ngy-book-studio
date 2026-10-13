//! Progress writes, document installation and revision replacement.

use super::*;
use crate::annotations::TextAnchor;

/// Resolves a stable unit against the currently published source and writes
/// its canonical ordinal without releasing the SQLite write lock.
pub(crate) fn update_current_unit_progress(
    conn: &mut Connection,
    book_id: &str,
    content_unit_id: &str,
    locator_json: &str,
    anchor: Option<&TextAnchor>,
    updated_at: u64,
) -> Result<CurrentProgressWrite> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动阅读进度事务")?;
    let Some(book) = books::get(&tx, book_id)? else {
        return Ok(CurrentProgressWrite::MissingBook);
    };
    let Some(unit) = content_units::get(&tx, content_unit_id)? else {
        return Ok(CurrentProgressWrite::MissingUnit);
    };
    let current_source =
        book_sources::get_revision(&tx, book_id, book.revision)?.context("当前图书来源不存在")?;
    if unit.book_id != book_id || unit.source_id != current_source.id {
        return Ok(CurrentProgressWrite::UnitNotCurrent);
    }
    progress::upsert(
        &tx,
        &ReadingProgress {
            book_id: book_id.to_string(),
            source_revision: book.revision,
            content_unit_id: Some(content_unit_id.to_string()),
            spine_index: unit.ordinal,
            locator_json: locator_json.to_string(),
            anchor: anchor.cloned(),
            fraction: 0.0,
            updated_at,
        },
    )?;
    tx.commit().context("无法提交阅读进度事务")?;
    Ok(CurrentProgressWrite::Updated {
        source_revision: book.revision,
        spine_index: unit.ordinal,
    })
}

/// Atomically publishes a newly imported document after all referenced object
/// files have been committed by the caller.
pub(crate) fn insert_document(
    conn: &mut Connection,
    graph: &DocumentGraph<'_>,
    auto_run_background_jobs: bool,
) -> Result<()> {
    validate_document_graph(graph)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动文档导入事务")?;
    insert_blob_metadata(&tx, graph)?;
    books::insert(&tx, graph.book)?;
    book_sources::insert(&tx, graph.source)?;
    insert_document_children(&tx, graph)?;
    enqueue_derivative_jobs(&tx, graph, auto_run_background_jobs)?;
    tx.commit().context("无法提交文档导入事务")?;
    Ok(())
}

/// Atomically installs a newer source revision and makes it current. Imported
/// originals remain addressable; superseded normalized projections are
/// removed because the product has no revision-history UI. The caller reclaims
/// the returned object bytes after commit.
pub(crate) fn install_document_revision(
    conn: &mut Connection,
    graph: &DocumentGraph<'_>,
    auto_run_background_jobs: bool,
) -> Result<Vec<BlobRecord>> {
    validate_document_graph(graph)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动文档版本事务")?;
    let current = books::get(&tx, &graph.book.id)?.context("图书不存在")?;
    ensure!(
        graph.book.revision > current.revision,
        "新文档版本必须大于当前版本"
    );
    let previous_source = book_sources::get_revision(&tx, &current.id, current.revision)?
        .context("当前图书来源不存在")?;
    insert_blob_metadata(&tx, graph)?;
    books::set_cover_asset(&tx, &graph.book.id, None, graph.book.updated_at)?;
    // Derived rows belong to exactly one source revision and are rebuilt below.
    // The canonical units are not: their rows — and with them the notes,
    // citations and whole-book translations anchored to a unit ID — survive an
    // edit, so only a unit that really left the document is deleted.
    let previous_units = content_units::list_for_source(&tx, &previous_source.id)?;
    toc_entries::delete_for_source(&tx, &previous_source.id)?;
    visual_pages::delete_for_source(&tx, &previous_source.id)?;
    search_chunks::delete_for_source(&tx, &previous_source.id)?;
    assets::delete_for_source(&tx, &previous_source.id)?;
    index_jobs::delete_for_source(&tx, &previous_source.id)?;
    book_sources::insert(&tx, graph.source)?;
    sync_content_units(&tx, graph, &previous_units)?;
    // Whatever still points at the superseded source is a unit the edit removed.
    content_units::delete_for_source(&tx, &previous_source.id)?;
    if previous_source.source_kind != "original" {
        ensure!(
            book_sources::delete(&tx, &previous_source.id)? == 1,
            "无法删除已被替代的规范化来源"
        );
    }
    insert_document_children_after_units(&tx, graph)?;
    // Chapters that kept their own revision are still current, so their译文 now
    // belongs to the newly published document revision: the reader keeps showing
    // them and the translation task skips them instead of translating again.
    super::super::translations::refresh_document_revision(
        &tx,
        &graph.book.id,
        graph.book.revision,
    )?;
    enqueue_derivative_jobs(&tx, graph, auto_run_background_jobs)?;
    let catalog = books::BookCatalogUpdate {
        title: &graph.book.title,
        author: &graph.book.author,
        language: graph.book.language.as_deref(),
        description: graph.book.description.as_deref(),
        updated_at: graph.book.updated_at,
    };
    books::update_catalog(&tx, &graph.book.id, &catalog)?;
    books::set_current_source(
        &tx,
        &graph.book.id,
        &graph.book.format,
        graph.book.revision,
        &graph.book.source_object_key,
        graph.book.updated_at,
    )?;
    books::set_cover_asset(
        &tx,
        &graph.book.id,
        graph.book.cover_asset_id.as_deref(),
        graph.book.updated_at,
    )?;
    let unreferenced = blobs::list_unreferenced(&tx)?;
    tx.commit().context("无法提交文档版本事务")?;
    Ok(unreferenced)
}

/// Deletes the relational document graph and returns object metadata that is no
/// longer referenced. The caller decides when to remove bytes from
/// `LocalBlobStore` and then deletes the corresponding metadata rows.
pub(crate) fn delete_document(conn: &mut Connection, book_id: &str) -> Result<Vec<BlobRecord>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动文档删除事务")?;
    if books::delete(&tx, book_id)? == 0 {
        bail!("图书不存在");
    }
    // The book's reading-window preferences live in `settings`, which no foreign
    // key cascades: leaving them behind would apply a deleted book's choice to a
    // later book that reuses the same ID.
    settings::delete(&tx, &settings::translation_display_book_key(book_id))?;
    for surface in settings::READER_ZOOM_SURFACES {
        settings::delete(&tx, &settings::reader_zoom_book_key(surface, book_id))?;
    }
    let unreferenced = blobs::list_unreferenced(&tx)?;
    tx.commit().context("无法提交文档删除事务")?;
    Ok(unreferenced)
}

/// Replaces extracted search chunks as one unit. FTS rows follow through
/// triggers, while embeddings cascade with the old chunks.
pub(crate) fn replace_search_chunks(
    conn: &mut Connection,
    source_id: &str,
    chunks: &[SearchChunk],
) -> Result<()> {
    ensure!(
        chunks.iter().all(|chunk| chunk.source_id == source_id),
        "搜索分块不属于指定来源"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动搜索索引事务")?;
    search_chunks::delete_for_source(&tx, source_id)?;
    for chunk in chunks {
        search_chunks::insert(&tx, chunk)?;
    }
    tx.commit().context("无法提交搜索索引事务")?;
    Ok(())
}
