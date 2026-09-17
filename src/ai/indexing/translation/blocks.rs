//! Extraction of the deterministic, revision-pinned translation block list.

use super::*;

use std::{collections::HashMap, sync::Arc};

use anyhow::{Result, ensure};

use crate::{
    ai::translation::{StoredTranslation, TranslationContext, TranslationSource},
    db,
    document::{BlockDocument, SourceLocator},
    storage::BlobKey,
};

pub(crate) async fn load_translation_blocks(
    inner: &Arc<IndexingInner>,
    book_id: &str,
    source_id: &str,
    revision: u64,
) -> Result<Vec<TranslationBlock>> {
    let source_id = source_id.to_string();
    let book_id = book_id.to_string();
    let source = {
        let source_id = source_id.clone();
        let book_id = book_id.clone();
        run_db_on(inner.runtime.clone(), inner.db_path.clone(), move |conn| {
            let source = db::book_sources::get(&conn, &source_id)?.context("翻译来源不存在")?;
            ensure!(
                source.book_id == book_id && source.revision == revision,
                "翻译来源与图书版本不匹配"
            );
            Ok(source)
        })
        .await?
    };
    let epub_bytes = if source.format == "epub" {
        Some(
            inner
                .blobs
                .get(&BlobKey::parse(&source.object_key)?)
                .await?,
        )
    } else {
        None
    };
    // 提取路径决定整个任务的文本块编号：EPUB 来源按阅读章节（spine）逐章解析，
    // 其它来源回落到 `content_units` 里的规范 HTML。两者对同一本书会给出不同的
    // 单元数和文本块数，所以必须先记录这次到底走了哪条路径。
    tracing::debug!(
        target: "ngy_ai",
        stage = "translation_source_resolved",
        book_id = %source.book_id,
        source_id = %source.id,
        revision = source.revision,
        format = %crate::ai::ai_diagnostics::safe_label(&source.format),
        source_kind = %crate::ai::ai_diagnostics::safe_label(&source.source_kind),
        object_key_bytes = source.object_key.len(),
        epub_path = epub_bytes.is_some(),
        epub_bytes = epub_bytes.as_ref().map(Vec::len),
        // 只有原件来源才逐章校验 source_locator 里的章节路径。
        check_original_href = source.source_kind == "original",
        "Translation source resolved"
    );
    // Both EPUB parsing and canonical HTML conversion are CPU work.
    // `run_db_on` executes this closure on spawn_blocking, away from the Tokio
    // workers, and locates the runtime from the coordinator instead of the
    // caller: this path is also reached from inspection APIs the UI awaits.
    run_db_on(inner.runtime.clone(), inner.db_path.clone(), move |conn| {
        if let Some(bytes) = epub_bytes {
            let opened = crate::reader::OpenedBook::open_bytes(bytes)?;
            translation_blocks_with_epub(
                &conn,
                &book_id,
                &source_id,
                Some((&opened, source.source_kind == "original")),
            )
        } else {
            translation_blocks(&conn, &book_id, &source_id)
        }
    })
    .await
}

pub(crate) fn translation_blocks(
    conn: &rusqlite::Connection,
    book_id: &str,
    source_id: &str,
) -> Result<Vec<TranslationBlock>> {
    translation_blocks_with_epub(conn, book_id, source_id, None)
}

pub(crate) fn translation_blocks_with_epub(
    conn: &rusqlite::Connection,
    book_id: &str,
    source_id: &str,
    epub: Option<(&crate::reader::OpenedBook, bool)>,
) -> Result<Vec<TranslationBlock>> {
    let units = db::content_units::list_for_source(conn, source_id)?;
    let unit_count = units.len();
    // 书名随每个文本块的请求一起发给模型，所以在这里读一次；读不到说明来源与图书
    // 已经不一致，宁可让任务失败，也不发一个没有书名的请求。
    let book_title = db::books::get(conn, book_id)?
        .context("翻译图书不存在")?
        .title;
    let spine_count = epub.map(|(opened, _)| opened.spine.len());
    if let Some((opened, _)) = epub {
        ensure!(
            opened.spine.len() == units.len(),
            "阅读章节与翻译图书结构不一致"
        );
    }
    tracing::debug!(
        target: "ngy_ai",
        stage = "translation_blocks_extraction_start",
        book_id = %book_id,
        source_id = %source_id,
        source_format = if epub.is_some() { "epub" } else { "canonical_html" },
        units = unit_count,
        spine = ?spine_count,
        // 只有原件来源才逐章校验 source_locator 里的章节路径。
        check_original_href = epub.is_some_and(|(_, check)| check),
        "Translation blocks extraction started"
    );
    let mut blocks = Vec::new();
    let mut ordinal = 0usize;
    let mut multi_segment_blocks = 0usize;
    let mut multi_segment_units = 0usize;
    for (unit_index, unit) in units.into_iter().enumerate() {
        ensure!(unit.book_id == book_id, "翻译文本块与图书不匹配");
        // Every chapter of the published source stays in the list, including a
        // chapter this revision did not change: the chapter's own revision
        // decides whether one of its blocks may reuse a stored译文, so the task
        // always describes the same whole book and never loses an untranslated
        // chapter to a revision filter.
        let sources = if let Some((opened, check_original_href)) = epub {
            ensure!(unit.ordinal == unit_index, "阅读章节顺序与翻译单元不一致");
            let href = &opened.spine[unit_index].href;
            if check_original_href {
                let locator =
                    serde_json::from_str::<Option<SourceLocator>>(&unit.source_locator_json)
                        .context("翻译单元原始定位信息无效")?;
                if let Some(SourceLocator::Epub { href: expected }) = locator {
                    let expected = crate::formats::normalized_archive_href(&expected)
                        .context("翻译单元原始章节路径无效")?;
                    ensure!(
                        Some(expected) == crate::formats::normalized_archive_href(href),
                        "翻译单元与阅读章节路径不匹配"
                    );
                }
            }
            let resource = crate::reader::load_resource(&opened.epub, href)?;
            let html = std::str::from_utf8(&resource.bytes).context("无法解码翻译阅读章节")?;
            crate::markup::translation_blocks_from_document_html(html)?
        } else {
            let document = serde_json::from_str::<BlockDocument>(&unit.block_json)
                .context("内容单元块结构无效，无法翻译")?;
            let html = crate::markup::serialize_source(&document)?;
            crate::markup::translation_blocks_from_html(&html)?
        };
        let unit_blocks = sources.len();
        // 一个片段就是一个可翻译的文字叶；片段数 > 1 的文本块要求模型逐片段作答，
        // 是本次翻译失败（弱模型把整段译文合并成一条）的唯一来源，所以按章统计。
        let unit_multi_segment = sources
            .iter()
            .filter(|source| source.segments.len() > 1)
            .count();
        let unit_segments = sources
            .iter()
            .map(|source| source.segments.len())
            .sum::<usize>();
        let unit_source_chars = sources
            .iter()
            .map(|source| source.text.chars().count())
            .sum::<usize>();
        let unit_title = unit.title.clone().unwrap_or_default();
        for (index, source) in sources.into_iter().enumerate() {
            // 请求体大小包含上下文，所以校验必须按模型实际收到的那一份来算。
            let context = TranslationContext::new(&book_title, &unit_title);
            validate_translation_source(&source, &context)?;
            ensure!(
                blocks.len() < MAX_TRANSLATION_BLOCKS,
                "图书文本块数量超过翻译上限"
            );
            blocks.push(TranslationBlock {
                book_title: book_title.clone(),
                unit_id: unit.id.clone(),
                unit_revision: unit.revision,
                unit_title: unit_title.clone(),
                unit_ordinal: unit.ordinal,
                block_id: format!("{}::h{index}", unit.id),
                ordinal,
                source,
            });
            ordinal += 1;
        }
        tracing::debug!(
            target: "ngy_ai",
            stage = "translation_unit_extracted",
            unit_ordinal = unit.ordinal,
            translatable_blocks = unit_blocks,
            segments = unit_segments,
            multi_segment_blocks = unit_multi_segment,
            source_chars = unit_source_chars,
            "Translation unit extracted"
        );
        multi_segment_blocks += unit_multi_segment;
        if unit_multi_segment > 0 {
            multi_segment_units += 1;
        }
    }
    tracing::debug!(
        target: "ngy_ai",
        stage = "translation_blocks_extracted",
        book_id = %book_id,
        source_id = %source_id,
        source_format = if epub.is_some() { "epub" } else { "canonical_html" },
        units = unit_count,
        spine = ?spine_count,
        translatable_blocks = blocks.len(),
        multi_segment_blocks,
        multi_segment_units,
        "Translation blocks extracted"
    );
    Ok(blocks)
}

pub(crate) fn validate_translation_source(
    source: &TranslationSource,
    context: &TranslationContext<'_>,
) -> Result<()> {
    let raw_chars = source.segments.iter().try_fold(0usize, |total, segment| {
        let next = total.saturating_add(segment.chars().count());
        ensure!(
            next <= MAX_TRANSLATION_SOURCE_CHARS,
            "段落超过翻译长度上限，请先拆分长段落后重试"
        );
        Ok(next)
    })?;
    ensure!(
        raw_chars != 0 && source.text.chars().count() <= MAX_TRANSLATION_SOURCE_CHARS,
        "段落超过翻译长度上限或没有可翻译文字，请先拆分长段落后重试"
    );
    ensure!(
        source.request_input(context).len() <= MAX_TRANSLATION_REQUEST_BYTES,
        "段落格式片段超过翻译请求大小上限，请先拆分长段落后重试"
    );
    Ok(())
}

/// Collapse whitespace and truncate one block for the task window preview.
/// The result is display-only data derived from the pinned revision; it is
/// never persisted or written to diagnostics.
pub(crate) fn translation_block_preview(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= TRANSLATION_BLOCK_PREVIEW_CHARS {
        return normalized;
    }
    let mut preview = normalized
        .chars()
        .take(TRANSLATION_BLOCK_PREVIEW_CHARS)
        .collect::<String>();
    preview.push('…');
    preview
}

/// Identity of one translatable block decision. Besides the block text and its
/// leaf split the model also sees the book and chapter context, but that context
/// is constant for every block of one unit — and a stored row is only ever
/// looked up inside its own unit — so the key stays text + leaf split: a stored
/// row may be reused for any block with the same key, even when an edit (or the
/// normalized EPUB projection, which adds the chapter title heading) moved the
/// block to another position.
pub(crate) fn translation_cache_key<'a>(
    source_text: &str,
    segments: impl Iterator<Item = &'a str>,
) -> String {
    let mut key = String::from(source_text);
    for segment in segments {
        key.push('\u{1f}');
        key.push_str(segment);
    }
    key
}

/// Stored translations of one chapter that are still current, keyed by
/// [`translation_cache_key`]. Keying on the text and its leaf split instead of
/// the block position keeps every paragraph that an edit did not change
/// reusable, while two blocks that only look alike are still translated
/// separately.
#[allow(clippy::too_many_arguments)]
pub(crate) fn current_translation_blocks(
    conn: &rusqlite::Connection,
    book_id: &str,
    unit_id: &str,
    target_language: &str,
    document_revision: u64,
    unit_revision: u64,
    model: &str,
    execution_identity: &str,
) -> Result<HashMap<String, StoredTranslation>> {
    let _ = book_id;
    let mut stored = HashMap::new();
    for row in db::translations::list_for_unit(conn, unit_id, target_language)? {
        if row.document_revision != document_revision
            || row.unit_revision != unit_revision
            || row.model != model
        {
            continue;
        }
        let Ok(translation) = serde_json::from_str::<StoredTranslation>(&row.translated_text)
        else {
            continue;
        };
        if translation.execution_identity != execution_identity {
            continue;
        }
        let key = translation_cache_key(
            &row.source_text,
            translation
                .segments
                .iter()
                .map(|segment| segment.source.as_str()),
        );
        stored.insert(key, translation);
    }
    Ok(stored)
}
