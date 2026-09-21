//! The last-resort fallback: one request per text leaf.

use super::*;

use std::{sync::Arc, time::Instant};

use anyhow::Result;

use crate::{
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics},
    ai::translation::{StoredTranslation, TranslationContext, TranslationSource},
    db,
};

/// 逐片段回退的适用条件。只有「模型答了、但片段不完整」这一类失败才回退：此时单个
/// 片段请求几乎必定通过（只要模型还会翻译）。JSON 层面的失败说明模型根本没写出答案，
/// 换请求形状解决不了；超过请求上限的文本块也不再回退，直接沿用原来的跳过规则。
///
/// `rejected_kinds` 是这一块**每一次尝试**的失败类别，按发生顺序排列，跨整块重发的轮次
/// 累积。判定放在整块上而不是最后一次：纠正提示与重发会改变错误的形状。现场日志里
/// 1786/1787 两块就是 attempt 1 合并成一条（`segment_count_mismatch`，
/// `merged_into_one=true`）、attempt 2 被纠正成「开始按段写」但 JSON 没闭合
/// （`containers=0 open_container=true segment_markers=3`，即恰好等于期望段数）。这两次
/// 描述的是同一个能力上限 —— 模型写不完多段 JSON —— 而逐片段请求正是它能满足的形状。只看
/// 最后一次会把 `invalid_json` / `incomplete_json` 当成「没写出答案」而放弃回退，让本可救回
/// 的块白白跳过（块级重发又会把这种形状推到后面的轮次里）。任一尝试落在可回退类别即回退。
pub(crate) fn fragment_fallback_applies(
    source: &TranslationSource,
    rejected_kinds: &[&str],
) -> bool {
    source.segments.len() > 1
        && source.segments.len() <= MAX_TRANSLATION_FRAGMENT_FALLBACKS
        && rejected_kinds.iter().any(|kind| {
            matches!(
                *kind,
                "segment_count_mismatch"
                    | "missing_segment_id"
                    | "unknown_segment_id"
                    | "duplicate_segment_id"
                    | "empty_segment_text"
                    | "invalid_segment_text"
                    | "keep_with_text"
                    | "positional_segments"
            )
        })
}

/// 逐片段回退：每个文字叶单独发一次请求，`source` 和 `segments` 都只包含该片段本身。
/// 模型因此没有任何可以越界翻译的内容，而校验仍是主路径那套严格解码（单片段只需一个
/// id=0 的答案）。结果按原顺序拼成一条完整的 `StoredTranslation`，片段边界的空白由
/// `parse_response` 从片段自身恢复，与主路径写入的行结构完全一致，缓存键也仍按整个
/// 文本块的原文计算。
///
/// 任何一次请求失败都会让整个文本块按原来的规则跳过（不保存部分译文），只有非协议
/// 失败（provider、数据库、取消）继续让整次运行失败。
pub(crate) async fn translate_block_by_fragment(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    scope: &TranslationBlockScope,
    translation: &TranslationServices,
    target_language: &str,
    source_language: Option<&str>,
    source: &TranslationSource,
    context: TranslationContext<'_>,
) -> Result<BlockWalk> {
    let fragments = source.segments.len();
    let started = Instant::now();
    // The fallback counts its own requests; the caller owns the block's totals.
    let mut stats = BlockStats::default();
    tracing::info!(
        target: "ngy_ai",
        stage = "translation_fragment_fallback_start",
        block_ordinal = scope.ordinal,
        fragments,
        source_chars = source.text.chars().count(),
        "Translation block retried one fragment at a time"
    );
    record_index_event(
        inner,
        job,
        JobLogEvent::SegmentFallback,
        JobLogMetrics {
            ordinal: Some(scope.ordinal as u64),
            request_attempt: Some(TRANSLATION_RESPONSE_ATTEMPTS as u64 + 1),
            expected_count: Some(fragments as u64),
            ..JobLogMetrics::default()
        },
    );
    let mut translated = Vec::with_capacity(fragments);
    for (index, fragment_text) in source.segments.iter().enumerate() {
        {
            let _transition = inner.transitions.lock().await;
            if let Some(stop) = block_stop_request(inner, job, &scope.committed_json()).await? {
                return Ok(BlockWalk::Stopped(stop));
            }
            if !source_is_current(inner, job, scope.revision).await? {
                return Ok(BlockWalk::Stopped(StopRequest::SourceChanged));
            }
        }
        let span = tracing::info_span!(
            target: "ngy_ai",
            "translation_fragment",
            block_ordinal = scope.ordinal,
            fragment = index,
            fragments,
        );
        // 片段自身就是它的上下文：把整段文本一起发过去，弱模型会把整段译文当成这一条
        // 片段的答案（主路径上的现场行为），拼回去会重复整段文字。
        let fragment = TranslationSource {
            text: fragment_text.clone(),
            segments: vec![fragment_text.clone()],
        };
        let request = translation_request(
            &translation.model,
            target_language,
            source_language,
            &fragment,
            &context,
            None,
        );
        let request_body = translation_request_body(&request);
        tracing::debug!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_fragment_start",
            fragment_chars = fragment_text.chars().count(),
            request_bytes = fragment.request_input(&context).len(),
            "Translation fragment started"
        );
        let stream = match await_translation_step(
            inner,
            job,
            scope,
            translation.provider.chat_stream(request),
        )
        .await?
        {
            BlockStep::Value(stream) => stream,
            BlockStep::Stopped(stop) => return Ok(BlockWalk::Stopped(stop)),
        };
        let response = match collect_translation_response(
            inner,
            job,
            scope,
            index + 1,
            &fragment,
            &context,
            &request_body,
            &translation.execution_identity,
            stream,
        )
        .await?
        {
            BlockStep::Value(response) => response,
            BlockStep::Stopped(stop) => return Ok(BlockWalk::Stopped(stop)),
        };
        let decoded = crate::ai::translation::decoded_segment_shapes(&response);
        tracing::debug!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_fragment_collected",
            fragment = index,
            fragments,
            response_bytes = response.len(),
            decoded_segments = decoded.as_ref().map(Vec::len).unwrap_or(0),
            decoded_ids = %decoded.as_ref().map(|shapes| decoded_segment_label(shapes)).unwrap_or_default(),
            decoded_error_kind = decoded.as_ref().err().map(|error| error.kind()).unwrap_or("-"),
            "Translation fragment response decoded"
        );
        let parsed = match crate::ai::translation::parse_response(
            &fragment,
            &response,
            &translation.execution_identity,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(
                    target: "ngy_ai",
                    parent: &span,
                    stage = "translation_fragment_rejected",
                    fragment = index,
                    fragments,
                    error_kind = crate::ai::ai_diagnostics::error_kind(&error),
                    response_bytes = response.len(),
                    "Translation fragment rejected"
                );
                dump_raw_translation_response(
                    &span,
                    scope.ordinal,
                    index + 1,
                    &request_body,
                    &response,
                    "fragment_rejected",
                );
                return Err(error.context(format!(
                    "逐片段回退时第 {} 个片段（共 {fragments} 个）仍不符合分段协议",
                    index + 1
                )));
            }
        };
        let Some(stored) = parsed.segments.into_iter().next() else {
            return Err(anyhow::anyhow!("逐片段回退没有产出译文"));
        };
        tracing::debug!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_fragment_translated",
            fragment = index,
            fragments,
            translated_chars = stored.translated.chars().count(),
            "Translation fragment translated"
        );
        // 逐片段回退的答案同样要能逐块核对（同一开关，默认关闭）。
        dump_raw_translation_response(
            &span,
            scope.ordinal,
            index + 1,
            &request_body,
            &response,
            "fragment_accepted",
        );
        translated.push(stored.translated);
        stats.fragment_fallback_requests += 1;
    }
    let segments = source
        .segments
        .iter()
        .cloned()
        .zip(translated)
        .map(
            |(source, translated)| crate::ai::translation::TranslationSegment {
                source,
                translated,
            },
        )
        .collect::<Vec<_>>();
    tracing::info!(
        target: "ngy_ai",
        stage = "translation_fragment_fallback_finish",
        block_ordinal = scope.ordinal,
        fragments,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Translation block translated one fragment at a time"
    );
    Ok(BlockWalk::Stored(
        StoredTranslation {
            execution_identity: translation.execution_identity.clone(),
            segments,
        },
        stats,
    ))
}
