//! Deciding what one ordinal of the window needs, and the per-run caches that
//! keep already-translated text out of the provider.

use super::*;

/// 这次运行接下来该用的翻译服务：库里最新的一份，前提是它属于同一套引擎。
///
/// `None` 表示保持现在这一份：没有配置翻译，或者模型、执行身份、目标语言里有一项与本次
/// 运行不同。后者（改地址、换模型或换目标语言）不是「换一份凭据」，而是一次会重置任务行的
/// 重配置：本次运行会在下一个控制检查里被判成已被替换（`Abandoned`），重新领取时才会按新
/// 引擎继续；绝不能在这里悄悄换身份，把两种来源的译文混进同一次运行。
///
/// 同一套引擎下换 provider（例如只换了 API key 或超时）或换生成参数必须立刻生效：这正是
/// 用户保存 Endpoint 设置之后期望看到的行为（现场 2026-09-24：改完 Endpoint 与 API key，
/// 正在跑的任务仍用旧值翻完剩下的块）。
pub(crate) fn refreshed_translation_services(
    inner: &Arc<IndexingInner>,
    current: &TranslationServices,
) -> Option<TranslationServices> {
    let fresh = inner.models_snapshot().translation?;
    let same_engine = fresh.model == current.model
        && fresh.execution_identity == current.execution_identity
        && fresh.target_language == current.target_language;
    same_engine.then_some(fresh)
}

/// Remembers one committed translation so a later block with the same text hits it
/// instead of the provider, exactly like the cache rows read from the database.
pub(crate) fn keep_in_unit_cache(
    unit_cache: &mut HashMap<String, HashMap<String, StoredTranslation>>,
    block: &TranslationBlock,
    stored: StoredTranslation,
) {
    unit_cache
        .entry(block.unit_id.clone())
        .or_default()
        .insert(block_cache_key(&block.source), stored);
}

fn block_cache_key(source: &TranslationSource) -> String {
    translation_cache_key(&source.text, source.segments.iter().map(String::as_str))
}

/// 一个块的失败该跳过还是该让整次运行失败。
///
/// `Some(..)` 表示这一块按跳过处理，原文保留、游标前进、后面的块照常翻：
///
/// - `SkipCause::Protocol`：模型答了、但形状不合格（`ResponseError`）；
/// - `SkipCause::Request`：这一发请求/这一份响应不行 —— 传输、超时、流协议、上下文
///   超限，以及「响应超过大小上限」这类没有更细分类的响应级错误。
///
/// `None` 表示不是某一块的毛病，照旧让整次运行失败：数据库、IO 与取消类错误跳过只会
/// 把损坏的结果铺满整本书，重试或换模型也解决不了它们。
pub(crate) fn block_skip_cause(error: &anyhow::Error) -> Option<(SkipCause, JobLogErrorKind)> {
    if error
        .downcast_ref::<crate::ai::translation::ResponseError>()
        .is_some()
    {
        return Some((SkipCause::Protocol, classify_error(error)));
    }
    let kind = classify_error(error);
    if matches!(
        kind,
        JobLogErrorKind::Database | JobLogErrorKind::Io | JobLogErrorKind::Cancelled
    ) {
        return None;
    }
    Some((SkipCause::Request, kind))
}

/// Decides what one ordinal of the window needs.
///
/// A block whose text was translated before is republished instead of being sent
/// again: the lookup is by content inside the block's own unit, so a paragraph an
/// edit moved (new block id, same text) still finds its row. The cache is read as
/// it is when the block enters the window, so two identical blocks inside one
/// window are both sent — the second one just writes the same text over its own
/// row.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_translation_block(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    book_id: &str,
    translation: &TranslationServices,
    target_language: &str,
    blocks: &[TranslationBlock],
    ordinal: usize,
    revision: u64,
    retry_targets: &HashSet<usize>,
    retry_last: Option<usize>,
    force_retry: bool,
    unit_cache: &mut HashMap<String, HashMap<String, StoredTranslation>>,
    stats: &mut TranslationRunStats,
) -> Result<BlockPlan> {
    // 出了本次重试范围（最后一个目标之后）的块不能再被「跳过」：跳过它等于把它记成
    // 已处理，而它可能从来没有译文。那里只允许走缓存命中，没有译文就由调用方停下运行。
    let beyond_retry = retry_last.is_some_and(|last| ordinal > last);
    // Not part of a per-block retry, and still inside the retried span: walk past it
    // without a provider request. Its stored translation is untouched, so nothing has
    // to be republished.
    if !retry_targets.is_empty() && !retry_targets.contains(&ordinal) && !beyond_retry {
        return Ok(BlockPlan::Pass);
    }
    let block = &blocks[ordinal];
    // 范围之外的块不是这次运行「开始处理」的块：不许留下 item_started 这种读数。
    if !beyond_retry {
        record_index_event(
            inner,
            job,
            JobLogEvent::ItemStarted,
            JobLogMetrics {
                ordinal: Some(block.ordinal as u64),
                total: Some(blocks.len() as u64),
                expected_count: Some(block.source.segments.len() as u64),
                ..JobLogMetrics::default()
            },
        );
        tracing::debug!(
            target: "ngy_ai",
            stage = "translation_block_start",
            block_ordinal = block.ordinal,
            block_id = %block.block_id,
            total_blocks = blocks.len(),
            unit_ordinal = block.unit_ordinal,
            segments = block.source.segments.len(),
            multi_segment = block.source.segments.len() > 1,
            segment_chars = %translation_segment_chars(&block.source.segments),
            source_chars = block.source.text.chars().count(),
            request_bytes = block.source.request_input(&block.translation_context()).len(),
            "Translation block started"
        );
    }
    if block.source.segments.is_empty() {
        tracing::debug!(
            target: "ngy_ai",
            stage = "translation_block_skipped",
            block_ordinal = block.ordinal,
            reason = "no_translatable_segments",
            "Translation block skipped"
        );
        stats.no_segments += 1;
        record_index_event(
            inner,
            job,
            JobLogEvent::NoTranslatableSegments,
            JobLogMetrics {
                ordinal: Some(block.ordinal as u64),
                actual_count: Some(0),
                ..JobLogMetrics::default()
            },
        );
        return Ok(BlockPlan::NoSegments);
    }
    if !unit_cache.contains_key(&block.unit_id) {
        let unit_id = block.unit_id.clone();
        let unit_revision = block.unit_revision;
        let language = target_language.to_string();
        let model = translation.model.clone();
        let execution_identity = translation.execution_identity.clone();
        let book_id = book_id.to_string();
        let db_path = inner.db_path.clone();
        let stored = run_db(db_path, move |conn| {
            current_translation_blocks(
                &conn,
                &book_id,
                &unit_id,
                &language,
                revision,
                unit_revision,
                &model,
                &execution_identity,
            )
        })
        .await?;
        unit_cache.insert(block.unit_id.clone(), stored);
    }
    let cache_key = block_cache_key(&block.source);
    let cached = unit_cache
        .get(&block.unit_id)
        .and_then(|unit| unit.get(&cache_key))
        .cloned();
    // 阅读端逐块「重新翻译」把这一块标成强制重发：命中缓存也要重新请求一次，新译文
    // 覆盖旧行（用户对译文不满意、或换了更好的模型重做这一块）。
    let forced = force_retry && retry_targets.contains(&ordinal);
    match &cached {
        Some(stored) if !forced && !stored.is_passthrough() => {
            return Ok(BlockPlan::Cached(Box::new(stored.clone())));
        }
        Some(_) if !forced => {
            // 模型把整块原样抄了回来：这一块其实还没有译文（现场 2026-10-15，阅读端
            // 显示的仍是英文）。它不再算缓存命中，继续往下走请求路径重新翻一次。
            // 只在本次运行真的要处理它时留读数：重试范围之外的块既不请求也不记录。
            if !beyond_retry {
                tracing::info!(
                    target: "ngy_ai",
                    stage = "translation_source_echoed",
                    block_ordinal = block.ordinal,
                    "Model echoed the source text back; treating the block as untranslated"
                );
                record_index_event(
                    inner,
                    job,
                    JobLogEvent::SourceEchoed,
                    JobLogMetrics {
                        ordinal: Some(block.ordinal as u64),
                        ..JobLogMetrics::default()
                    },
                );
            }
        }
        _ => {}
    }
    if beyond_retry {
        // 不在本次重试里、又没有译文：既不发请求，也不许提交。调用方会停在
        // 这一块上，任务因此留在「还没翻完、可以继续」的状态。
        tracing::info!(
            target: "ngy_ai",
            stage = "translation_retry_stop_block",
            block_ordinal = block.ordinal,
            total_blocks = blocks.len(),
            "Narrowed retry reached an untranslated block outside its targets"
        );
        return Ok(BlockPlan::Untouched);
    }
    Ok(BlockPlan::Request)
}
