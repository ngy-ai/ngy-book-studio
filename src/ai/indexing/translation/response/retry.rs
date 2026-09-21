//! The strict retry loop of one block: a fixed failure is re-sent up to the
//! retry budget before the block is given up on.

use super::*;

use std::{sync::Arc, time::Instant};

use anyhow::Result;

use crate::{
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics},
    ai::translation::{TranslationContext, TranslationSource},
    db,
};

/// Translates one text block. A block that cannot be produced in the required
/// shape is otherwise skipped and left in the original language, so a whole
/// round that fails is re-sent up to `TRANSLATION_BLOCK_RETRIES` times. Only a
/// protocol failure is retried: a provider, database or cancellation error still
/// fails the whole run, because re-sending the same request is not what fixes it.
///
/// 每次重发的第一发都带着上一轮的失败类别，所以重试不是逐字相同的请求；逐片段回退
/// 只留给最后一轮，否则一个多片段块会把请求预算耗在重复的逐片段请求上。
///
/// The control checks read `scope`, not the run cursor: the cursor keeps moving
/// while this request waits, and only the committed JSON the scope carries proves
/// the durable row is still this execution's.
pub(crate) async fn translate_block(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    scope: &TranslationBlockScope,
    translation: &TranslationServices,
    target_language: &str,
    source_language: Option<&str>,
    source: &TranslationSource,
    context: TranslationContext<'_>,
) -> Result<BlockWalk> {
    let started = Instant::now();
    // The walk counts its own work: a parallel window cannot share one mutable
    // summary across requests, and the run adds these up when it commits.
    let mut stats = BlockStats::default();
    // 上一轮最后一次失败的固定分类，作为这一轮第一发请求的纠正提示。第一轮是干净的
    // 严格尝试（`None`），重试轮则从一开始就告诉模型上一轮错在哪里。
    let mut carried_kind: Option<&'static str> = None;
    // 每一次失败尝试的固定分类，跨重发轮累积：整块重试的最后一轮要靠它判断「这块该不该
    // 走逐片段回退」，而这块模型的失败形状可能出现在任意一轮里。
    let mut rejected_kinds: Vec<&'static str> = Vec::new();
    let rounds = TRANSLATION_BLOCK_RETRIES + 1;
    for round in 1..=rounds {
        if round > 1 {
            {
                let _transition = inner.transitions.lock().await;
                if let Some(stop) = block_stop_request(inner, job, &scope.committed_json()).await? {
                    return Ok(BlockWalk::Stopped(stop));
                }
                if !source_is_current(inner, job, scope.revision).await? {
                    return Ok(BlockWalk::Stopped(StopRequest::SourceChanged));
                }
            }
            record_index_event(
                inner,
                job,
                JobLogEvent::BlockRetry,
                JobLogMetrics {
                    ordinal: Some(scope.ordinal as u64),
                    request_attempt: Some(round as u64),
                    expected_count: Some(source.segments.len() as u64),
                    ..JobLogMetrics::default()
                },
            );
            tracing::warn!(
                target: "ngy_ai",
                stage = "translation_block_retry",
                block_ordinal = scope.ordinal,
                round,
                rounds,
                previous_error_kind = carried_kind.unwrap_or("-"),
                segments = source.segments.len(),
                "Translation block re-sent after a rejected answer"
            );
        }
        let outcome = translate_block_round(
            inner,
            job,
            scope,
            translation,
            target_language,
            source_language,
            source,
            context,
            &mut rejected_kinds,
            carried_kind,
            round == rounds,
        )
        .await;
        match outcome {
            Ok(BlockWalk::Stopped(stop)) => return Ok(BlockWalk::Stopped(stop)),
            Ok(BlockWalk::Stored(stored, round_stats)) => {
                stats.fragment_fallback_blocks += round_stats.fragment_fallback_blocks;
                stats.fragment_fallback_requests += round_stats.fragment_fallback_requests;
                stats.duration_ms = started.elapsed().as_millis() as u64;
                if round > 1 {
                    tracing::info!(
                        target: "ngy_ai",
                        stage = "translation_block_retry_accepted",
                        block_ordinal = scope.ordinal,
                        round,
                        rounds,
                        "Translation block accepted on a retry"
                    );
                }
                return Ok(BlockWalk::Stored(stored, stats));
            }
            Err(error) => {
                // 只有「模型答了、但形状不对」才重发。provider、数据库和取消错误照旧
                // 上抛，让整次运行失败：重发同一个请求解决不了它们。
                let Some(detail) = error.downcast_ref::<crate::ai::translation::ResponseError>()
                else {
                    return Err(error);
                };
                carried_kind = Some(detail.kind());
                // 最后一次失败的类别同样入列：round 只会记录进入纠正的那次（第一次），
                // 最后一发是在这里补上的。
                rejected_kinds.push(detail.kind());
                if round == rounds {
                    return Err(error.context(format!(
                        "翻译响应在自动纠正一次并重发 {TRANSLATION_BLOCK_RETRIES} 次后仍不符合格式要求，\
                         请重试或在 AI 设置的「模型设置」里更换支持指令的翻译模型"
                    )));
                }
            }
        }
    }
    unreachable!("the final round always returns a result")
}

/// One round of one text block: the strict attempt, the single protocol-correction
/// attempt, and (only in the last round) the per-fragment fallback when the model
/// answered but not in the required shape.
///
/// `rejected_kinds` belongs to the whole block, not to one round: the fallback
/// decision below reads every attempt of every round, because the shape a weak
/// model fails with can change between rounds exactly as it changes between the
/// strict and the corrected attempt.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_block_round(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    scope: &TranslationBlockScope,
    translation: &TranslationServices,
    target_language: &str,
    source_language: Option<&str>,
    source: &TranslationSource,
    context: TranslationContext<'_>,
    rejected_kinds: &mut Vec<&'static str>,
    initial_correction: Option<&'static str>,
    allow_fragment_fallback: bool,
) -> Result<BlockWalk> {
    let started = Instant::now();
    let mut stats = BlockStats::default();
    // Fixed category of the rejected response, used only to restate which part
    // of the format the single correction attempt must repair.
    let mut correction_kind: Option<&'static str> = initial_correction;
    for attempt in 1..=TRANSLATION_RESPONSE_ATTEMPTS {
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
            "translation_block",
            block_ordinal = scope.ordinal,
            attempt,
            segments = source.segments.len(),
        );
        let attempt_started = Instant::now();
        if attempt > 1 {
            record_index_event(
                inner,
                job,
                JobLogEvent::ProtocolCorrection,
                JobLogMetrics {
                    ordinal: Some(scope.ordinal as u64),
                    request_attempt: Some(attempt as u64),
                    expected_count: Some(source.segments.len() as u64),
                    ..JobLogMetrics::default()
                },
            );
        }
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelRequested,
            JobLogMetrics {
                ordinal: Some(scope.ordinal as u64),
                request_attempt: Some(attempt as u64),
                expected_count: Some(source.segments.len() as u64),
                ..JobLogMetrics::default()
            },
        );
        // Reuse the frozen source, language and provider. A malformed response
        // is never copied into the next prompt, and no partial result is saved.
        let request = translation_request(
            &translation.model,
            target_language,
            source_language,
            source,
            &context,
            correction_kind,
        );
        // 本次尝试真正要发出去的 body，供 NGY_DUMP_TRANSLATION_RAW 原样打印。
        let request_body = translation_request_body(&request);
        tracing::debug!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_attempt_start",
            correction = attempt > 1,
            correction_kind = ?correction_kind,
            segments = source.segments.len(),
            expected_ids = %translation_expected_ids(source.segments.len()),
            segment_chars = %translation_segment_chars(&source.segments),
            source_chars = source.text.chars().count(),
            system_bytes = request
                .messages
                .first()
                .and_then(|message| message.content.as_ref())
                .and_then(|content| match content {
                    MessageContent::Text(text) => Some(text.len()),
                    MessageContent::Parts(_) => None,
                })
                .unwrap_or(0),
            user_bytes = source.request_input(&context).len(),
            message_count = request.messages.len(),
            max_tokens = TRANSLATION_MAX_OUTPUT_TOKENS,
            "Translation attempt started"
        );
        let response = async {
            let stream = match await_translation_step(
                inner,
                job,
                scope,
                translation.provider.chat_stream(request),
            )
            .await?
            {
                BlockStep::Value(stream) => stream,
                BlockStep::Stopped(stop) => return Ok(BlockStep::Stopped(stop)),
            };
            collect_translation_response(
                inner,
                job,
                scope,
                attempt,
                source,
                &context,
                &request_body,
                &translation.execution_identity,
                stream,
            )
            .await
        }
        .instrument(span.clone())
        .await;

        let _transition = inner.transitions.lock().await;
        if let Some(stop) = block_stop_request(inner, job, &scope.committed_json()).await? {
            return Ok(BlockWalk::Stopped(stop));
        }
        let response = match response {
            Err(error) => {
                record_index_event(
                    inner,
                    job,
                    JobLogEvent::StepFailed,
                    JobLogMetrics {
                        ordinal: Some(scope.ordinal as u64),
                        request_attempt: Some(attempt as u64),
                        duration_ms: Some(attempt_started.elapsed().as_millis() as u64),
                        error_kind: Some(classify_error(&error)),
                        ..JobLogMetrics::for_error(&error)
                    },
                );
                return Err(error);
            }
            Ok(response) => response,
        };
        let response = match response {
            BlockStep::Value(response) => response,
            BlockStep::Stopped(stop) => return Ok(BlockWalk::Stopped(stop)),
        };
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelCompleted,
            JobLogMetrics {
                ordinal: Some(scope.ordinal as u64),
                request_attempt: Some(attempt as u64),
                response_bytes: Some(response.len() as u64),
                duration_ms: Some(attempt_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );
        tracing::debug!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_attempt_collected",
            response_bytes = response.len(),
            "Translation response collected"
        );
        // 严格解码后的形状：这是「模型到底写了几条片段、每条多长」的唯一权威读数。
        // 现场（2026-09-13，qwen3.5:0.8b）：期望 2 条、实际 1 条，整段译文被塞进
        // id=0，据此才能判断模型是把多个片段合并了，而不是没有回答。
        let decoded = crate::ai::translation::decoded_segment_shapes(&response);
        tracing::debug!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_response_decoded",
            expected_segments = source.segments.len(),
            expected_ids = %translation_expected_ids(source.segments.len()),
            decoded_segments = decoded.as_ref().map(Vec::len).unwrap_or(0),
            decoded_ids = %decoded.as_ref().map(|shapes| decoded_segment_label(shapes)).unwrap_or_default(),
            decoded_error_kind = decoded.as_ref().err().map(|error| error.kind()).unwrap_or("-"),
            merged_into_one = decoded
                .as_ref()
                .is_ok_and(|shapes| shapes.len() == 1 && source.segments.len() > 1),
            response_bytes = response.len(),
            parsed_as = translation_response_shape(&response).parsed_as,
            "Translation response decoded"
        );
        match crate::ai::translation::parse_response(
            source,
            &response,
            &translation.execution_identity,
        ) {
            Ok(parsed) => {
                // 「保持原文」的片段在解码形状里就是长度 0（`keep` 必须与空 text 同时给出，
                // 空 text 又不带 keep 的答案会在校验里被拒），所以这个计数是模型声明了
                // 「这段不需要翻译」的片段数，也是排查「任务完成但阅读端没有译文」的第一手读数。
                let kept_segments = decoded
                    .as_ref()
                    .map(|shapes| shapes.iter().filter(|(_, chars)| *chars == 0).count())
                    .unwrap_or_default();
                tracing::debug!(
                    target: "ngy_ai",
                    parent: &span,
                    stage = "translation_response_accepted",
                    segments = parsed.segments.len(),
                    kept_segments,
                    "Translation response accepted"
                );
                // 被接受的答案也要留下「模型收到了什么、答了什么」这一对原始读数：
                // 译文到底是不是真的由模型翻出来的，聚合计数证明不了（现场 2026-09-20：
                // 任务显示「已处理 18168」而库里只有 71 行译文）。与拒绝路径同一开关，
                // 默认关闭，只有 NGY_DUMP_TRANSLATION_RAW 打开时才输出。
                dump_raw_translation_response(
                    &span,
                    scope.ordinal,
                    attempt,
                    &request_body,
                    &response,
                    "accepted",
                );
                if attempt > 1 {
                    tracing::info!(target: "ngy_ai", parent: &span, "Translation response corrected");
                }
                stats.duration_ms = started.elapsed().as_millis() as u64;
                return Ok(BlockWalk::Stored(parsed, stats));
            }
            Err(error) => {
                let Some(detail) = error.downcast_ref::<crate::ai::translation::ResponseError>()
                else {
                    record_index_event(
                        inner,
                        job,
                        JobLogEvent::ProtocolRejected,
                        JobLogMetrics {
                            ordinal: Some(scope.ordinal as u64),
                            request_attempt: Some(attempt as u64),
                            response_bytes: Some(response.len() as u64),
                            error_kind: Some(classify_error(&error)),
                            ..JobLogMetrics::for_error(&error)
                        },
                    );
                    tracing::warn!(
                        target: "ngy_ai",
                        parent: &span,
                        stage = "translation_protocol",
                        error_kind = crate::ai::ai_diagnostics::error_kind(&error),
                        response_bytes = response.len(),
                        retry = false,
                        "Translation response failed before format validation"
                    );
                    return Err(error);
                };
                let retry = attempt < TRANSLATION_RESPONSE_ATTEMPTS;
                record_index_event(
                    inner,
                    job,
                    JobLogEvent::ProtocolRejected,
                    JobLogMetrics {
                        ordinal: Some(scope.ordinal as u64),
                        request_attempt: Some(attempt as u64),
                        response_bytes: Some(response.len() as u64),
                        error_kind: Some(classify_error(&error)),
                        json_line: detail.json_line().map(|value| value as u64),
                        json_column: detail.json_column().map(|value| value as u64),
                        expected_count: detail.expected_segments().map(|value| value as u64),
                        actual_count: detail.actual_segments().map(|value| value as u64),
                        duration_ms: Some(attempt_started.elapsed().as_millis() as u64),
                        ..JobLogMetrics::for_error(&error)
                    },
                );
                let shape = translation_response_shape(&response);
                tracing::warn!(
                    target: "ngy_ai",
                    parent: &span,
                    stage = "translation_response",
                    error_kind = crate::ai::ai_diagnostics::error_kind(&error),
                    json_line = detail.json_line(),
                    json_column = detail.json_column(),
                    expected_segments = detail.expected_segments(),
                    actual_segments = detail.actual_segments(),
                    response_bytes = response.len(),
                    parsed_as = shape.parsed_as,
                    top_level_keys = shape.top_level_keys,
                    longest_string_leaf = shape.longest_string_leaf,
                    containers = shape.containers,
                    open_container = shape.open_container,
                    segment_markers = shape.segment_markers,
                    thinking_open = shape.thinking_open,
                    repeated_container = shape.repeated_container,
                    retry,
                    "Translation response rejected"
                );
                dump_raw_translation_response(
                    &span,
                    scope.ordinal,
                    attempt,
                    &request_body,
                    &response,
                    "protocol_rejected",
                );
                if !retry {
                    // 回退在每个片段前会重新取同一把转换锁，所以必须先释放本次迭代持有的
                    // 锁，否则 `transitions.lock().await` 会在这里自锁。
                    drop(_transition);
                    // 两次严格尝试都用完了。多片段文本块再试一次「逐片段」：弱模型
                    // （现场 qwen3.5:0.8b）的能力上限就是「一次只写一条译文」，把整段
                    // 译文合并进 id=0，因此单片段请求是它能满足的形状。回退仍然只用
                    // 冻结原文，不把模型的错误输出传回去，也不猜片段边界。
                    //
                    // 判定看整轮尝试的失败类别（`rejected_kinds`），不是只看最后一次：
                    // 纠正提示会改变模型写出的错误形状 —— attempt 1 合并成一条
                    // （`segment_count_mismatch`）、attempt 2 变成写不闭合的 JSON
                    // （`invalid_json` / `incomplete_json`）是同一件事的两面。只读最后一次
                    // 会让这类块白白跳过（取消连续跳过上限之前，连续三块还会结束整次运行）。
                    // 逐片段回退只在整块重试的最后一轮执行：它已经是最后手段，
                    // 前面几轮先靠「带着上一轮失败类别的重发」解决。
                    let fallback = allow_fragment_fallback
                        && fragment_fallback_applies(source, rejected_kinds.as_slice());
                    tracing::warn!(
                        target: "ngy_ai",
                        parent: &span,
                        stage = "translation_protocol_exhausted",
                        error_kind = detail.kind(),
                        rejected_kinds = %rejected_kinds.join(","),
                        segments = source.segments.len(),
                        fragment_fallback = fallback,
                        fragment_fallback_limit = MAX_TRANSLATION_FRAGMENT_FALLBACKS,
                        "Translation block exhausted its protocol attempts"
                    );
                    if fallback {
                        stats.fragment_fallback_blocks += 1;
                        let walk = translate_block_by_fragment(
                            inner,
                            job,
                            scope,
                            translation,
                            target_language,
                            source_language,
                            source,
                            context,
                        )
                        .await?;
                        // The block's duration covers both the strict attempts and
                        // the fallback: the task window reports one number per block.
                        stats.duration_ms = started.elapsed().as_millis() as u64;
                        return Ok(match walk {
                            BlockWalk::Stored(stored, fragment) => {
                                stats.fragment_fallback_requests +=
                                    fragment.fragment_fallback_requests;
                                BlockWalk::Stored(stored, stats)
                            }
                            BlockWalk::Stopped(stop) => BlockWalk::Stopped(stop),
                        });
                    }
                    return Err(error);
                }
                rejected_kinds.push(detail.kind());
                correction_kind = Some(detail.kind());
            }
        }
    }
    unreachable!("the final attempt always returns a result")
}
