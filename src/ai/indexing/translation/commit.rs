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
) -> Result<CommittedBlock> {
    let block = &blocks[ordinal];
    let committed = match slot {
        BlockSlot::Pass => CommittedBlock::Untouched,
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
            CommittedBlock::Translated
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
            CommittedBlock::Translated
        }
        BlockSlot::Untranslated(cause, error_kind) => {
            *untranslated_blocks += 1;
            stats.untranslated += 1;
            // 事件按原因分开：协议失败与「请求根本没答上来」是两句不同的话，用户据此
            // 判断该换模型还是该查端点。块号与固定分类两者都记，任务窗口据此把这一块
            // 列进「失败」，用户点「重试」时重发的就是它。
            let (event, stage) = match cause {
                SkipCause::Protocol => (
                    JobLogEvent::ProtocolSkipped,
                    "translation_block_untranslated",
                ),
                SkipCause::Request => (
                    JobLogEvent::ProviderSkipped,
                    "translation_block_untranslated_request",
                ),
            };
            record_index_event(
                inner,
                job,
                event,
                JobLogMetrics {
                    ordinal: Some(block.ordinal as u64),
                    total: Some(blocks.len() as u64),
                    // 协议失败用满了一整块的尝试预算；请求级失败在第一发就放弃了。
                    request_attempt: (cause == SkipCause::Protocol)
                        .then_some(TRANSLATION_RESPONSE_ATTEMPTS as u64),
                    error_kind: Some(error_kind),
                    ..JobLogMetrics::default()
                },
            );
            tracing::warn!(
                target: "ngy_ai",
                stage = stage,
                block_ordinal = block.ordinal,
                total_blocks = blocks.len(),
                cause = match cause {
                    SkipCause::Protocol => "protocol",
                    SkipCause::Request => "request",
                },
                error_kind = error_kind.label(),
                untranslated = *untranslated_blocks,
                "Translation block left untranslated"
            );
            // A block the model never answered leaves no row and the walk goes
            // past it: the whole book has to reach the end even if every block
            // fails, because the per-block "重试" button is the repair path, not a
            // second attempt at the whole run. Stopping early would strand every
            // later block —— 请求级失败也一样，连续多少个都不停（2026-09-24 用户
            // 裁定，中止条件已删除；连续失败凑满
            // `TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT` 只告警并冷却一次）。
            CommittedBlock::Skipped(cause, error_kind)
        }
    };
    run.commit(inner, job, ordinal).await?;
    Ok(committed)
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
