use std::collections::{BTreeMap, HashSet};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "windows")]
use crate::windows_pdf_renderer::WindowsPdfRenderer;
use crate::{
    document::{BlockDocument, DocumentLocator, Revision, SourceLocator, deterministic_id},
    preview::{
        RenderProfile, RendererDescriptor, StructuralPngRenderer, VisualJobSpec,
        decode_persisted_visual_job, encode_persisted_visual_job, office_preview_unit_id,
    },
};

use super::{
    asset_refs::{self, AssetRef},
    assets::{self, Asset},
    blobs::{self, BlobRecord},
    book_sources::{self, BookSource},
    books::{self, BookRecord},
    chat_citations::{self, ChatCitation},
    chat_messages::{self, ChatMessage},
    chat_threads,
    content_units::{self, ContentUnit},
    embeddings::{self, Embedding},
    groups::{self, BookGroup},
    index_jobs::{self, IndexJob, IndexJobStatus},
    office_enhancements,
    progress::{self, ReadingProgress},
    search_chunks::{self, SearchChunk},
    toc_entries::{self, TocEntry},
    visual_page_staging,
    visual_pages::{self, VisualPage},
};

const MAX_STAGED_VISUAL_BYTES: u64 = 512 * 1024 * 1024;

/// The text was checked against immutable reader bytes by the caller. Recheck
/// the owning book and unit revisions under the write lock before publishing.
pub(crate) fn insert_annotation(
    conn: &mut Connection,
    note: &crate::annotations::Annotation,
) -> Result<crate::annotations::Annotation> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动笔记保存事务")?;
    validate_current_annotation_scope(
        &tx,
        &note.book_id,
        &note.content_unit_id,
        note.document_revision,
        note.unit_revision,
    )?;
    let mut saved = note.clone();
    if note.kind.is_mark()
        && let Some(mut existing) = super::annotations::mark_at_anchor(
            &tx,
            &note.book_id,
            &note.content_unit_id,
            note.document_revision,
            note.unit_revision,
            &note.anchor,
        )?
    {
        if existing.kind != note.kind {
            existing.kind = note.kind;
            existing.updated_at = existing.updated_at.max(note.updated_at);
            ensure!(
                super::annotations::update_mark_kind(&tx, &existing)? == 1,
                "选区划线已不存在"
            );
        }
        saved = existing;
    } else {
        super::annotations::insert(&tx, note)?;
    }
    tx.commit().context("无法提交笔记保存事务")?;
    Ok(saved)
}

pub(crate) fn delete_annotation_marks(
    conn: &mut Connection,
    book_id: &str,
    content_unit_id: &str,
    document_revision: u64,
    unit_revision: u64,
    anchor: &crate::annotations::TextAnchor,
) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动删除划线事务")?;
    validate_current_annotation_scope(
        &tx,
        book_id,
        content_unit_id,
        document_revision,
        unit_revision,
    )?;
    super::annotations::delete_marks_at_anchor(
        &tx,
        book_id,
        content_unit_id,
        document_revision,
        unit_revision,
        anchor,
    )?;
    tx.commit().context("无法提交删除划线事务")
}

fn validate_current_annotation_scope(
    conn: &Connection,
    book_id: &str,
    content_unit_id: &str,
    document_revision: u64,
    unit_revision: u64,
) -> Result<()> {
    let book = books::get(conn, book_id)?.context("图书不存在")?;
    let unit = content_units::get(conn, content_unit_id)?.context("笔记章节不存在")?;
    let source =
        book_sources::get_revision(conn, &book.id, book.revision)?.context("当前图书来源不存在")?;
    ensure!(
        unit.book_id == book.id && unit.source_id == source.id,
        "笔记章节不属于当前图书"
    );
    ensure!(
        book.revision == document_revision && unit.revision == unit_revision,
        "图书已更新，请重新打开章节后操作笔记"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisionJobReconciliation {
    Unchanged,
    Requeued,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedIndexCursor {
    schema_version: u32,
    book_id: String,
    source_id: String,
    revision: u64,
    kind: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    execution_identity: Option<String>,
    #[serde(default)]
    input_execution_identity: Option<String>,
    #[serde(default)]
    next_ordinal: usize,
}

/// All SQLite metadata produced from one immutable source revision. Object
/// bytes must already have been durably written to `LocalBlobStore`; this value
/// only contains their metadata and object keys.
pub(crate) struct DocumentGraph<'a> {
    pub(crate) book: &'a BookRecord,
    pub(crate) source_blob: &'a BlobRecord,
    pub(crate) additional_blobs: &'a [BlobRecord],
    pub(crate) source: &'a BookSource,
    pub(crate) progress: Option<&'a ReadingProgress>,
    pub(crate) content_units: &'a [ContentUnit],
    pub(crate) toc_entries: &'a [TocEntry],
    pub(crate) assets: &'a [Asset],
    pub(crate) asset_refs: &'a [AssetRef],
    pub(crate) search_chunks: &'a [SearchChunk],
    pub(crate) visual_pages: &'a [VisualPage],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CurrentProgressWrite {
    Updated {
        source_revision: u64,
        spine_index: usize,
    },
    MissingBook,
    MissingUnit,
    UnitNotCurrent,
}

/// Resolves a stable unit against the currently published source and writes
/// its canonical ordinal without releasing the SQLite write lock.
pub(crate) fn update_current_unit_progress(
    conn: &mut Connection,
    book_id: &str,
    content_unit_id: &str,
    locator_json: &str,
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
    toc_entries::delete_for_source(&tx, &previous_source.id)?;
    visual_pages::delete_for_source(&tx, &previous_source.id)?;
    content_units::delete_for_source(&tx, &previous_source.id)?;
    assets::delete_for_source(&tx, &previous_source.id)?;
    index_jobs::delete_for_source(&tx, &previous_source.id)?;
    if previous_source.source_kind != "original" {
        ensure!(
            book_sources::delete(&tx, &previous_source.id)? == 1,
            "无法删除已被替代的规范化来源"
        );
    }
    book_sources::insert(&tx, graph.source)?;
    insert_document_children(&tx, graph)?;
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

/// Collapses every legacy vision row for a current source into the stable
/// `vision:<source_id>` identity. Only one canonical row whose endpoint/model
/// execution identity is explicitly current may retain its state and output.
/// Duplicate, unknown-provenance, or stale-identity rows make the shared
/// visual chunks ambiguous, so they are removed and rebuilt from ordinal zero
/// in the same SQLite transaction.
///
/// A running row may be replaced here without letting its former worker
/// publish late output: every vision publication path validates the canonical
/// running row and its exact cursor under an IMMEDIATE transaction.
pub(crate) fn reconcile_current_vision_job(
    conn: &mut Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    now: u64,
) -> Result<VisionJobReconciliation> {
    validate_index_model(model, "视觉")?;
    validate_index_model(execution_identity, "视觉任务执行身份")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉任务去重事务")?;
    let outcome =
        reconcile_current_vision_job_in_transaction(&tx, source, model, execution_identity, now)?;
    tx.commit().context("无法提交视觉任务去重事务")?;
    Ok(outcome)
}

/// Atomically changes the derived-index contract for every currently
/// published source. The coordinator publishes the matching in-memory model
/// handles only after this transaction succeeds, so a failed settings update
/// cannot leave a mixture of old and new source identities. Embedding jobs are
/// left byte-for-byte untouched when their endpoint/model execution contract
/// did not change; post-vision embedding jobs remain independently managed.
pub(crate) fn reconfigure_current_index_jobs(
    conn: &mut Connection,
    embedding_model: &str,
    embedding_execution_identity: &str,
    embedding_execution_changed: bool,
    vision_model: &str,
    vision_execution_identity: &str,
    now: u64,
) -> Result<()> {
    validate_index_model(embedding_model, "embedding")?;
    validate_index_model(embedding_execution_identity, "embedding 任务执行身份")?;
    validate_index_model(vision_model, "视觉")?;
    validate_index_model(vision_execution_identity, "视觉任务执行身份")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动索引模型重配置事务")?;
    for source in book_sources::list_current(&tx)? {
        reconcile_current_vision_job_in_transaction(
            &tx,
            &source,
            vision_model,
            vision_execution_identity,
            now,
        )?;
        reconcile_current_embedding_job_in_transaction(
            &tx,
            &source,
            embedding_model,
            embedding_execution_identity,
            vision_execution_identity,
            embedding_execution_changed,
            now,
        )?;
    }
    tx.commit().context("无法提交索引模型重配置事务")?;
    Ok(())
}

/// Reconciles the durable whole-book translation jobs with the active target
/// language, chat model and execution identity. One job per current source and
/// target language is retained; jobs for a superseded language/source are
/// cancelled, and jobs whose model identity changed restart from the first
/// block. Passing `None` disables translation and cancels every active job.
pub(crate) fn reconfigure_translation_jobs(
    conn: &mut Connection,
    target_language: Option<&str>,
    model: &str,
    execution_identity: &str,
    auto_run: bool,
    now: u64,
) -> Result<usize> {
    if let Some(language) = target_language {
        validate_index_model(language, "翻译目标语言")?;
    }
    validate_index_model(model, "对话")?;
    validate_index_model(execution_identity, "翻译任务执行身份")?;

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动翻译任务重配置事务")?;

    let mut desired: BTreeMap<String, BookSource> = BTreeMap::new();
    if let Some(language) = target_language {
        for source in book_sources::list_current(&tx)? {
            if !format_supports_translation(&source.format) {
                continue;
            }
            let book = books::get(&tx, &source.book_id)?.context("翻译任务图书不存在")?;
            if book.revision != source.revision {
                continue;
            }
            if language_matches_source(&book.language, language) {
                // The book already uses the target language; there is nothing
                // to translate and any previous job must be superseded.
                continue;
            }
            desired.insert(format!("translation:{}:{language}", source.id), source);
        }
    }

    let existing = index_jobs::list_by_kind(&tx, "translation")?;
    let mut changed = 0usize;
    for job in &existing {
        match desired.get(&job.id) {
            Some(source) => {
                let current = valid_translation_cursor(job, source).is_some_and(|cursor| {
                    cursor.model.as_deref() == Some(model)
                        && cursor.execution_identity.as_deref() == Some(execution_identity)
                });
                if current {
                    continue;
                }
                let cursor_json = translation_cursor_json(source, model, execution_identity, 0)?;
                let status = translation_initial_status(auto_run);
                if index_jobs::reset_reconfigured(&tx, &job.id, status, &cursor_json, now)? == 1 {
                    changed += 1;
                }
            }
            None => {
                if index_jobs::cancel_reconfigured(&tx, &job.id, None, now)? == 1 {
                    changed += 1;
                }
            }
        }
    }

    let existing_ids = existing
        .iter()
        .map(|job| job.id.as_str())
        .collect::<HashSet<_>>();
    for (id, source) in &desired {
        if existing_ids.contains(id.as_str()) {
            continue;
        }
        let cursor_json = translation_cursor_json(source, model, execution_identity, 0)?;
        index_jobs::insert(
            &tx,
            &IndexJob {
                id: id.clone(),
                book_id: source.book_id.clone(),
                source_id: Some(source.id.clone()),
                kind: "translation".to_string(),
                status: translation_initial_status(auto_run),
                pause_requested: false,
                cancel_requested: false,
                attempts: 0,
                cursor_json,
                error: None,
                created_at: now,
                updated_at: now,
                started_at: None,
                finished_at: None,
            },
        )?;
        changed += 1;
    }

    tx.commit().context("无法提交翻译任务重配置事务")?;
    Ok(changed)
}

pub(crate) fn translation_initial_status(auto_run: bool) -> IndexJobStatus {
    if auto_run {
        IndexJobStatus::Queued
    } else {
        IndexJobStatus::Paused
    }
}

fn translation_cursor_json(
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    next_ordinal: usize,
) -> Result<String> {
    let cursor = PersistedIndexCursor {
        schema_version: 1,
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        revision: source.revision,
        kind: "translation".to_string(),
        model: Some(model.to_string()),
        execution_identity: Some(execution_identity.to_string()),
        input_execution_identity: None,
        next_ordinal,
    };
    serde_json::to_string(&cursor).context("无法序列化翻译任务游标")
}

fn valid_translation_cursor(job: &IndexJob, source: &BookSource) -> Option<PersistedIndexCursor> {
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).ok()?;
    (job.kind == "translation"
        && job.book_id == source.book_id
        && job.source_id.as_deref() == Some(source.id.as_str())
        && cursor.schema_version == 1
        && cursor.book_id == source.book_id
        && cursor.source_id == source.id
        && cursor.revision == source.revision
        && cursor.kind == "translation")
        .then_some(cursor)
}

/// Whole-book translation targets reflowable prose only. Fixed-layout PDF and
/// Office spreadsheet/presentation sources are outside the current scope and
/// must not accumulate meaningless translation jobs.
fn format_supports_translation(format: &str) -> bool {
    matches!(format, "epub" | "mobi" | "azw" | "azw3" | "doc" | "docx")
}

/// Whether a book's declared language already satisfies the target. Compares
/// only the primary subtag so `en-US` matches `en` and `zh` matches `zh-Hans`.
fn language_matches_source(source: &Option<String>, target: &str) -> bool {
    fn primary(tag: &str) -> String {
        tag.trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
    match source {
        Some(source) => {
            let source = primary(source);
            !source.is_empty() && source == primary(target)
        }
        None => false,
    }
}

fn reconcile_current_vision_job_in_transaction(
    conn: &Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    now: u64,
) -> Result<VisionJobReconciliation> {
    ensure!(
        !model.trim().is_empty() && model.trim() == model,
        "视觉任务模型无效"
    );
    ensure!(
        !execution_identity.trim().is_empty() && execution_identity.trim() == execution_identity,
        "视觉任务执行身份无效"
    );
    let current = books::get(conn, &source.book_id)?.context("视觉任务图书不存在")?;
    ensure!(
        current.revision == source.revision
            && book_sources::get_revision(conn, &source.book_id, source.revision)?
                .is_some_and(|stored| stored.id == source.id),
        "不能为过期来源投递视觉任务"
    );

    let canonical_id = format!("vision:{}", source.id);
    let jobs = index_jobs::list_for_source_kind(conn, &source.id, "vision")?;
    let canonical = jobs.iter().find(|job| job.id == canonical_id);
    let canonical_is_current = canonical.is_some_and(|job| {
        valid_vision_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
        })
    });
    let only_current_canonical = jobs.len() == 1 && canonical_is_current;
    if only_current_canonical {
        return Ok(VisionJobReconciliation::Unchanged);
    }

    let cancel_intent = jobs.iter().any(|job| job.cancel_requested)
        || canonical.is_some_and(|job| job.status == IndexJobStatus::Cancelled);
    let pause_intent = !cancel_intent
        && (jobs.iter().any(|job| job.pause_requested)
            || canonical.is_some_and(|job| job.status == IndexJobStatus::Paused));
    let status = if cancel_intent {
        IndexJobStatus::Cancelled
    } else if pause_intent {
        IndexJobStatus::Paused
    } else {
        IndexJobStatus::Queued
    };
    let cursor = PersistedIndexCursor {
        schema_version: 1,
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        revision: source.revision,
        kind: "vision".to_string(),
        model: Some(model.to_string()),
        execution_identity: Some(execution_identity.to_string()),
        input_execution_identity: None,
        next_ordinal: 0,
    };
    let replacement = IndexJob {
        id: canonical_id,
        book_id: source.book_id.clone(),
        source_id: Some(source.id.clone()),
        kind: "vision".to_string(),
        status,
        pause_requested: false,
        cancel_requested: false,
        attempts: 0,
        cursor_json: serde_json::to_string(&cursor).context("无法序列化新视觉任务游标")?,
        error: None,
        created_at: canonical.map_or(now, |job| job.created_at),
        updated_at: now,
        started_at: None,
        finished_at: (status == IndexJobStatus::Cancelled).then_some(now),
    };
    for job in &jobs {
        index_jobs::delete(conn, &job.id)?;
    }
    search_chunks::delete_stale_visual_derived(conn, &source.id, 0)?;
    index_jobs::insert(conn, &replacement)?;
    Ok(VisionJobReconciliation::Requeued)
}

pub(crate) fn reconcile_current_embedding_job(
    conn: &mut Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    vision_execution_identity: &str,
    now: u64,
) -> Result<()> {
    validate_index_model(model, "embedding")?;
    validate_index_model(execution_identity, "embedding 任务执行身份")?;
    validate_index_model(vision_execution_identity, "视觉任务执行身份")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动 embedding 任务收敛事务")?;
    reconcile_current_embedding_job_in_transaction(
        &tx,
        source,
        model,
        execution_identity,
        vision_execution_identity,
        true,
        now,
    )?;
    tx.commit().context("无法提交 embedding 任务收敛事务")?;
    Ok(())
}

fn reconcile_current_embedding_job_in_transaction(
    conn: &Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    vision_execution_identity: &str,
    reconcile_canonical: bool,
    now: u64,
) -> Result<()> {
    let current = books::get(conn, &source.book_id)?.context("embedding 任务图书不存在")?;
    ensure!(
        current.revision == source.revision
            && book_sources::get_revision(conn, &source.book_id, source.revision)?
                .is_some_and(|stored| stored.id == source.id),
        "不能为过期来源投递 embedding 任务"
    );
    let canonical_id = format!("embedding:{}", source.id);
    let jobs = index_jobs::list_for_source_kind(conn, &source.id, "embedding")?;
    let mut config_jobs = Vec::new();
    for job in &jobs {
        if embedding_config_job(job, &source.id) {
            config_jobs.push(job);
            continue;
        }
        let current_phase = valid_embedding_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
                && cursor.input_execution_identity.as_deref() == Some(vision_execution_identity)
        });
        if !current_phase
            && matches!(
                job.status,
                IndexJobStatus::Queued | IndexJobStatus::Running | IndexJobStatus::Paused
            )
        {
            index_jobs::update_state(
                conn,
                &job.id,
                IndexJobStatus::Cancelled,
                &job.cursor_json,
                Some("embedding 执行配置已经变更"),
                now,
                None,
                Some(now),
            )?;
        }
    }

    if !reconcile_canonical {
        return Ok(());
    }

    let canonical = config_jobs
        .iter()
        .copied()
        .find(|job| job.id == canonical_id);
    let canonical_is_current = canonical.is_some_and(|job| {
        valid_embedding_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
                && cursor.input_execution_identity.is_none()
        })
    });
    if canonical_is_current {
        for job in config_jobs {
            if job.id != canonical_id {
                index_jobs::delete(conn, &job.id)?;
            }
        }
        return Ok(());
    }

    let donor = config_jobs.iter().copied().find(|job| {
        valid_embedding_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
                && cursor.input_execution_identity.is_none()
        })
    });
    let cancel_intent = config_jobs.iter().any(|job| job.cancel_requested)
        || canonical.is_some_and(|job| job.status == IndexJobStatus::Cancelled);
    let pause_intent = !cancel_intent
        && (config_jobs.iter().any(|job| job.pause_requested)
            || canonical.is_some_and(|job| job.status == IndexJobStatus::Paused));
    let cursor = PersistedIndexCursor {
        schema_version: 1,
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        revision: source.revision,
        kind: "embedding".to_string(),
        model: Some(model.to_string()),
        execution_identity: Some(execution_identity.to_string()),
        input_execution_identity: None,
        next_ordinal: donor
            .and_then(|job| valid_embedding_cursor(job, source))
            .map_or(0, |cursor| cursor.next_ordinal),
    };
    let cursor_json = serde_json::to_string(&cursor).context("无法序列化 embedding 任务游标")?;
    let mut row = donor
        .cloned()
        .or_else(|| canonical.cloned())
        .unwrap_or_else(|| IndexJob {
            id: canonical_id.clone(),
            book_id: source.book_id.clone(),
            source_id: Some(source.id.clone()),
            kind: "embedding".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: cursor_json.clone(),
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        });
    if donor.is_none() {
        embeddings::delete_for_source_model(conn, &source.id, model)?;
    }
    row.id = canonical_id;
    row.cursor_json = cursor_json;
    if donor.is_none() {
        row.status = if cancel_intent {
            IndexJobStatus::Cancelled
        } else if pause_intent {
            IndexJobStatus::Paused
        } else {
            IndexJobStatus::Queued
        };
        row.error = None;
        row.started_at = None;
        row.finished_at = (row.status == IndexJobStatus::Cancelled).then_some(now);
    } else if row.status == IndexJobStatus::Running {
        row.status = IndexJobStatus::Queued;
        row.started_at = None;
        row.finished_at = None;
    }
    row.pause_requested = false;
    row.cancel_requested = false;
    row.updated_at = now;
    for job in config_jobs {
        index_jobs::delete(conn, &job.id)?;
    }
    index_jobs::insert(conn, &row)?;
    Ok(())
}

fn embedding_config_job(job: &IndexJob, source_id: &str) -> bool {
    if job.id == format!("embedding:{source_id}") {
        return true;
    }
    let Ok(cursor) = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json) else {
        return false;
    };
    cursor.model.as_deref().is_some_and(|model| {
        job.id
            == deterministic_id(
                "index-job",
                format!("model\0embedding\0{source_id}\0{model}").as_bytes(),
            )
    })
}

fn valid_embedding_cursor(job: &IndexJob, source: &BookSource) -> Option<PersistedIndexCursor> {
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).ok()?;
    (job.kind == "embedding"
        && job.book_id == source.book_id
        && job.source_id.as_deref() == Some(source.id.as_str())
        && cursor.schema_version == 1
        && cursor.book_id == source.book_id
        && cursor.source_id == source.id
        && cursor.revision == source.revision
        && cursor.kind == "embedding")
        .then_some(cursor)
}

/// Publishes one embedding batch and advances its durable cursor in the same
/// write transaction. Reconfiguration replaces the cursor generation, so an
/// old provider response cannot write vectors after an endpoint/model switch.
pub(crate) fn publish_embedding_batch_for_running_job(
    conn: &mut Connection,
    job_id: &str,
    expected_cursor_json: &str,
    cursor_json: &str,
    rows: &[Embedding],
    now: u64,
) -> Result<bool> {
    ensure!(!rows.is_empty(), "embedding 批次不能为空");
    let expected = serde_json::from_str::<PersistedIndexCursor>(expected_cursor_json)
        .context("embedding 任务旧游标无效")?;
    let next = serde_json::from_str::<PersistedIndexCursor>(cursor_json)
        .context("embedding 任务新游标无效")?;
    ensure!(
        expected.schema_version == 1
            && expected.kind == "embedding"
            && expected.model.is_some()
            && expected.execution_identity.is_some()
            && next.schema_version == expected.schema_version
            && next.book_id == expected.book_id
            && next.source_id == expected.source_id
            && next.revision == expected.revision
            && next.kind == expected.kind
            && next.model == expected.model
            && next.execution_identity == expected.execution_identity
            && next.input_execution_identity == expected.input_execution_identity
            && next.next_ordinal == expected.next_ordinal.saturating_add(rows.len()),
        "embedding 批次游标与执行代次不匹配"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动 embedding 批次发布事务")?;
    let Some(job) = index_jobs::get(&tx, job_id)? else {
        return Ok(false);
    };
    if job.kind != "embedding"
        || job.book_id != expected.book_id
        || job.source_id.as_deref() != Some(expected.source_id.as_str())
        || job.status != IndexJobStatus::Running
        || job.pause_requested
        || job.cancel_requested
        || job.cursor_json != expected_cursor_json
    {
        return Ok(false);
    }
    let source =
        book_sources::get(&tx, &expected.source_id)?.context("embedding 批次来源不存在")?;
    let book = books::get(&tx, &expected.book_id)?.context("embedding 批次图书不存在")?;
    ensure!(
        source.book_id == book.id
            && source.revision == expected.revision
            && book.revision == expected.revision
            && book_sources::get_revision(&tx, &book.id, book.revision)?
                .is_some_and(|current| current.id == source.id),
        "embedding 批次来源已经过期"
    );
    for row in rows {
        ensure!(
            row.model.as_str() == expected.model.as_deref().unwrap_or_default()
                && row.dimensions != 0
                && row.vector.len() == row.dimensions.saturating_mul(std::mem::size_of::<f32>()),
            "embedding 批次向量与任务模型不匹配"
        );
        let chunk = search_chunks::get(&tx, &row.search_chunk_id)?
            .context("embedding 批次搜索分块不存在")?;
        ensure!(
            chunk.book_id == expected.book_id && chunk.source_id == expected.source_id,
            "embedding 批次搜索分块越界"
        );
        embeddings::upsert(&tx, row)?;
    }
    ensure!(
        index_jobs::advance_running_cursor_from(
            &tx,
            job_id,
            expected_cursor_json,
            cursor_json,
            now,
        )?,
        "embedding 批次提交前任务代次发生变化"
    );
    tx.commit().context("无法提交 embedding 批次发布事务")?;
    Ok(true)
}

fn validate_index_model(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.trim() == value
            && value.chars().count() <= 256
            && !value.chars().any(char::is_control),
        "{label} 模型标识无效"
    );
    Ok(())
}

fn valid_vision_cursor(job: &IndexJob, source: &BookSource) -> Option<PersistedIndexCursor> {
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).ok()?;
    (job.kind == "vision"
        && job.book_id == source.book_id
        && job.source_id.as_deref() == Some(source.id.as_str())
        && cursor.schema_version == 1
        && cursor.book_id == source.book_id
        && cursor.source_id == source.id
        && cursor.revision == source.revision
        && cursor.kind == "vision")
        .then_some(cursor)
}

/// Atomically replaces every visual chunk from one page onward. Vision output
/// can contain several regions for a page, so publishing them one row at a
/// time would expose a partial page and leave surplus regions from an earlier
/// run. Later-page chunks are intentionally removed as well: the durable page
/// cursor will rebuild them with the same model contract before completion.
pub(crate) fn replace_visual_search_chunks_for_running_vision_job(
    conn: &mut Connection,
    job_id: &str,
    expected_cursor_json: &str,
    book_id: &str,
    source_id: &str,
    first_ordinal: usize,
    chunks: &[SearchChunk],
) -> Result<bool> {
    validate_visual_search_chunks(book_id, source_id, first_ordinal, chunks)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动受保护的视觉页面分块替换事务")?;
    if !running_canonical_vision_job_matches(&tx, job_id, expected_cursor_json, source_id)? {
        return Ok(false);
    }
    validate_current_visual_source(&tx, book_id, source_id)?;
    for chunk in chunks {
        ensure!(
            content_units::get(&tx, &chunk.content_unit_id)?.is_some_and(|unit| {
                unit.book_id == chunk.book_id && unit.source_id == chunk.source_id
            }),
            "视觉分块内容单元与来源不匹配"
        );
    }
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_ordinal)?;
    for chunk in chunks {
        search_chunks::upsert_derived(&tx, chunk)?;
    }
    tx.commit()
        .context("无法提交受保护的视觉页面分块替换事务")?;
    Ok(true)
}

pub(crate) fn replace_visual_search_chunks_from_ordinal(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    first_ordinal: usize,
    chunks: &[SearchChunk],
) -> Result<()> {
    validate_visual_search_chunks(book_id, source_id, first_ordinal, chunks)?;

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面分块替换事务")?;
    validate_current_visual_source(&tx, book_id, source_id)?;
    for chunk in chunks {
        ensure!(
            content_units::get(&tx, &chunk.content_unit_id)?.is_some_and(|unit| {
                unit.book_id == chunk.book_id && unit.source_id == chunk.source_id
            }),
            "视觉分块内容单元与来源不匹配"
        );
    }
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_ordinal)?;
    for chunk in chunks {
        search_chunks::upsert_derived(&tx, chunk)?;
    }
    tx.commit().context("无法提交视觉页面分块替换事务")?;
    Ok(())
}

fn validate_visual_search_chunks(
    book_id: &str,
    source_id: &str,
    first_ordinal: usize,
    chunks: &[SearchChunk],
) -> Result<()> {
    ensure!(!chunks.is_empty(), "视觉页面至少需要一个可索引分块");
    ensure!(
        chunks.iter().all(|chunk| {
            chunk.id.starts_with("vision-chunk-")
                && chunk.book_id == book_id
                && chunk.source_id == source_id
                && chunk.ordinal >= first_ordinal
        }),
        "视觉页面分块身份或序号不匹配"
    );
    ensure!(
        chunks.iter().any(|chunk| chunk.ordinal == first_ordinal),
        "视觉页面分块必须从页面首序号开始"
    );
    ensure!(
        chunks
            .iter()
            .map(|chunk| chunk.id.as_str())
            .collect::<HashSet<_>>()
            .len()
            == chunks.len()
            && chunks
                .iter()
                .map(|chunk| chunk.ordinal)
                .collect::<HashSet<_>>()
                .len()
                == chunks.len(),
        "视觉页面包含重复分块 ID 或序号"
    );
    Ok(())
}

fn validate_current_visual_source(conn: &Connection, book_id: &str, source_id: &str) -> Result<()> {
    let source = book_sources::get(conn, source_id)?.context("视觉分块来源不存在")?;
    let book = books::get(conn, book_id)?.context("视觉分块图书不存在")?;
    ensure!(
        source.book_id == book.id
            && book.revision == source.revision
            && book_sources::get_revision(conn, &book.id, book.revision)?
                .is_some_and(|current| current.id == source.id),
        "视觉分块不能写入过期来源"
    );
    Ok(())
}

fn running_canonical_vision_job_matches(
    conn: &Connection,
    job_id: &str,
    expected_cursor_json: &str,
    source_id: &str,
) -> Result<bool> {
    Ok(index_jobs::get(conn, job_id)?.is_some_and(|job| {
        job.id == format!("vision:{source_id}")
            && job.source_id.as_deref() == Some(source_id)
            && job.kind == "vision"
            && job.status == IndexJobStatus::Running
            && !job.pause_requested
            && !job.cancel_requested
            && job.cursor_json == expected_cursor_json
    }))
}

/// Removes descriptions for pages no longer present in the current atomic
/// visual-page set. Their embeddings cascade with the deleted chunks.
pub(crate) fn finish_visual_search_chunks(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    first_stale_ordinal: usize,
) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉搜索收尾事务")?;
    let book = books::get(&tx, book_id)?.context("视觉搜索图书不存在")?;
    let source = book_sources::get(&tx, source_id)?.context("视觉搜索来源不存在")?;
    ensure!(
        source.book_id == book.id && source.revision == book.revision,
        "视觉搜索任务来源已经过期"
    );
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_stale_ordinal)?;
    tx.commit().context("无法提交视觉搜索收尾事务")?;
    Ok(())
}

pub(crate) fn finish_visual_search_chunks_for_running_vision_job(
    conn: &mut Connection,
    job_id: &str,
    expected_cursor_json: &str,
    book_id: &str,
    source_id: &str,
    first_stale_ordinal: usize,
) -> Result<bool> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动受保护的视觉搜索收尾事务")?;
    if !running_canonical_vision_job_matches(&tx, job_id, expected_cursor_json, source_id)? {
        return Ok(false);
    }
    validate_current_visual_source(&tx, book_id, source_id)?;
    search_chunks::delete_stale_visual_derived(&tx, source_id, first_stale_ordinal)?;
    tx.commit().context("无法提交受保护的视觉搜索收尾事务")?;
    Ok(true)
}

pub(crate) fn enqueue_index_job_for_running_vision_job(
    conn: &mut Connection,
    vision_job_id: &str,
    expected_vision_cursor_json: &str,
    source_id: &str,
    job: &IndexJob,
) -> Result<bool> {
    ensure!(
        job.kind == "embedding" && job.source_id.as_deref() == Some(source_id),
        "视觉后续索引任务身份无效"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉后续索引投递事务")?;
    if !running_canonical_vision_job_matches(
        &tx,
        vision_job_id,
        expected_vision_cursor_json,
        source_id,
    )? {
        return Ok(false);
    }
    if index_jobs::insert_if_absent(&tx, job)? == 0 {
        index_jobs::reset_terminal(&tx, &job.id, &job.cursor_json, job.updated_at)?;
    }
    tx.commit().context("无法提交视觉后续索引投递事务")?;
    Ok(true)
}

/// Atomically publishes a fully rendered visual-page set after the caller has
/// durably written every immutable object. The returned rows are objects from
/// the replaced page set that have no remaining database reference after the
/// commit; the caller may remove their bytes and then their metadata. Objects
/// left by a failed transaction are intentionally not deleted here; startup GC
/// owns that cleanup.
pub(crate) fn replace_visual_pages(
    conn: &mut Connection,
    book_id: &str,
    source_id: &str,
    revision: u64,
    page_blobs: &[BlobRecord],
    pages: &[VisualPage],
) -> Result<Vec<BlobRecord>> {
    ensure!(!book_id.trim().is_empty(), "视觉页面 book ID 不能为空");
    ensure!(!source_id.trim().is_empty(), "视觉页面 source ID 不能为空");
    let blob_keys = page_blobs
        .iter()
        .map(|blob| blob.object_key.as_str())
        .collect::<HashSet<_>>();
    ensure!(
        blob_keys.len() == page_blobs.len(),
        "视觉页面包含重复的对象元数据"
    );
    ensure!(
        pages.iter().all(|page| {
            page.book_id == book_id
                && page.source_id == source_id
                && page.document_revision == revision
                && !page.renderer.trim().is_empty()
                && !page.renderer_version.trim().is_empty()
                && !page.profile_id.trim().is_empty()
                && matches!(
                    page.fidelity.as_str(),
                    "normalized" | "structural" | "office_enhanced"
                )
                && blob_keys.contains(page.object_key.as_str())
        }),
        "视觉页面与图书、来源、渲染元数据或对象元数据不匹配"
    );
    ensure!(
        pages
            .iter()
            .enumerate()
            .all(|(index, page)| page.page_index == index),
        "视觉页面序号必须连续且从 0 开始"
    );

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面替换事务")?;
    let book = books::get(&tx, book_id)?.context("视觉页面所属图书不存在")?;
    ensure!(
        book.revision == revision,
        "视觉页面对应的图书 revision 已过期"
    );
    let source = super::book_sources::get(&tx, source_id)?.context("视觉页面来源不存在")?;
    ensure!(
        source.book_id == book_id && source.revision == revision,
        "视觉页面来源与图书 revision 不匹配"
    );
    for page in pages {
        validate_visual_page_unit_for_source(&tx, book_id, source_id, page)?;
    }
    for blob in page_blobs {
        blobs::ensure_present(&tx, blob)?;
    }
    let replaced_object_keys = visual_pages::list_for_source(&tx, source_id)?
        .into_iter()
        .map(|page| page.object_key)
        .collect::<HashSet<_>>();
    visual_pages::delete_for_source(&tx, source_id)?;
    for page in pages {
        visual_pages::upsert(&tx, page)?;
    }
    let unreferenced = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉页面替换事务")?;
    Ok(unreferenced)
}

/// Durably checkpoints one newly rendered page. The immutable object must be
/// written before this function is called. Its metadata, the staging
/// reference, and the visual job cursor advance in one transaction, so a
/// recovered cursor can never point past its durable page prefix.
pub(crate) fn checkpoint_visual_page(
    conn: &mut Connection,
    spec: &VisualJobSpec,
    blob: &BlobRecord,
    page: &VisualPage,
    completed_pages: usize,
    now: u64,
) -> Result<()> {
    spec.validate()?;
    ensure!(
        page.page_index.checked_add(1) == Some(completed_pages),
        "视觉页面断点与页面序号不一致"
    );
    validate_visual_page_for_spec(spec, page)?;
    ensure!(blob.object_key == page.object_key, "视觉页面断点对象不匹配");

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面断点事务")?;
    let book = books::get(&tx, &spec.book_id)?.context("视觉页面断点所属图书不存在")?;
    let source = book_sources::get(&tx, &spec.source_id)?.context("视觉页面断点来源不存在")?;
    ensure!(
        book.revision == spec.document_revision.get()
            && source.book_id == book.id
            && source.revision == book.revision,
        "视觉页面断点不能写入过期来源"
    );
    validate_visual_page_unit(&tx, spec, page)?;

    let job = index_jobs::get(&tx, &spec.id)?.context("视觉页面断点任务不存在")?;
    ensure!(
        job.kind == "visual_render"
            && job.status == IndexJobStatus::Running
            && job.book_id == spec.book_id
            && job.source_id.as_deref() == Some(spec.source_id.as_str()),
        "视觉页面断点任务状态或归属不匹配"
    );
    let (stored_spec, stored_completed) = decode_persisted_visual_job(&job.cursor_json)?;
    ensure!(stored_spec == *spec, "视觉页面断点任务规格已经变化");
    ensure!(
        stored_completed == page.page_index,
        "视觉页面断点不是当前持久前缀的下一页"
    );
    let prefix = visual_page_staging::prefix_stats(&tx, &spec.id)?;
    ensure!(
        prefix.is_contiguous_prefix(stored_completed),
        "视觉页面持久断点不连续"
    );
    ensure!(
        prefix
            .total_bytes
            .checked_add(blob.byte_len)
            .is_some_and(|total| total <= MAX_STAGED_VISUAL_BYTES),
        "视觉页面断点超过批次总大小上限"
    );

    blobs::ensure_present(&tx, blob)?;
    ensure!(
        visual_page_staging::insert(&tx, &spec.id, page)? == 1,
        "视觉页面断点没有写入"
    );
    let cursor_json = encode_persisted_visual_job(spec.clone(), completed_pages)?;
    ensure!(
        index_jobs::update_state_from(
            &tx,
            &spec.id,
            IndexJobStatus::Running,
            IndexJobStatus::Running,
            &cursor_json,
            None,
            now,
            None,
            None,
        )? == 1,
        "视觉页面断点游标没有更新"
    );
    tx.commit().context("无法提交视觉页面断点事务")?;
    Ok(())
}

/// Atomically promotes a complete staged prefix to the visible visual page
/// set and marks its job succeeded. Until this transaction commits, an older
/// final page set remains fully visible.
pub(crate) fn publish_staged_visual_pages(
    conn: &mut Connection,
    spec: &VisualJobSpec,
    total_pages: usize,
    now: u64,
) -> Result<Vec<BlobRecord>> {
    spec.validate()?;
    ensure!(total_pages > 0, "视觉任务没有可发布页面");
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面发布事务")?;
    let book = books::get(&tx, &spec.book_id)?.context("视觉页面所属图书不存在")?;
    let source = book_sources::get(&tx, &spec.source_id)?.context("视觉页面来源不存在")?;
    ensure!(
        book.revision == spec.document_revision.get()
            && source.book_id == book.id
            && source.revision == book.revision,
        "视觉页面不能发布到过期来源"
    );
    let job = index_jobs::get(&tx, &spec.id)?.context("视觉页面任务不存在")?;
    ensure!(
        job.kind == "visual_render"
            && job.status == IndexJobStatus::Running
            && job.book_id == spec.book_id
            && job.source_id.as_deref() == Some(spec.source_id.as_str())
            && !job.pause_requested
            && !job.cancel_requested,
        "视觉页面任务状态已变化，拒绝发布"
    );
    let (stored_spec, completed_pages) = decode_persisted_visual_job(&job.cursor_json)?;
    ensure!(stored_spec == *spec, "视觉页面任务规格已经变化");
    ensure!(completed_pages == total_pages, "视觉页面断点尚未完成");

    let staged = visual_page_staging::list_for_job(&tx, &spec.id)?;
    ensure!(staged.len() == total_pages, "视觉页面断点数量不完整");
    for (index, staged) in staged.iter().enumerate() {
        ensure!(
            staged.job_id == spec.id && staged.page.page_index == index,
            "视觉页面断点序号不连续"
        );
        validate_visual_page_for_spec(spec, &staged.page)?;
        validate_visual_page_unit(&tx, spec, &staged.page)?;
        ensure!(
            blobs::get(&tx, &staged.page.object_key)?.is_some(),
            "视觉页面断点对象元数据不存在"
        );
    }

    let replaced_object_keys = visual_pages::list_for_source(&tx, &spec.source_id)?
        .into_iter()
        .map(|page| page.object_key)
        .collect::<HashSet<_>>();
    visual_pages::delete_for_source(&tx, &spec.source_id)?;
    for staged in &staged {
        visual_pages::upsert(&tx, &staged.page)?;
    }
    visual_page_staging::delete_for_job(&tx, &spec.id)?;
    let cursor_json = encode_persisted_visual_job(spec.clone(), total_pages)?;
    ensure!(
        index_jobs::update_state_from(
            &tx,
            &spec.id,
            IndexJobStatus::Running,
            IndexJobStatus::Succeeded,
            &cursor_json,
            None,
            now,
            None,
            Some(now),
        )? == 1,
        "视觉页面任务完成状态没有更新"
    );
    let unreferenced = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉页面发布事务")?;
    Ok(unreferenced)
}

/// Drops a failed/cancelled job's resumable prefix before an explicit retry.
/// Returned objects lost their last reference in this transaction and may be
/// reclaimed after the caller releases the publication lock.
pub(crate) fn clear_visual_page_staging(
    conn: &mut Connection,
    spec: &VisualJobSpec,
) -> Result<Vec<BlobRecord>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉页面断点清理事务")?;
    let job = index_jobs::get(&tx, &spec.id)?.context("视觉页面断点任务不存在")?;
    let (stored_spec, _) = decode_persisted_visual_job(&job.cursor_json)?;
    ensure!(
        job.kind == "visual_render"
            && job.book_id == spec.book_id
            && job.source_id.as_deref() == Some(spec.source_id.as_str())
            && stored_spec == *spec,
        "视觉页面断点清理目标不匹配"
    );
    let staged_keys = visual_page_staging::list_for_job(&tx, &spec.id)?
        .into_iter()
        .map(|staged| staged.page.object_key)
        .collect::<HashSet<_>>();
    visual_page_staging::delete_for_job(&tx, &spec.id)?;
    let unreferenced = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| staged_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉页面断点清理事务")?;
    Ok(unreferenced)
}

fn validate_visual_page_for_spec(spec: &VisualJobSpec, page: &VisualPage) -> Result<()> {
    ensure!(
        page.book_id == spec.book_id
            && page.source_id == spec.source_id
            && page.document_revision == spec.document_revision.get()
            && page.render_scale == spec.profile.scale()
            && page.renderer == spec.renderer
            && page.renderer_version == spec.renderer_version
            && page.profile_id == spec.profile.stable_id()
            && page.fidelity
                == match spec.fidelity {
                    crate::preview::RenderFidelity::Normalized => "normalized",
                    crate::preview::RenderFidelity::Structural => "structural",
                    crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
                }
            && page.width > 0
            && page.height > 0
            && !page.object_key.trim().is_empty()
            && !page.locator_json.trim().is_empty(),
        "视觉页面与任务规格不匹配"
    );
    Ok(())
}

fn validate_visual_page_unit(
    conn: &Connection,
    spec: &VisualJobSpec,
    page: &VisualPage,
) -> Result<()> {
    validate_visual_page_unit_for_source(conn, &spec.book_id, &spec.source_id, page)?;
    Ok(())
}

fn validate_visual_page_unit_for_source(
    conn: &Connection,
    book_id: &str,
    source_id: &str,
    page: &VisualPage,
) -> Result<()> {
    let source = book_sources::get(conn, source_id)?.context("视觉页面来源不存在")?;
    ensure!(source.book_id == book_id, "视觉页面来源不属于当前图书");
    let locator = serde_json::from_str::<DocumentLocator>(&page.locator_json)
        .context("视觉页面 locator JSON 无效")?;
    locator.validate().context("视觉页面 locator 无效")?;
    ensure!(
        locator.book_id == book_id,
        "视觉页面 locator 不属于当前图书"
    );
    match page.content_unit_id.as_deref() {
        Some(unit_id) => {
            ensure!(
                locator.unit_id == unit_id
                    && !matches!(
                        locator.source.as_ref(),
                        Some(SourceLocator::OfficeRenderedPage { .. })
                    ),
                "视觉页面 locator 与内容单元不匹配"
            );
            let unit = content_units::get(conn, unit_id)?.context("视觉页面内容单元不存在")?;
            ensure!(
                unit.book_id == book_id
                    && unit.source_id == source_id
                    && unit.revision == page.unit_revision,
                "视觉页面内容单元 revision 不匹配"
            );
        }
        None => {
            let expected_page = u32::try_from(page.page_index)
                .ok()
                .and_then(|index| index.checked_add(1));
            ensure!(
                page.renderer == "moye-office-com-enhanced"
                    && page.fidelity == "office_enhanced"
                    && matches!(source.format.as_str(), "doc" | "docx" | "xlsx")
                    && page.unit_revision == Revision::INITIAL.get()
                    && locator.unit_id == office_preview_unit_id(book_id, source_id)
                    && locator.block_id.is_none()
                    && locator.text_range.is_none()
                    && locator.region.is_none()
                    && matches!(
                        (locator.source.as_ref(), expected_page),
                        (
                            Some(SourceLocator::OfficeRenderedPage { page }),
                            Some(expected_page)
                        ) if *page == expected_page
                    ),
                "无内容单元的视觉页面不是有效的 Office preview-only 页面"
            );
        }
    }
    Ok(())
}

/// Rebuilds only derived visual work when the registered renderer or default
/// profile changes. Canonical content, original objects and text FTS remain
/// untouched; stale visual chunks (and their cascading embeddings) are
/// removed before vision is requeued from ordinal zero.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct VisualRenderReconciliation {
    pub(crate) changed_sources: usize,
    /// Visual page objects whose last database reference was removed by this
    /// transaction. Callers must still serialize with object publication and
    /// re-check the reference immediately before deleting the bytes.
    pub(crate) unreferenced_blobs: Vec<BlobRecord>,
}

pub(crate) fn reconcile_visual_render_jobs(
    conn: &mut Connection,
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
    now: u64,
) -> Result<VisualRenderReconciliation> {
    validate_visual_renderers(renderers, profile)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉 renderer 恢复事务")?;
    let current_sources = book_sources::list_current(&tx)?;
    let mut changed = 0;
    let mut replaced_object_keys = HashSet::new();

    for source in current_sources {
        if reconcile_visual_source(
            &tx,
            source,
            renderers,
            profile,
            now,
            false,
            &mut replaced_object_keys,
        )? {
            changed += 1;
        }
    }

    let unreferenced_blobs = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交视觉 renderer 恢复事务")?;
    Ok(VisualRenderReconciliation {
        changed_sources: changed,
        unreferenced_blobs,
    })
}

/// Persists the explicit per-book Office opt-in and atomically replaces the
/// current source's visual/vision pipeline. The caller must first stop any
/// in-memory renderer for the returned source ID, then wake the coordinator
/// after this transaction commits.
#[cfg(target_os = "windows")]
pub(crate) fn set_office_enhancement(
    conn: &mut Connection,
    book_id: &str,
    enabled: bool,
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
    now: u64,
) -> Result<(String, VisualRenderReconciliation)> {
    validate_visual_renderers(renderers, profile)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动 Office 增强预览设置事务")?;
    let book = books::get(&tx, book_id)?.context("图书不存在")?;
    let source =
        book_sources::get_revision(&tx, book_id, book.revision)?.context("当前图书来源不存在")?;
    if enabled {
        ensure!(
            source.source_kind == "original"
                && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx"),
            "只有尚未规范化编辑的 Office 原件可以启用增强预览"
        );
        ensure!(
            renderers
                .iter()
                .any(|renderer| renderer.renderer == "moye-office-com-enhanced"),
            "Office 增强 renderer 未注册"
        );
    }
    office_enhancements::upsert(&tx, book_id, enabled, now)?;
    let source_id = source.id.clone();
    let mut replaced_object_keys = HashSet::new();
    let changed = reconcile_visual_source(
        &tx,
        source,
        renderers,
        profile,
        now,
        true,
        &mut replaced_object_keys,
    )?;
    debug_assert!(changed, "forced Office renderer reconciliation did not run");
    let unreferenced_blobs = blobs::list_unreferenced(&tx)?
        .into_iter()
        .filter(|blob| replaced_object_keys.contains(&blob.object_key))
        .collect();
    tx.commit().context("无法提交 Office 增强预览设置事务")?;
    Ok((
        source_id,
        VisualRenderReconciliation {
            changed_sources: usize::from(changed),
            unreferenced_blobs,
        },
    ))
}

fn validate_visual_renderers(
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
) -> Result<()> {
    ensure!(!renderers.is_empty(), "视觉任务恢复缺少已注册 renderer");
    profile.validate()?;
    let mut renderer_names = HashSet::new();
    for renderer in renderers {
        renderer.validate()?;
        ensure!(
            renderer_names.insert(renderer.renderer.as_str()),
            "视觉任务恢复包含重复 renderer {}",
            renderer.renderer
        );
    }
    Ok(())
}

fn reconcile_visual_source(
    conn: &Connection,
    source: BookSource,
    renderers: &[RendererDescriptor],
    profile: &RenderProfile,
    now: u64,
    force: bool,
    replaced_object_keys: &mut HashSet<String>,
) -> Result<bool> {
    let units = content_units::list_for_source(conn, &source.id)?;
    ensure!(!units.is_empty(), "当前图书来源没有内容单元");
    let descriptor = renderer_for_source(conn, &source, renderers)?;
    let job_id = format!("visual-render:{}", source.id);
    let expected = VisualJobSpec {
        id: job_id.clone(),
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        document_revision: Revision::new(source.revision),
        renderer: descriptor.renderer.clone(),
        renderer_version: descriptor.version.clone(),
        fidelity: descriptor.fidelity,
        unit_ids: units.iter().map(|unit| unit.id.clone()).collect(),
        profile: profile.clone(),
    };
    expected.validate()?;
    let existing = index_jobs::get(conn, &job_id)?;
    let is_current = match existing.as_ref() {
        Some(job) => visual_job_matches_current(conn, job, &expected)?,
        None => false,
    };
    if is_current && !force {
        return Ok(false);
    }

    replaced_object_keys.extend(
        visual_pages::list_for_source(conn, &source.id)?
            .into_iter()
            .map(|page| page.object_key),
    );
    replaced_object_keys.extend(
        visual_page_staging::list_for_job(conn, &job_id)?
            .into_iter()
            .map(|staged| staged.page.object_key),
    );
    visual_pages::delete_for_source(conn, &source.id)?;
    visual_page_staging::delete_for_job(conn, &job_id)?;
    search_chunks::delete_stale_visual_derived(conn, &source.id, 0)?;
    if existing.is_some() {
        index_jobs::delete(conn, &job_id)?;
    }
    index_jobs::insert(
        conn,
        &IndexJob {
            id: job_id,
            book_id: source.book_id.clone(),
            source_id: Some(source.id.clone()),
            kind: "visual_render".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: encode_persisted_visual_job(expected, 0)?,
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        },
    )?;

    let base_vision_id = format!("vision:{}", source.id);
    let base_embedding_id = format!("embedding:{}", source.id);
    for job in index_jobs::list_for_book(conn, &source.book_id)? {
        if job.source_id.as_deref() != Some(source.id.as_str()) {
            continue;
        }
        if job.kind == "vision" || (job.kind == "embedding" && job.id != base_embedding_id) {
            index_jobs::delete(conn, &job.id)?;
        }
    }
    let vision_cursor = serde_json::json!({
        "schema_version": 1,
        "book_id": source.book_id,
        "source_id": source.id,
        "revision": source.revision,
        "kind": "vision",
        "model": null,
        "next_ordinal": 0,
    });
    index_jobs::insert(
        conn,
        &IndexJob {
            id: base_vision_id,
            book_id: source.book_id,
            source_id: Some(source.id),
            kind: "vision".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: serde_json::to_string(&vision_cursor)
                .context("无法序列化视觉索引恢复游标")?,
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        },
    )?;
    Ok(true)
}

fn renderer_for_source<'a>(
    conn: &Connection,
    source: &BookSource,
    renderers: &'a [RendererDescriptor],
) -> Result<&'a RendererDescriptor> {
    #[cfg(target_os = "windows")]
    if source.source_kind == "original"
        && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx")
        && office_enhancements::is_enabled(conn, &source.book_id)?
        && let Some(renderer) = renderers
            .iter()
            .find(|renderer| renderer.renderer == "moye-office-com-enhanced")
    {
        return Ok(renderer);
    }
    #[cfg(target_os = "windows")]
    if source.source_kind == "original"
        && source.format == "pdf"
        && let Some(renderer) = renderers
            .iter()
            .find(|renderer| renderer.renderer == "moye-windows-pdf-png")
    {
        return Ok(renderer);
    }
    renderers
        .iter()
        .find(|renderer| renderer.renderer == "moye-structural-png")
        .or_else(|| (renderers.len() == 1).then(|| &renderers[0]))
        .context("视觉任务恢复找不到当前来源所需的 renderer")
}

fn visual_job_matches_current(
    conn: &Connection,
    job: &IndexJob,
    expected: &VisualJobSpec,
) -> Result<bool> {
    if job.kind != "visual_render"
        || job.book_id != expected.book_id
        || job.source_id.as_deref() != Some(expected.source_id.as_str())
    {
        return Ok(false);
    }
    let Ok((stored, completed_pages)) = decode_persisted_visual_job(&job.cursor_json) else {
        return Ok(false);
    };
    if stored != *expected {
        return Ok(false);
    }
    if job.status != IndexJobStatus::Succeeded {
        let staged = visual_page_staging::list_for_job(conn, &job.id)?;
        return Ok(staged.len() == completed_pages
            && staged.iter().enumerate().all(|(index, staged)| {
                staged.page.page_index == index
                    && staged.page.book_id == expected.book_id
                    && staged.page.source_id == expected.source_id
                    && staged.page.document_revision == expected.document_revision.get()
                    && staged.page.renderer == expected.renderer
                    && staged.page.renderer_version == expected.renderer_version
                    && staged.page.profile_id == expected.profile.stable_id()
                    && staged.page.render_scale == expected.profile.scale()
                    && validate_visual_page_for_spec(expected, &staged.page).is_ok()
                    && validate_visual_page_unit(conn, expected, &staged.page).is_ok()
            }));
    }
    if visual_page_staging::count_for_job(conn, &job.id)? != 0 {
        return Ok(false);
    }
    let pages = visual_pages::list_for_source(conn, &expected.source_id)?;
    if pages.is_empty() || pages.len() != completed_pages {
        return Ok(false);
    }
    let all_preview_only = pages.iter().all(|page| page.content_unit_id.is_none());
    let all_unit_mapped = pages.iter().all(|page| page.content_unit_id.is_some());
    if !all_preview_only && !all_unit_mapped {
        return Ok(false);
    }
    let covered_units = pages
        .iter()
        .filter_map(|page| page.content_unit_id.as_deref())
        .collect::<HashSet<_>>();
    Ok((all_preview_only
        || expected
            .unit_ids
            .iter()
            .all(|unit_id| covered_units.contains(unit_id.as_str())))
        && pages.iter().enumerate().all(|(index, page)| {
            page.page_index == index
                && page.book_id == expected.book_id
                && page.source_id == expected.source_id
                && page.document_revision == expected.document_revision.get()
                && page.renderer == expected.renderer
                && page.renderer_version == expected.renderer_version
                && page.profile_id == expected.profile.stable_id()
                && page.render_scale == expected.profile.scale()
                && page
                    .content_unit_id
                    .as_deref()
                    .is_none_or(|unit_id| expected.unit_ids.iter().any(|id| id == unit_id))
                && validate_visual_page_for_spec(expected, page).is_ok()
                && validate_visual_page_unit(conn, expected, page).is_ok()
        }))
}

/// Adds a chat message and all of its citations without exposing a message that
/// has only a partial citation list.
pub(crate) fn insert_chat_message(
    conn: &mut Connection,
    message: &ChatMessage,
    citations: &[ChatCitation],
    thread_updated_at: u64,
) -> Result<()> {
    ensure!(
        citations
            .iter()
            .all(|citation| citation.message_id == message.id),
        "对话引用不属于待写入消息"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动对话消息事务")?;
    let thread = chat_threads::get(&tx, &message.thread_id)?.context("对话不存在")?;
    let scope = serde_json::from_str::<PersistedChatScope>(&thread.scope_json)
        .context("对话授权范围无效")?;
    for citation in citations {
        validate_current_chat_citation(&tx, citation, &scope.book_ids)?;
    }
    chat_messages::insert(&tx, message)?;
    for citation in citations {
        chat_citations::insert(&tx, citation)?;
    }
    if super::chat_threads::touch(&tx, &message.thread_id, thread_updated_at)? == 0 {
        bail!("对话不存在");
    }
    tx.commit().context("无法提交对话消息事务")?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct PersistedChatScope {
    book_ids: Vec<String>,
}

fn validate_current_chat_citation(
    conn: &Connection,
    citation: &ChatCitation,
    allowed_book_ids: &[String],
) -> Result<()> {
    // Web-sourced citations carry no book locator and must not be validated
    // against the current content tree.
    if citation.locator_json.is_none() {
        return Ok(());
    }
    let locator =
        serde_json::from_str::<DocumentLocator>(citation.locator_json.as_deref().unwrap())
            .context("对话引用定位信息无效")?;
    locator.validate().context("对话引用定位信息无效")?;
    ensure!(
        !matches!(
            locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        ),
        "Office 增强预览页不能持久化为对话引用"
    );
    ensure!(
        allowed_book_ids
            .iter()
            .any(|book_id| book_id == &locator.book_id),
        "对话引用超出当前授权范围"
    );
    let book = books::get(conn, &locator.book_id)?.context("对话引用图书已不存在")?;
    ensure!(
        book.revision == citation.document_revision,
        "对话引用的文档版本已失效"
    );
    let source = super::book_sources::get_revision(conn, &book.id, book.revision)?
        .context("对话引用的文档来源已不存在")?;
    let unit = content_units::get(conn, &locator.unit_id)?.context("对话引用的内容单元已不存在")?;
    ensure!(
        unit.book_id == book.id && unit.source_id == source.id,
        "对话引用的内容单元不属于当前文档版本"
    );
    ensure!(
        unit.revision == citation.unit_revision,
        "对话引用的内容单元版本已失效"
    );
    if let Some(content_unit_id) = citation.content_unit_id.as_deref() {
        ensure!(
            content_unit_id == locator.unit_id,
            "对话引用的内容单元与定位信息不一致"
        );
    }
    let blocks = serde_json::from_str::<BlockDocument>(&unit.block_json)
        .context("对话引用的内容单元结构无效")?;
    if let Some(block_id) = locator.block_id.as_deref() {
        let block = blocks
            .find_block(block_id)
            .context("对话引用的内容块已不存在")?;
        if let Some(range) = locator.text_range {
            let text = block.plain_text();
            let start = usize::try_from(range.start_byte).context("对话引用起始位置超出范围")?;
            let end = usize::try_from(range.end_byte).context("对话引用结束位置超出范围")?;
            ensure!(
                start <= end
                    && end <= text.len()
                    && text.is_char_boundary(start)
                    && text.is_char_boundary(end),
                "对话引用文本范围已失效"
            );
        }
    }
    if let Some(chunk_id) = citation.search_chunk_id.as_deref() {
        let chunk = search_chunks::get(conn, chunk_id)?.context("对话引用的搜索片段已不存在")?;
        let chunk_locator = serde_json::from_str::<DocumentLocator>(&chunk.locator_json)
            .context("对话引用的搜索片段定位信息无效")?;
        ensure!(
            chunk.book_id == book.id
                && chunk.source_id == source.id
                && chunk.content_unit_id == unit.id
                && chunk_locator == locator,
            "对话引用与当前搜索片段不一致"
        );
    }
    Ok(())
}

pub(crate) fn create_group(
    conn: &mut Connection,
    group: &BookGroup,
    max_depth: usize,
) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分组创建事务")?;
    if let Some(parent_id) = group.parent_id.as_deref() {
        let depth = groups::depth(&tx, parent_id, max_depth)?.context("父分组不存在")?;
        if depth >= max_depth {
            bail!("分组最多支持 {max_depth} 级");
        }
    }
    if groups::sibling_name_exists(&tx, None, &group.name, group.parent_id.as_deref())? {
        bail!("同级下已存在同名分组");
    }
    groups::insert(&tx, group)?;
    tx.commit().context("无法提交分组创建事务")?;
    Ok(())
}

pub(crate) fn rename_group(conn: &mut Connection, group_id: &str, name: &str) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分组重命名事务")?;
    let parent_id = groups::parent_id(&tx, group_id)?.context("分组不存在")?;
    if groups::sibling_name_exists(&tx, Some(group_id), name, parent_id.as_deref())? {
        bail!("同级下已存在同名分组");
    }
    if groups::rename(&tx, group_id, name)? == 0 {
        bail!("分组不存在");
    }
    tx.commit().context("无法提交分组重命名事务")?;
    Ok(())
}

pub(crate) fn delete_group_tree(conn: &mut Connection, root_group_id: &str) -> Result<Vec<String>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分组删除事务")?;
    let removed = groups::subtree_ids(&tx, root_group_id)?;
    if removed.is_empty() {
        bail!("分组不存在");
    }
    books::clear_groups_in_subtree(&tx, root_group_id)?;
    groups::delete_subtree(&tx, root_group_id)?;
    tx.commit().context("无法提交分组删除事务")?;
    Ok(removed)
}

pub(crate) fn set_book_group(
    conn: &Connection,
    book_id: &str,
    group_id: Option<&str>,
) -> Result<()> {
    if books::set_group(conn, book_id, group_id)? != 0 {
        return Ok(());
    }
    if !books::exists(conn, book_id)? {
        bail!("图书不存在");
    }
    bail!("分组不存在");
}

fn insert_blob_metadata(conn: &Connection, graph: &DocumentGraph<'_>) -> Result<()> {
    blobs::ensure_present(conn, graph.source_blob)?;
    for blob in graph.additional_blobs {
        blobs::ensure_present(conn, blob)?;
    }
    Ok(())
}

fn insert_document_children(conn: &Connection, graph: &DocumentGraph<'_>) -> Result<()> {
    for unit in graph.content_units {
        content_units::insert(conn, unit)?;
    }
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
fn enqueue_derivative_jobs(
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

fn visual_job_spec(graph: &DocumentGraph<'_>, job_id: String) -> VisualJobSpec {
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

fn validate_document_graph(graph: &DocumentGraph<'_>) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        book_search,
        chat_citations::{self, ChatCitation},
        chat_messages::{self, ChatMessage},
        chat_threads::{self, ChatThread},
        connection,
        embeddings::{self, Embedding},
        index_jobs::{self, IndexJob, IndexJobStatus},
        settings::{self, Setting},
    };

    #[derive(Clone)]
    struct Fixture {
        book: BookRecord,
        source_blob: BlobRecord,
        additional_blobs: Vec<BlobRecord>,
        source: BookSource,
        progress: ReadingProgress,
        units: Vec<ContentUnit>,
        toc: Vec<TocEntry>,
        assets: Vec<Asset>,
        refs: Vec<AssetRef>,
        chunks: Vec<SearchChunk>,
        pages: Vec<VisualPage>,
    }

    impl Fixture {
        fn new() -> Self {
            let source_blob = BlobRecord {
                object_key: "objects/source-1".to_string(),
                media_type: "application/epub+zip".to_string(),
                byte_len: 101,
                hash: "hash-source-1".to_string(),
                created_at: 1,
            };
            let cover_blob = BlobRecord {
                object_key: "objects/cover-1".to_string(),
                media_type: "image/png".to_string(),
                byte_len: 23,
                hash: "hash-cover-1".to_string(),
                created_at: 1,
            };
            let page_blob = BlobRecord {
                object_key: "objects/page-1".to_string(),
                media_type: "image/webp".to_string(),
                byte_len: 44,
                hash: "hash-page-1".to_string(),
                created_at: 1,
            };
            Self {
                book: BookRecord {
                    id: "book-1".to_string(),
                    title: "统一文档".to_string(),
                    author: "作者".to_string(),
                    language: Some("zh-CN".to_string()),
                    description: Some("测试文档".to_string()),
                    format: "epub".to_string(),
                    revision: 1,
                    source_object_key: source_blob.object_key.clone(),
                    cover_asset_id: Some("asset-cover".to_string()),
                    cover_object_key: Some(cover_blob.object_key.clone()),
                    cover_mime: Some(cover_blob.media_type.clone()),
                    added_at: 1,
                    updated_at: 1,
                    last_spine: 2,
                    group_id: None,
                },
                source: BookSource {
                    id: "source-1".to_string(),
                    book_id: "book-1".to_string(),
                    revision: 1,
                    format: "epub".to_string(),
                    source_kind: "original".to_string(),
                    object_key: source_blob.object_key.clone(),
                    source_name: Some("book.epub".to_string()),
                    created_at: 1,
                },
                progress: ReadingProgress {
                    book_id: "book-1".to_string(),
                    source_revision: 1,
                    content_unit_id: Some("unit-1".to_string()),
                    spine_index: 2,
                    locator_json: r#"{"spine":2}"#.to_string(),
                    fraction: 0.4,
                    updated_at: 2,
                },
                units: vec![ContentUnit {
                    id: "unit-1".to_string(),
                    book_id: "book-1".to_string(),
                    source_id: "source-1".to_string(),
                    parent_id: None,
                    ordinal: 2,
                    kind: "chapter".to_string(),
                    href: Some("EPUB/chapter.xhtml".to_string()),
                    source_locator_json: r#"{"href":"EPUB/chapter.xhtml"}"#.to_string(),
                    title: Some("第一章".to_string()),
                    media_type: Some("text/html".to_string()),
                    source_text: Some("<p>searchable document body</p>".to_string()),
                    block_json: serde_json::to_string(&BlockDocument::new(vec![
                        crate::document::Block::paragraph("block-1", "searchable document body"),
                    ]))
                    .unwrap(),
                    revision: 1,
                    created_at: 1,
                    updated_at: 1,
                }],
                toc: vec![TocEntry {
                    id: "toc-1".to_string(),
                    book_id: "book-1".to_string(),
                    source_id: "source-1".to_string(),
                    parent_id: None,
                    ordinal: 0,
                    depth: 0,
                    label: "第一章".to_string(),
                    href: Some("EPUB/chapter.xhtml".to_string()),
                    content_unit_id: "unit-1".to_string(),
                    target_block_id: Some("block-1".to_string()),
                    locator_json: r#"{"href":"EPUB/chapter.xhtml"}"#.to_string(),
                    created_at: 1,
                }],
                assets: vec![Asset {
                    id: "asset-cover".to_string(),
                    book_id: "book-1".to_string(),
                    source_id: "source-1".to_string(),
                    object_key: cover_blob.object_key.clone(),
                    kind: "cover".to_string(),
                    href: "EPUB/cover.png".to_string(),
                    media_type: cover_blob.media_type.clone(),
                    byte_len: cover_blob.byte_len,
                    width: Some(120),
                    height: Some(180),
                    created_at: 1,
                }],
                refs: vec![AssetRef {
                    id: "asset-ref-1".to_string(),
                    content_unit_id: "unit-1".to_string(),
                    asset_id: "asset-cover".to_string(),
                    relation: "image".to_string(),
                    ordinal: 0,
                    locator_json: serde_json::to_string(&DocumentLocator::block(
                        "book-1", "unit-1", "block-1",
                    ))
                    .unwrap(),
                    created_at: 1,
                }],
                chunks: vec![SearchChunk {
                    id: "chunk-1".to_string(),
                    book_id: "book-1".to_string(),
                    source_id: "source-1".to_string(),
                    content_unit_id: "unit-1".to_string(),
                    ordinal: 0,
                    heading: "第一章".to_string(),
                    body: "searchable document body".to_string(),
                    token_count: 3,
                    content_hash: "hash-chunk-1".to_string(),
                    locator_json: serde_json::to_string(&DocumentLocator::block(
                        "book-1", "unit-1", "block-1",
                    ))
                    .unwrap(),
                    created_at: 1,
                }],
                pages: vec![VisualPage {
                    id: "page-1".to_string(),
                    book_id: "book-1".to_string(),
                    source_id: "source-1".to_string(),
                    content_unit_id: Some("unit-1".to_string()),
                    page_index: 0,
                    object_key: page_blob.object_key.clone(),
                    width: 800,
                    height: 1200,
                    render_scale: 1.0,
                    renderer: "structural-svg".to_string(),
                    renderer_version: "1".to_string(),
                    document_revision: 1,
                    unit_revision: 1,
                    profile_id: "render-profile-test".to_string(),
                    fidelity: "structural".to_string(),
                    locator_json: r#"{"page":0}"#.to_string(),
                    created_at: 1,
                }],
                source_blob,
                additional_blobs: vec![cover_blob, page_blob],
            }
        }

        fn graph(&self) -> DocumentGraph<'_> {
            DocumentGraph {
                book: &self.book,
                source_blob: &self.source_blob,
                additional_blobs: &self.additional_blobs,
                source: &self.source,
                progress: Some(&self.progress),
                content_units: &self.units,
                toc_entries: &self.toc,
                assets: &self.assets,
                asset_refs: &self.refs,
                search_chunks: &self.chunks,
                visual_pages: &self.pages,
            }
        }

        fn revision_two(&self) -> Self {
            let mut next = self.clone();
            next.source_blob = BlobRecord {
                object_key: "objects/source-2".to_string(),
                media_type: "application/epub+zip".to_string(),
                byte_len: 111,
                hash: "hash-source-2".to_string(),
                created_at: 3,
            };
            next.book.revision = 2;
            next.book.source_object_key = next.source_blob.object_key.clone();
            next.book.updated_at = 3;
            next.source.id = "source-2".to_string();
            next.source.revision = 2;
            next.source.source_kind = "normalized".to_string();
            next.source.object_key = next.source_blob.object_key.clone();
            next.source.created_at = 3;
            next.progress.source_revision = 2;
            next.progress.updated_at = 3;
            for unit in &mut next.units {
                unit.source_id = next.source.id.clone();
                unit.revision = 2;
                unit.updated_at = 3;
            }
            for entry in &mut next.toc {
                entry.source_id = next.source.id.clone();
            }
            for asset in &mut next.assets {
                asset.source_id = next.source.id.clone();
            }
            for chunk in &mut next.chunks {
                chunk.source_id = next.source.id.clone();
                chunk.body = "searchable revised document body".to_string();
                chunk.content_hash = "hash-chunk-2".to_string();
            }
            for page in &mut next.pages {
                page.source_id = next.source.id.clone();
                page.document_revision = 2;
                page.unit_revision = 2;
            }
            next
        }
    }

    fn open_database() -> (tempfile::TempDir, Connection) {
        let temp = tempfile::tempdir().unwrap();
        let conn =
            connection::open_or_recreate(&temp.path().join(connection::DATABASE_FILE)).unwrap();
        (temp, conn)
    }

    #[test]
    fn translation_jobs_track_target_language_and_model_identity() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();

        // The fixture book is declared zh-CN, so a zh target has nothing to do.
        assert_eq!(
            reconfigure_translation_jobs(
                &mut conn,
                Some("zh-Hans"),
                "chat-1",
                "translation-v1:a",
                true,
                10
            )
            .unwrap(),
            0
        );
        assert!(
            index_jobs::list_by_kind(&conn, "translation")
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            reconfigure_translation_jobs(
                &mut conn,
                Some("en"),
                "chat-1",
                "translation-v1:a",
                true,
                11
            )
            .unwrap(),
            1
        );
        let job = index_jobs::get(&conn, "translation:source-1:en")
            .unwrap()
            .unwrap();
        assert_eq!(job.kind, "translation");
        assert_eq!(job.status, IndexJobStatus::Queued);
        let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).unwrap();
        assert_eq!(cursor.model.as_deref(), Some("chat-1"));
        assert_eq!(cursor.next_ordinal, 0);

        // An unchanged identity preserves the existing job and its progress.
        assert_eq!(
            reconfigure_translation_jobs(
                &mut conn,
                Some("en"),
                "chat-1",
                "translation-v1:a",
                true,
                12
            )
            .unwrap(),
            0
        );
        // A changed model restarts the job from the first block.
        assert_eq!(
            reconfigure_translation_jobs(
                &mut conn,
                Some("en"),
                "chat-2",
                "translation-v1:b",
                true,
                13
            )
            .unwrap(),
            1
        );
        let job = index_jobs::get(&conn, "translation:source-1:en")
            .unwrap()
            .unwrap();
        let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).unwrap();
        assert_eq!(cursor.model.as_deref(), Some("chat-2"));
        assert_eq!(cursor.next_ordinal, 0);

        // Switching the target supersedes the old language and adds the new one.
        assert_eq!(
            reconfigure_translation_jobs(
                &mut conn,
                Some("ja"),
                "chat-2",
                "translation-v1:b",
                true,
                14
            )
            .unwrap(),
            2
        );
        assert_eq!(
            index_jobs::get(&conn, "translation:source-1:en")
                .unwrap()
                .unwrap()
                .status,
            IndexJobStatus::Cancelled
        );
        assert_eq!(
            index_jobs::get(&conn, "translation:source-1:ja")
                .unwrap()
                .unwrap()
                .status,
            IndexJobStatus::Queued
        );

        // Disabling translation cancels every active job.
        assert_eq!(
            reconfigure_translation_jobs(&mut conn, None, "chat-2", "translation-v1:b", true, 15)
                .unwrap(),
            1
        );
        assert_eq!(
            index_jobs::get(&conn, "translation:source-1:ja")
                .unwrap()
                .unwrap()
                .status,
            IndexJobStatus::Cancelled
        );
    }

    #[test]
    fn translation_format_gate_excludes_fixed_layout_sources() {
        for format in ["epub", "mobi", "azw", "azw3", "doc", "docx"] {
            assert!(
                format_supports_translation(format),
                "{format} must translate"
            );
        }
        for format in ["pdf", "pptx", "xlsx"] {
            assert!(
                !format_supports_translation(format),
                "{format} is outside the current translation scope"
            );
        }
    }

    #[test]
    fn translation_jobs_skip_fixed_layout_formats() {
        let (_temp, mut conn) = open_database();
        let mut fixture = Fixture::new();
        fixture.book.format = "pdf".to_string();
        fixture.source.format = "pdf".to_string();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();

        assert_eq!(
            reconfigure_translation_jobs(
                &mut conn,
                Some("en"),
                "chat-1",
                "translation-v1:a",
                true,
                10
            )
            .unwrap(),
            0
        );
        assert!(
            index_jobs::list_by_kind(&conn, "translation")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn translation_jobs_start_paused_without_auto_run() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), false).unwrap();

        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-1",
            "translation-v1:a",
            false,
            10,
        )
        .unwrap();
        assert_eq!(
            index_jobs::get(&conn, "translation:source-1:en")
                .unwrap()
                .unwrap()
                .status,
            IndexJobStatus::Paused
        );
    }

    fn running_visual_job(conn: &Connection) -> VisualJobSpec {
        let job = index_jobs::get(conn, "visual-render:source-1")
            .unwrap()
            .unwrap();
        let (spec, completed_pages) = decode_persisted_visual_job(&job.cursor_json).unwrap();
        assert_eq!(completed_pages, 0);
        index_jobs::update_state(
            conn,
            &job.id,
            IndexJobStatus::Running,
            &job.cursor_json,
            None,
            10,
            Some(10),
            None,
        )
        .unwrap();
        spec
    }

    fn staged_visual_blob(suffix: &str, byte_len: u64) -> BlobRecord {
        BlobRecord {
            object_key: format!("objects/staged-{suffix}"),
            media_type: "image/png".to_string(),
            byte_len,
            hash: format!("hash-staged-{suffix}"),
            created_at: 10,
        }
    }

    fn staged_visual_page(
        fixture: &Fixture,
        spec: &VisualJobSpec,
        id: &str,
        page_index: usize,
        object_key: &str,
    ) -> VisualPage {
        VisualPage {
            id: id.to_string(),
            book_id: spec.book_id.clone(),
            source_id: spec.source_id.clone(),
            content_unit_id: Some(fixture.units[0].id.clone()),
            page_index,
            object_key: object_key.to_string(),
            width: 800,
            height: 1200,
            render_scale: spec.profile.scale(),
            renderer: spec.renderer.clone(),
            renderer_version: spec.renderer_version.clone(),
            document_revision: spec.document_revision.get(),
            unit_revision: fixture.units[0].revision,
            profile_id: spec.profile.stable_id(),
            fidelity: match spec.fidelity {
                crate::preview::RenderFidelity::Normalized => "normalized",
                crate::preview::RenderFidelity::Structural => "structural",
                crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
            }
            .to_string(),
            locator_json: serde_json::to_string(&DocumentLocator::unit(
                spec.book_id.clone(),
                fixture.units[0].id.clone(),
            ))
            .unwrap(),
            created_at: 10,
        }
    }

    #[test]
    fn preview_only_office_locator_cannot_be_committed_as_chat_citation() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let thread = ChatThread {
            id: "thread-preview-only".to_string(),
            book_id: Some(fixture.book.id.clone()),
            title: "Preview-only citation".to_string(),
            scope_json: r#"{"book_ids":["book-1"]}"#.to_string(),
            window_kind: "reader".to_string(),
            created_at: 2,
            updated_at: 2,
        };
        chat_threads::insert(&conn, &thread).unwrap();
        let message = ChatMessage {
            id: "message-preview-only".to_string(),
            thread_id: thread.id,
            parent_id: None,
            ordinal: 0,
            role: "assistant".to_string(),
            content: "must roll back".to_string(),
            model: Some("test-model".to_string()),
            created_at: 3,
        };
        let citation = ChatCitation {
            id: "citation-preview-only".to_string(),
            message_id: message.id.clone(),
            content_unit_id: Some(fixture.units[0].id.clone()),
            search_chunk_id: None,
            ordinal: 0,
            quote: "preview text".to_string(),
            document_revision: fixture.book.revision,
            unit_revision: fixture.units[0].revision,
            locator_json: Some(
                serde_json::to_string(
                    &DocumentLocator::unit(&fixture.book.id, &fixture.units[0].id)
                        .with_source(SourceLocator::office_rendered_page(1)),
                )
                .unwrap(),
            ),
            source_kind: "book".to_string(),
            url: None,
            source_title: None,
            created_at: 3,
        };

        let error = insert_chat_message(&mut conn, &message, std::slice::from_ref(&citation), 3)
            .expect_err("preview-only locator must be rejected at the commit boundary");
        assert!(error.to_string().contains("Office 增强预览页"));
        assert!(chat_messages::get(&conn, &message.id).unwrap().is_none());
        assert!(chat_citations::get(&conn, &citation.id).unwrap().is_none());
    }

    #[test]
    fn visual_page_checkpoints_are_atomic_and_publish_as_one_final_commit() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let spec = running_visual_job(&conn);
        let first_blob = staged_visual_blob("first", 17);
        let first_page = staged_visual_page(
            &fixture,
            &spec,
            "staged-page-first",
            0,
            &first_blob.object_key,
        );

        let empty = visual_page_staging::prefix_stats(&conn, &spec.id).unwrap();
        assert!(empty.is_contiguous_prefix(0));
        checkpoint_visual_page(&mut conn, &spec, &first_blob, &first_page, 1, 11).unwrap();
        let first_prefix = visual_page_staging::prefix_stats(&conn, &spec.id).unwrap();
        assert_eq!(first_prefix.page_count, 1);
        assert_eq!(first_prefix.min_page_index, Some(0));
        assert_eq!(first_prefix.max_page_index, Some(0));
        assert_eq!(first_prefix.total_bytes, first_blob.byte_len);
        assert!(first_prefix.is_contiguous_prefix(1));
        assert!(!blobs::is_unreferenced(&conn, &first_blob.object_key).unwrap());
        assert_eq!(
            visual_pages::list_for_source(&conn, &spec.source_id).unwrap(),
            fixture.pages,
            "a durable prefix must not expose a partial replacement"
        );

        // The blob row is inserted before the staging row in the transaction.
        // A duplicate page ID fails afterwards and must roll both metadata and
        // the cursor back to the one-page prefix.
        let rejected_blob = staged_visual_blob("rejected", 19);
        let rejected_page = staged_visual_page(
            &fixture,
            &spec,
            &first_page.id,
            1,
            &rejected_blob.object_key,
        );
        assert!(
            checkpoint_visual_page(&mut conn, &spec, &rejected_blob, &rejected_page, 2, 12)
                .is_err()
        );
        assert!(
            blobs::get(&conn, &rejected_blob.object_key)
                .unwrap()
                .is_none()
        );
        assert!(
            visual_page_staging::prefix_stats(&conn, &spec.id)
                .unwrap()
                .is_contiguous_prefix(1)
        );
        let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
        assert_eq!(decode_persisted_visual_job(&job.cursor_json).unwrap().1, 1);

        let second_blob = staged_visual_blob("second", 23);
        let second_page = staged_visual_page(
            &fixture,
            &spec,
            "staged-page-second",
            1,
            &second_blob.object_key,
        );
        checkpoint_visual_page(&mut conn, &spec, &second_blob, &second_page, 2, 13).unwrap();
        let complete = visual_page_staging::prefix_stats(&conn, &spec.id).unwrap();
        assert!(complete.is_contiguous_prefix(2));
        assert_eq!(complete.total_bytes, 40);

        let replaced = publish_staged_visual_pages(&mut conn, &spec, 2, 14).unwrap();
        assert_eq!(replaced, vec![fixture.additional_blobs[1].clone()]);
        assert!(
            visual_page_staging::prefix_stats(&conn, &spec.id)
                .unwrap()
                .is_contiguous_prefix(0)
        );
        let pages = visual_pages::list_for_source(&conn, &spec.source_id).unwrap();
        assert_eq!(
            pages.iter().map(|page| page.page_index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(pages.iter().all(|page| {
            page.document_revision == spec.document_revision.get()
                && page.renderer == spec.renderer
                && page.renderer_version == spec.renderer_version
        }));
        let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
        assert_eq!(job.status, IndexJobStatus::Succeeded);
        assert_eq!(decode_persisted_visual_job(&job.cursor_json).unwrap().1, 2);
        assert!(
            blobs::is_unreferenced(&conn, &fixture.additional_blobs[1].object_key).unwrap(),
            "the replaced final object must become reclaimable"
        );
    }

    #[test]
    fn visual_job_retry_discards_staging_before_resetting_cursor() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let spec = running_visual_job(&conn);
        let blob = staged_visual_blob("retry", 29);
        let page = staged_visual_page(&fixture, &spec, "staged-page-retry", 0, &blob.object_key);
        checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 11).unwrap();
        let cursor = encode_persisted_visual_job(spec.clone(), 1).unwrap();
        index_jobs::update_state(
            &conn,
            &spec.id,
            IndexJobStatus::Failed,
            &cursor,
            Some("test failure"),
            12,
            None,
            Some(12),
        )
        .unwrap();

        let reclaimable = clear_visual_page_staging(&mut conn, &spec).unwrap();
        assert_eq!(reclaimable, vec![blob.clone()]);
        assert!(
            visual_page_staging::prefix_stats(&conn, &spec.id)
                .unwrap()
                .is_contiguous_prefix(0)
        );
        let failed = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
        assert_eq!(failed.status, IndexJobStatus::Failed);
        assert_eq!(
            decode_persisted_visual_job(&failed.cursor_json).unwrap().1,
            1
        );
        let reset_cursor = encode_persisted_visual_job(spec.clone(), 0).unwrap();
        index_jobs::update_state(
            &conn,
            &spec.id,
            IndexJobStatus::Queued,
            &reset_cursor,
            None,
            13,
            None,
            None,
        )
        .unwrap();
        let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
        assert_eq!(job.status, IndexJobStatus::Queued);
        assert_eq!(decode_persisted_visual_job(&job.cursor_json).unwrap().1, 0);
        assert!(blobs::is_unreferenced(&conn, &blob.object_key).unwrap());
    }

    #[test]
    fn visual_publication_observes_last_page_pause_and_cancel_requests() {
        for cancel in [false, true] {
            let (_temp, mut conn) = open_database();
            let fixture = Fixture::new();
            insert_document(&mut conn, &fixture.graph(), true).unwrap();
            let spec = running_visual_job(&conn);
            let suffix = if cancel { "cancel-race" } else { "pause-race" };
            let blob = staged_visual_blob(suffix, 31);
            let page = staged_visual_page(
                &fixture,
                &spec,
                &format!("staged-page-{suffix}"),
                0,
                &blob.object_key,
            );
            checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 11).unwrap();
            if cancel {
                assert!(index_jobs::request_cancel(&conn, &spec.id, 12).unwrap() == 1);
            } else {
                assert!(index_jobs::request_pause(&conn, &spec.id, 12).unwrap() == 1);
            }

            assert!(publish_staged_visual_pages(&mut conn, &spec, 1, 13).is_err());
            assert_eq!(
                visual_pages::list_for_source(&conn, &spec.source_id).unwrap(),
                fixture.pages,
                "a final-page control request must keep the old visible set"
            );
            assert!(
                visual_page_staging::prefix_stats(&conn, &spec.id)
                    .unwrap()
                    .is_contiguous_prefix(1)
            );
            let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
            assert_eq!(job.status, IndexJobStatus::Running);
            assert_eq!(job.cancel_requested, cancel);
            assert_eq!(job.pause_requested, !cancel);
        }
    }

    #[test]
    fn document_and_supporting_crud_round_trip() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();

        let derivative_jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
        assert_eq!(derivative_jobs.len(), 3);
        assert!(derivative_jobs.iter().all(|job| {
            matches!(job.kind.as_str(), "embedding" | "vision" | "visual_render")
                && job.status == IndexJobStatus::Queued
                && job.source_id.as_deref() == Some("source-1")
        }));
        assert!(derivative_jobs.iter().all(|job| {
            serde_json::from_str::<serde_json::Value>(&job.cursor_json).is_ok_and(|cursor| {
                cursor["revision"] == 1 || cursor["spec"]["document_revision"] == 1
            })
        }));

        assert_eq!(
            books::get(&conn, "book-1").unwrap(),
            Some(fixture.book.clone())
        );
        assert_eq!(
            book_sources::get(&conn, "source-1").unwrap(),
            Some(fixture.source.clone())
        );
        assert_eq!(
            blobs::get(&conn, "objects/source-1").unwrap(),
            Some(fixture.source_blob.clone())
        );
        assert_eq!(
            progress::get(&conn, "book-1").unwrap(),
            Some(fixture.progress.clone())
        );
        assert_eq!(
            content_units::list_for_source(&conn, "source-1").unwrap(),
            fixture.units
        );
        assert_eq!(
            toc_entries::list_for_source(&conn, "source-1").unwrap(),
            fixture.toc
        );
        assert_eq!(
            assets::list_for_source(&conn, "source-1").unwrap(),
            fixture.assets
        );
        assert_eq!(
            asset_refs::list_for_unit(&conn, "unit-1").unwrap(),
            fixture.refs
        );
        assert_eq!(
            search_chunks::list_for_source(&conn, "source-1").unwrap(),
            fixture.chunks
        );
        assert_eq!(
            visual_pages::list_for_source(&conn, "source-1").unwrap(),
            fixture.pages
        );
        assert!(blobs::list_unreferenced(&conn).unwrap().is_empty());

        let hits = book_search::search(&conn, None, &["document".to_string()], 10).unwrap();
        assert!(hits.iter().any(|hit| hit.spine_index == Some(2)));

        let embedding = Embedding {
            id: "embedding-1".to_string(),
            search_chunk_id: "chunk-1".to_string(),
            model: "test-model".to_string(),
            dimensions: 2,
            vector: vec![0, 1, 2, 3, 4, 5, 6, 7],
            created_at: 2,
        };
        embeddings::upsert(&conn, &embedding).unwrap();
        assert_eq!(
            embeddings::get(&conn, "embedding-1").unwrap(),
            Some(embedding)
        );

        let job = IndexJob {
            id: "job-1".to_string(),
            book_id: "book-1".to_string(),
            source_id: Some("source-1".to_string()),
            kind: "embedding".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: "{}".to_string(),
            error: None,
            created_at: 2,
            updated_at: 2,
            started_at: None,
            finished_at: None,
        };
        index_jobs::insert(&conn, &job).unwrap();
        index_jobs::request_pause(&conn, "job-1", 3).unwrap();
        assert!(
            index_jobs::get(&conn, "job-1")
                .unwrap()
                .unwrap()
                .pause_requested
        );
        index_jobs::request_cancel(&conn, "job-1", 4).unwrap();
        let cancelled = index_jobs::get(&conn, "job-1").unwrap().unwrap();
        assert!(!cancelled.pause_requested);
        assert!(cancelled.cancel_requested);

        let thread = ChatThread {
            id: "thread-1".to_string(),
            book_id: Some("book-1".to_string()),
            title: "章节问答".to_string(),
            scope_json: r#"{"book_ids":["book-1"]}"#.to_string(),
            window_kind: "reader".to_string(),
            created_at: 2,
            updated_at: 2,
        };
        chat_threads::insert(&conn, &thread).unwrap();
        let message = ChatMessage {
            id: "message-1".to_string(),
            thread_id: thread.id.clone(),
            parent_id: None,
            ordinal: 0,
            role: "assistant".to_string(),
            content: "回答".to_string(),
            model: Some("test-model".to_string()),
            created_at: 3,
        };
        let citation = ChatCitation {
            id: "citation-1".to_string(),
            message_id: message.id.clone(),
            content_unit_id: Some("unit-1".to_string()),
            search_chunk_id: Some("chunk-1".to_string()),
            ordinal: 0,
            quote: "document body".to_string(),
            document_revision: 1,
            unit_revision: 1,
            locator_json: Some(fixture.chunks[0].locator_json.clone()),
            source_kind: "book".to_string(),
            url: None,
            source_title: None,
            created_at: 3,
        };
        insert_chat_message(&mut conn, &message, std::slice::from_ref(&citation), 3).unwrap();
        assert_eq!(
            chat_messages::list_for_thread(&conn, "thread-1").unwrap(),
            vec![message.clone()]
        );
        assert_eq!(
            chat_citations::list_for_message(&conn, "message-1").unwrap(),
            vec![citation.clone()]
        );
        assert_eq!(
            chat_threads::get(&conn, "thread-1")
                .unwrap()
                .unwrap()
                .updated_at,
            3
        );

        chat_threads::update_context(
            &conn,
            "thread-1",
            "章节问答",
            r#"{"book_ids":[]}"#,
            "reader",
            4,
        )
        .unwrap();
        let rejected_message = ChatMessage {
            id: "message-after-scope-change".to_string(),
            thread_id: thread.id.clone(),
            parent_id: Some(message.id.clone()),
            ordinal: 1,
            role: "assistant".to_string(),
            content: "不应写入".to_string(),
            model: Some("test-model".to_string()),
            created_at: 4,
        };
        let rejected_citation = ChatCitation {
            id: "citation-after-scope-change".to_string(),
            message_id: rejected_message.id.clone(),
            created_at: 4,
            ..citation.clone()
        };
        let error = insert_chat_message(
            &mut conn,
            &rejected_message,
            std::slice::from_ref(&rejected_citation),
            4,
        )
        .unwrap_err();
        assert!(error.to_string().contains("授权范围"));
        assert!(
            chat_messages::get(&conn, &rejected_message.id)
                .unwrap()
                .is_none(),
            "scope validation and message insertion must share one transaction"
        );
        assert!(
            chat_citations::get(&conn, &rejected_citation.id)
                .unwrap()
                .is_none()
        );

        let setting = Setting {
            key: "reader.theme".to_string(),
            value_json: r#"{"name":"paper"}"#.to_string(),
            updated_at: 2,
        };
        settings::upsert(&conn, &setting).unwrap();
        assert_eq!(settings::get(&conn, "reader.theme").unwrap(), Some(setting));
        assert!(crate::db::schema::is_current(&conn).unwrap());
    }

    #[test]
    fn deleting_a_mapped_unit_cascades_its_derived_visual_pages() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();

        conn.execute("DELETE FROM content_units WHERE id = ?1", ["unit-1"])
            .unwrap();

        assert!(
            visual_pages::list_for_source(&conn, "source-1")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn newer_revision_drops_old_derivative_jobs_and_queues_current_work() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let next = fixture.revision_two();
        let _ = install_document_revision(&mut conn, &next.graph(), true).unwrap();

        let jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
        assert_eq!(jobs.len(), 3);
        assert!(
            jobs.iter()
                .all(|job| job.source_id.as_deref() != Some("source-1"))
        );
        assert!(
            jobs.iter()
                .filter(|job| {
                    job.source_id.as_deref() == Some("source-2")
                        && job.status == IndexJobStatus::Queued
                })
                .count()
                == 3
        );
    }

    #[test]
    fn disabled_auto_run_pauses_import_and_replacement_derivative_jobs() {
        fn assert_paused_jobs(jobs: &[IndexJob], source_id: &str) {
            assert_eq!(jobs.len(), 3);
            assert!(jobs.iter().all(|job| {
                matches!(job.kind.as_str(), "embedding" | "vision" | "visual_render")
                    && job.status == IndexJobStatus::Paused
                    && job.source_id.as_deref() == Some(source_id)
                    && !job.pause_requested
                    && !job.cancel_requested
                    && job.attempts == 0
                    && job.started_at.is_none()
                    && job.finished_at.is_none()
            }));
        }

        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), false).unwrap();
        let imported_jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
        assert_paused_jobs(&imported_jobs, "source-1");
        let imported_job_ids = imported_jobs
            .iter()
            .map(|job| job.id.clone())
            .collect::<Vec<_>>();

        let next = fixture.revision_two();
        let _ = install_document_revision(&mut conn, &next.graph(), false).unwrap();
        let replacement_jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
        assert_paused_jobs(&replacement_jobs, "source-2");
        assert!(
            imported_job_ids
                .iter()
                .all(|job_id| index_jobs::get(&conn, job_id).unwrap().is_none()),
            "the superseded source must not retain paused derivative jobs"
        );
    }

    #[test]
    fn renderer_change_requeues_visual_vision_and_visual_embeddings_only() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let visual_job_id = "visual-render:source-1";
        let current_spec = running_visual_job(&conn);
        let staged_blob = staged_visual_blob("reconcile", 37);
        let staged_page = staged_visual_page(
            &fixture,
            &current_spec,
            "staged-page-reconcile",
            0,
            &staged_blob.object_key,
        );
        checkpoint_visual_page(&mut conn, &current_spec, &staged_blob, &staged_page, 1, 9).unwrap();
        let legacy_spec = VisualJobSpec {
            id: visual_job_id.to_string(),
            book_id: fixture.book.id.clone(),
            source_id: fixture.source.id.clone(),
            document_revision: Revision::new(fixture.book.revision),
            renderer: "moye-structural-svg".to_string(),
            renderer_version: "0.0.1".to_string(),
            fidelity: crate::preview::RenderFidelity::Structural,
            unit_ids: fixture.units.iter().map(|unit| unit.id.clone()).collect(),
            profile: RenderProfile::default(),
        };
        index_jobs::update_state(
            &conn,
            visual_job_id,
            IndexJobStatus::Succeeded,
            &encode_persisted_visual_job(legacy_spec, 1).unwrap(),
            None,
            10,
            Some(9),
            Some(10),
        )
        .unwrap();
        let visual_chunk = SearchChunk {
            id: "vision-chunk-old-renderer".to_string(),
            book_id: fixture.book.id.clone(),
            source_id: fixture.source.id.clone(),
            content_unit_id: fixture.units[0].id.clone(),
            ordinal: 1_000_000_000,
            heading: "旧视觉结果".to_string(),
            body: "stale OCR".to_string(),
            token_count: 2,
            content_hash: "stale-vision-hash".to_string(),
            locator_json: fixture.units[0].source_locator_json.clone(),
            created_at: 9,
        };
        search_chunks::insert(&conn, &visual_chunk).unwrap();
        let visual_embedding = Embedding {
            id: "embedding-old-vision".to_string(),
            search_chunk_id: visual_chunk.id.clone(),
            model: "embed-test".to_string(),
            dimensions: 1,
            vector: 1_f32.to_le_bytes().to_vec(),
            created_at: 9,
        };
        embeddings::upsert(&conn, &visual_embedding).unwrap();
        index_jobs::insert(
            &conn,
            &IndexJob {
                id: "post-vision-embedding-old".to_string(),
                book_id: fixture.book.id.clone(),
                source_id: Some(fixture.source.id.clone()),
                kind: "embedding".to_string(),
                status: IndexJobStatus::Succeeded,
                pause_requested: false,
                cancel_requested: false,
                attempts: 1,
                cursor_json: serde_json::json!({
                    "schema_version": 1,
                    "book_id": fixture.book.id,
                    "source_id": fixture.source.id,
                    "revision": fixture.book.revision,
                    "kind": "embedding",
                    "model": "embed-test",
                    "next_ordinal": 2,
                })
                .to_string(),
                error: None,
                created_at: 9,
                updated_at: 9,
                started_at: Some(9),
                finished_at: Some(9),
            },
        )
        .unwrap();

        let descriptor = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
        let reconciliation = reconcile_visual_render_jobs(
            &mut conn,
            std::slice::from_ref(&descriptor),
            &RenderProfile::default(),
            20,
        )
        .unwrap();
        assert_eq!(reconciliation.changed_sources, 1);
        assert_eq!(
            reconciliation.unreferenced_blobs,
            vec![fixture.additional_blobs[1].clone(), staged_blob]
        );

        let visual_job = index_jobs::get(&conn, visual_job_id).unwrap().unwrap();
        assert_eq!(visual_job.status, IndexJobStatus::Queued);
        let (spec, completed_pages) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
        assert_eq!(completed_pages, 0);
        assert_eq!(spec.renderer, "moye-structural-png");
        assert_eq!(spec.renderer_version, descriptor.version);
        assert!(
            visual_pages::list_for_source(&conn, "source-1")
                .unwrap()
                .is_empty()
        );
        assert!(
            search_chunks::get(&conn, &visual_chunk.id)
                .unwrap()
                .is_none()
        );
        assert!(
            embeddings::get(&conn, &visual_embedding.id)
                .unwrap()
                .is_none()
        );
        assert!(
            index_jobs::get(&conn, "post-vision-embedding-old")
                .unwrap()
                .is_none()
        );
        let vision = index_jobs::get(&conn, "vision:source-1").unwrap().unwrap();
        assert_eq!(vision.status, IndexJobStatus::Queued);
        let vision_cursor: serde_json::Value = serde_json::from_str(&vision.cursor_json).unwrap();
        assert_eq!(vision_cursor["next_ordinal"], 0);
        assert!(
            index_jobs::get(&conn, "embedding:source-1")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            reconcile_visual_render_jobs(
                &mut conn,
                std::slice::from_ref(&descriptor),
                &RenderProfile::default(),
                21,
            )
            .unwrap(),
            VisualRenderReconciliation::default()
        );

        let changed_profile = RenderProfile {
            name: "high-density".to_string(),
            scale_milli: 1_500,
            ..RenderProfile::default()
        };
        let reconciliation = reconcile_visual_render_jobs(
            &mut conn,
            std::slice::from_ref(&descriptor),
            &changed_profile,
            22,
        )
        .unwrap();
        assert_eq!(reconciliation.changed_sources, 1);
        assert!(reconciliation.unreferenced_blobs.is_empty());
        let visual_job = index_jobs::get(&conn, visual_job_id).unwrap().unwrap();
        let (spec, completed_pages) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
        assert_eq!(completed_pages, 0);
        assert_eq!(spec.profile, changed_profile);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn office_opt_in_is_atomic_and_selects_the_enhanced_renderer() {
        let (_temp, mut conn) = open_database();
        let mut fixture = Fixture::new();
        fixture.book.format = "pptx".to_string();
        fixture.source.format = "pptx".to_string();
        fixture.source_blob.media_type =
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();

        let structural = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
        let enhanced = RendererDescriptor {
            renderer: "moye-office-com-enhanced".to_string(),
            version: "test-office-renderer".to_string(),
            fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
        };
        let renderers = vec![structural.clone(), enhanced.clone()];
        let (source_id, reconciliation) = set_office_enhancement(
            &mut conn,
            &fixture.book.id,
            true,
            &renderers,
            &RenderProfile::default(),
            20,
        )
        .unwrap();

        assert_eq!(source_id, fixture.source.id);
        assert_eq!(reconciliation.changed_sources, 1);
        assert_eq!(
            reconciliation.unreferenced_blobs,
            vec![fixture.additional_blobs[1].clone()]
        );
        assert!(crate::db::office_enhancements::is_enabled(&conn, &fixture.book.id).unwrap());
        let visual_job = index_jobs::get(&conn, "visual-render:source-1")
            .unwrap()
            .unwrap();
        let (spec, completed_pages) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
        assert_eq!(completed_pages, 0);
        assert_eq!(spec.renderer, enhanced.renderer);
        assert_eq!(spec.renderer_version, enhanced.version);
        assert_eq!(
            spec.fidelity,
            crate::preview::RenderFidelity::OfficeEnhanced
        );
        assert!(
            visual_pages::list_for_source(&conn, &fixture.source.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            index_jobs::get(&conn, "vision:source-1")
                .unwrap()
                .unwrap()
                .status,
            IndexJobStatus::Queued
        );

        // Startup reconciliation must derive the same renderer solely from
        // the durable opt-in even if the visual job row was lost or stale.
        index_jobs::delete(&conn, "visual-render:source-1").unwrap();
        let startup =
            reconcile_visual_render_jobs(&mut conn, &renderers, &RenderProfile::default(), 20)
                .unwrap();
        assert_eq!(startup.changed_sources, 1);
        let startup_job = index_jobs::get(&conn, "visual-render:source-1")
            .unwrap()
            .unwrap();
        let (startup_spec, _) = decode_persisted_visual_job(&startup_job.cursor_json).unwrap();
        assert_eq!(startup_spec.renderer, enhanced.renderer);

        let (_, reconciliation) = set_office_enhancement(
            &mut conn,
            &fixture.book.id,
            false,
            &renderers,
            &RenderProfile::default(),
            21,
        )
        .unwrap();
        assert_eq!(reconciliation.changed_sources, 1);
        assert!(!crate::db::office_enhancements::is_enabled(&conn, &fixture.book.id).unwrap());
        let visual_job = index_jobs::get(&conn, "visual-render:source-1")
            .unwrap()
            .unwrap();
        let (spec, _) = decode_persisted_visual_job(&visual_job.cursor_json).unwrap();
        assert_eq!(spec.renderer, structural.renderer);
        assert_eq!(spec.fidelity, crate::preview::RenderFidelity::Structural);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn preview_only_office_pdf_with_fewer_pages_than_units_stays_current_after_reopen() {
        let (temp, mut conn) = open_database();
        let mut fixture = Fixture::new();
        fixture.book.format = "docx".to_string();
        fixture.source.format = "docx".to_string();
        fixture.source_blob.media_type =
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string();
        let mut second_unit = fixture.units[0].clone();
        second_unit.id = "unit-2".to_string();
        second_unit.ordinal = 3;
        second_unit.href = Some("word/section-2".to_string());
        second_unit.source_locator_json =
            serde_json::to_string(&SourceLocator::office_section(2)).unwrap();
        fixture.units.push(second_unit);
        insert_document(&mut conn, &fixture.graph(), true).unwrap();

        let structural = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
        let enhanced = RendererDescriptor {
            renderer: "moye-office-com-enhanced".to_string(),
            version: "test-office-renderer".to_string(),
            fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
        };
        let renderers = vec![structural, enhanced];
        set_office_enhancement(
            &mut conn,
            &fixture.book.id,
            true,
            &renderers,
            &RenderProfile::default(),
            20,
        )
        .unwrap();
        let spec = running_visual_job(&conn);
        assert_eq!(spec.unit_ids.len(), 2);

        let blob = staged_visual_blob("office-preview-only", 41);
        let page = VisualPage {
            id: "office-preview-page-1".to_string(),
            book_id: spec.book_id.clone(),
            source_id: spec.source_id.clone(),
            content_unit_id: None,
            page_index: 0,
            object_key: blob.object_key.clone(),
            width: 800,
            height: 1200,
            render_scale: spec.profile.scale(),
            renderer: spec.renderer.clone(),
            renderer_version: spec.renderer_version.clone(),
            document_revision: spec.document_revision.get(),
            unit_revision: Revision::INITIAL.get(),
            profile_id: spec.profile.stable_id(),
            fidelity: "office_enhanced".to_string(),
            locator_json: serde_json::to_string(
                &DocumentLocator::unit(
                    &spec.book_id,
                    office_preview_unit_id(&spec.book_id, &spec.source_id),
                )
                .with_source(SourceLocator::office_rendered_page(1)),
            )
            .unwrap(),
            created_at: 21,
        };
        conn.execute(
            "UPDATE book_sources SET format = 'pptx' WHERE id = ?1",
            [&spec.source_id],
        )
        .unwrap();
        assert!(
            checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 21).is_err(),
            "PPTX preview pages must retain exact slide-to-unit identity"
        );
        conn.execute(
            "UPDATE book_sources SET format = 'docx' WHERE id = ?1",
            [&spec.source_id],
        )
        .unwrap();
        checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 21).unwrap();
        publish_staged_visual_pages(&mut conn, &spec, 1, 22).unwrap();
        drop(conn);

        let database_path = temp.path().join(connection::DATABASE_FILE);
        let mut reopened = connection::open_or_recreate(&database_path).unwrap();
        assert_eq!(
            reconcile_visual_render_jobs(&mut reopened, &renderers, &RenderProfile::default(), 23,)
                .unwrap(),
            VisualRenderReconciliation::default()
        );
        let pages = visual_pages::list_for_source(&reopened, &spec.source_id).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].content_unit_id, None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn office_opt_in_rejects_non_office_sources_without_persisting_state() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let structural = crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer);
        let enhanced = RendererDescriptor {
            renderer: "moye-office-com-enhanced".to_string(),
            version: "test-office-renderer".to_string(),
            fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
        };

        assert!(
            set_office_enhancement(
                &mut conn,
                &fixture.book.id,
                true,
                &[structural.clone(), enhanced],
                &RenderProfile::default(),
                20,
            )
            .is_err()
        );
        assert!(
            crate::db::office_enhancements::get(&conn, &fixture.book.id)
                .unwrap()
                .is_none()
        );
        let job = index_jobs::get(&conn, "visual-render:source-1")
            .unwrap()
            .unwrap();
        let (spec, _) = decode_persisted_visual_job(&job.cursor_json).unwrap();
        assert_eq!(spec.renderer, structural.renderer);
    }

    #[test]
    fn document_import_rolls_back_every_table_on_child_failure() {
        let (_temp, mut conn) = open_database();
        let mut fixture = Fixture::new();
        let mut duplicate = fixture.chunks[0].clone();
        duplicate.id = "chunk-duplicate".to_string();
        fixture.chunks.push(duplicate);

        assert!(insert_document(&mut conn, &fixture.graph(), true).is_err());
        assert!(!books::exists(&conn, "book-1").unwrap());
        assert!(blobs::get(&conn, "objects/source-1").unwrap().is_none());
        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM search_chunks_fts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fts_count, 0);
    }

    #[test]
    fn revision_reuses_stable_child_ids_and_keeps_only_original_history() {
        let (_temp, mut conn) = open_database();
        let first = Fixture::new();
        insert_document(&mut conn, &first.graph(), true).unwrap();
        let current_spec = running_visual_job(&conn);
        let staged_blob = staged_visual_blob("old-source", 41);
        let staged_page = staged_visual_page(
            &first,
            &current_spec,
            "staged-page-old-source",
            0,
            &staged_blob.object_key,
        );
        checkpoint_visual_page(&mut conn, &current_spec, &staged_blob, &staged_page, 1, 2).unwrap();
        let second = first.revision_two();

        let unreferenced = install_document_revision(&mut conn, &second.graph(), true).unwrap();
        assert!(
            unreferenced
                .iter()
                .any(|blob| blob.object_key == staged_blob.object_key),
            "replacing a source must release its staged page objects"
        );

        let stored = books::get(&conn, "book-1").unwrap().unwrap();
        assert_eq!(stored.revision, 2);
        assert_eq!(stored.source_object_key, "objects/source-2");
        assert_eq!(
            book_sources::list_for_book(&conn, "book-1").unwrap().len(),
            2
        );
        assert!(
            content_units::list_for_source(&conn, "source-1")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            content_units::list_for_source(&conn, "source-2")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(search_chunks::count_for_book(&conn, "book-1").unwrap(), 1);
        assert!(crate::db::schema::is_current(&conn).unwrap());
    }

    #[test]
    fn deleting_document_cascades_graph_and_reports_unreferenced_objects() {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let spec = running_visual_job(&conn);
        let staged_blob = staged_visual_blob("deleted-book", 43);
        let staged_page = staged_visual_page(
            &fixture,
            &spec,
            "staged-page-deleted-book",
            0,
            &staged_blob.object_key,
        );
        checkpoint_visual_page(&mut conn, &spec, &staged_blob, &staged_page, 1, 2).unwrap();

        let mut unreferenced = delete_document(&mut conn, "book-1")
            .unwrap()
            .into_iter()
            .map(|blob| blob.object_key)
            .collect::<Vec<_>>();
        unreferenced.sort();
        assert_eq!(
            unreferenced,
            vec![
                "objects/cover-1",
                "objects/page-1",
                "objects/source-1",
                "objects/staged-deleted-book"
            ]
        );
        assert_eq!(search_chunks::count_for_book(&conn, "book-1").unwrap(), 0);
        assert!(crate::db::schema::is_current(&conn).unwrap());
    }
}
