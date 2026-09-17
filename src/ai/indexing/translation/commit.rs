//! Committing one finished block and publishing a finished run.

use super::*;

/// Commits one finished block in ordinal order.
///
/// Everything a block needs to be published — the stored row, the diagnostics and
/// the counters — is applied here and nowhere else, so the durable cursor and the
/// task window can never disagree about what has been done.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn commit_translation_slot(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    source_language: &Option<String>,
    run: &mut TranslationRun,
    blocks: &[TranslationBlock],
    ordinal: usize,
    slot: BlockSlot,
    unit_cache: &mut HashMap<String, HashMap<String, StoredTranslation>>,
    stats: &mut TranslationRunStats,
    saved_blocks: &mut usize,
    untranslated_blocks: &mut usize,
) -> Result<()> {
    let block = &blocks[ordinal];
    match slot {
        BlockSlot::Pass => {}
        BlockSlot::Cached(stored) => {
            publish_translation(
                inner,
                job,
                translation,
                target_language,
                source_language,
                run,
                block,
                &stored,
            )
            .await?;
            keep_in_unit_cache(unit_cache, block, stored);
            stats.cached += 1;
            record_index_event(
                inner,
                job,
                JobLogEvent::CacheHit,
                JobLogMetrics {
                    ordinal: Some(block.ordinal as u64),
                    actual_count: Some(block.source.segments.len() as u64),
                    ..JobLogMetrics::default()
                },
            );
        }
        BlockSlot::Translated(stored, block_stats) => {
            publish_translation(
                inner,
                job,
                translation,
                target_language,
                source_language,
                run,
                block,
                &stored,
            )
            .await?;
            let segments = stored.segments.len();
            keep_in_unit_cache(unit_cache, block, stored);
            *saved_blocks += 1;
            stats.saved += 1;
            stats.fragment_fallback_blocks += block_stats.fragment_fallback_blocks;
            stats.fragment_fallback_requests += block_stats.fragment_fallback_requests;
            record_index_event(
                inner,
                job,
                JobLogEvent::ItemSaved,
                JobLogMetrics {
                    ordinal: Some(block.ordinal as u64),
                    total: Some(blocks.len() as u64),
                    actual_count: Some(segments as u64),
                    duration_ms: Some(block_stats.duration_ms),
                    ..JobLogMetrics::default()
                },
            );
            tracing::debug!(
                target: "ngy_ai",
                stage = "translation_block_saved",
                block_ordinal = block.ordinal,
                segments,
                next_ordinal = ordinal + 1,
                duration_ms = block_stats.duration_ms,
                saved_blocks = *saved_blocks,
                untranslated_blocks = *untranslated_blocks,
                "Translation block saved"
            );
        }
        BlockSlot::Untranslated(error_kind) => {
            *untranslated_blocks += 1;
            stats.untranslated += 1;
            record_index_event(
                inner,
                job,
                JobLogEvent::ProtocolSkipped,
                JobLogMetrics {
                    ordinal: Some(block.ordinal as u64),
                    total: Some(blocks.len() as u64),
                    request_attempt: Some(TRANSLATION_RESPONSE_ATTEMPTS as u64),
                    error_kind: Some(error_kind),
                    ..JobLogMetrics::default()
                },
            );
            tracing::warn!(
                target: "ngy_ai",
                stage = "translation_block_untranslated",
                block_ordinal = block.ordinal,
                total_blocks = blocks.len(),
                error_kind = error_kind.label(),
                untranslated = *untranslated_blocks,
                "Translation block left untranslated"
            );
            // A block the model never answered leaves no row and the walk goes
            // past it: the whole book has to reach the end even if every block
            // fails, because the per-block "重试" button is the repair path, not a
            // second attempt at the whole run. Stopping early here would also
            // strand every later block.
        }
    }
    run.commit(inner, job, ordinal).await
}

/// Writes one block's translation row. The stored text is republished under the
/// block's current id: an edit may move a paragraph without changing it, and the
/// row has to follow the block it belongs to instead of holding a stale position.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn publish_translation(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    source_language: &Option<String>,
    run: &TranslationRun,
    block: &TranslationBlock,
    stored: &StoredTranslation,
) -> Result<()> {
    let now = unix_timestamp()?;
    let entry = db::translations::NewTranslation {
        book_id: run.cursor.book_id.clone(),
        content_unit_id: block.unit_id.clone(),
        block_id: block.block_id.clone(),
        ordinal: block.ordinal as u64,
        document_revision: run.cursor.revision,
        unit_revision: block.unit_revision,
        target_language: target_language.to_string(),
        source_language: source_language.clone(),
        model: translation.model.clone(),
        source_text: block.source.text.clone(),
        translated_text: serde_json::to_string(stored)?,
        created_at: now,
        updated_at: now,
    };
    let db_path = inner.db_path.clone();
    run_db(db_path, move |conn| db::translations::upsert(&conn, &entry)).await?;
    let _ = job;
    Ok(())
}
