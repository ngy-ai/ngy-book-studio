//! Collecting one streamed answer and the awaited provider step.

use super::*;

use std::{future::Future, pin::Pin, sync::Arc, time::Instant};

use anyhow::{Result, ensure};

use crate::{
    ai::job_diagnostics::{JobLogErrorKind, JobLogEvent, JobLogMetrics, classify_error},
    ai::translation::{TranslationContext, TranslationSource},
    db,
};

/// Collects one streaming translation answer.
///
/// There is deliberately no total-duration deadline: a slow local model may need
/// minutes for one block, and the observed failure (2026-09-11, block 1269) was a
/// *healthy* 120-second stream — 1579 content chunks, 7024 bytes, no `[DONE]` —
/// that the old whole-request timeout cut in half. Silence is bounded by the
/// provider layer **and** by [`TRANSLATION_STREAM_SILENCE_LIMIT`] here, because a
/// stream that stops producing events altogether (no TCP connection, no error) is
/// exactly what the provider layer cannot see; the reply itself stays bounded by
/// `MAX_TRANSLATION_RESPONSE_BYTES`. Progress lines and the end-of-stream shape
/// keep such a run diagnosable.
pub(crate) async fn collect_translation_response(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    scope: &TranslationBlockScope,
    request_attempt: usize,
    source: &TranslationSource,
    // 收集阶段只用 `request_body` 与冻结的 `source`：上下文已经进了那一份 body，
    // 这里不再解包它（调用方照旧传进来，因为它与 source 一起构成这次请求的身份）。
    _context: &TranslationContext<'_>,
    request_body: &str,
    identity: &str,
    mut stream: crate::ai::ChatEventStream,
) -> Result<BlockStep<String>> {
    let started = Instant::now();
    let span = tracing::Span::current();
    let mut response = String::new();
    let mut stats = TranslationStreamStats::new(started);
    let mut next_progress_at = started + TRANSLATION_STREAM_PROGRESS_INTERVAL;
    let collected = loop {
        tokio::select! {
            event = stream.next() => {
                let Some(event) = event else {
                    stats.end = "stream_eof";
                    break Ok(None);
                };
                stats.events = stats.events.saturating_add(1);
                let now = Instant::now();
                stats
                    .first_event_ms
                    .get_or_insert_with(|| (now - started).as_millis() as u64);
                stats.last_event_at = now;
                let event = match event.context("翻译模型流式响应失败") {
                    Ok(event) => event,
                    Err(error) => {
                        stats.end = "stream_error";
                        break Err(error);
                    }
                };
                if let Some(reason) = event.finish_reason.as_deref() {
                    stats.finish_reason = Some(crate::ai::ai_diagnostics::finish_reason_label(reason));
                }
                if let Some(usage) = event.usage {
                    stats.usage = Some(usage);
                }
                if !event.tool_call_deltas.is_empty() {
                    stats.end = "tool_call";
                    break Err(anyhow::anyhow!("翻译模型意外请求了工具"));
                }
                if let Some(reason) = stats.finish_reason
                    && reason != "stop"
                {
                    stats.end = "unexpected_finish_reason";
                    // 输出上限截断：流是正常收尾的，只是模型没写完整答案。它不带运行级
                    // 含义——「这个文本块这次没翻出来」——所以按 `ResponseError` 分类返回，
                    // 由调用方按块跳过并继续翻译其余块（与 `docs/agent/code-map-ai.md`
                    // 的「不重试输出上限错误」一致：跳过，不重复同一种请求）。当成不透明的
                    // anyhow 错误会让整本书永远停在同一个退化文本块上——现场 2026-09-15
                    // 的 block 1762
                    // （qwen3.5:0.8b，3 个片段）：两次运行都在 4096 个输出 token 处截断，
                    // 响应各 4425 字节、一个完整容器都没有，任务重试多少次都停在 ordinal
                    // 1762。
                    if reason == "length" {
                        break Err(crate::ai::translation::ResponseError::response_truncated().into());
                    }
                    break Err(anyhow::anyhow!(
                        "翻译模型提前结束响应（finish_reason={reason}），请重试或更换模型"
                    ));
                }
                if let Some(delta) = event.content_delta {
                    if delta.is_empty() {
                        stats.empty_content_events = stats.empty_content_events.saturating_add(1);
                    } else {
                        stats.content_events = stats.content_events.saturating_add(1);
                        if response.len().saturating_add(delta.len())
                            > MAX_TRANSLATION_RESPONSE_BYTES
                        {
                            stats.end = "response_too_large";
                            break Err(anyhow::anyhow!("翻译模型响应超过大小上限"));
                        }
                        stats.content_bytes = stats.content_bytes.saturating_add(delta.len());
                        response.push_str(&delta);
                    }
                }
                if event.done {
                    stats.done = true;
                    stats.end = "done";
                    break Ok(None);
                }
            }
            _ = tokio::time::sleep(CONTROL_POLL_INTERVAL) => {
                match block_stop_request(inner, job, &scope.committed_json()).await {
                    Ok(Some(stop)) => {
                        stats.end = "interrupted";
                        break Ok(Some(stop));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        stats.end = "control_error";
                        break Err(error);
                    }
                }
                // 只记录进度，不设总时长上限：继续产出数据的流必须能翻完这一块。
                // 静默由 provider 层的空闲超时负责，输出规模由 max_tokens 与响应字节上限约束。
                //
                // 但静默必须有自己的上限：provider 层兜不住「流不再产生任何事件」
                // （见 [`TRANSLATION_STREAM_SILENCE_LIMIT`]），没有这条检查时整本书会
                // 永久停在这一块，并连带占住 `transitions` 门闩让设置保存一起卡死。
                if stream_silence_exceeded(stats.since_last_event_ms()) {
                    stats.end = "stream_silent";
                    tracing::warn!(
                        target: "ngy_ai",
                        parent: &span,
                        stage = "translation_stream_silent",
                        block_ordinal = scope.ordinal,
                        request_attempt,
                        since_last_event_ms = stats.since_last_event_ms(),
                        events = stats.events,
                        response_bytes = response.len(),
                        "Translation response stream went silent"
                    );
                    // 用 provider 层同一个超时类型报错，分类仍是 `http_timeout`：
                    // 后台任务日志与 UI 不因为「静默改由收流这一层兜底」多出一个类别。
                    break Err(anyhow::Error::new(crate::ai::ProviderTimeout::stream(
                        TRANSLATION_STREAM_SILENCE_LIMIT.as_secs(),
                    ))
                    .context("翻译模型流式响应静默超时"));
                }
                if Instant::now() >= next_progress_at {
                    next_progress_at = Instant::now() + TRANSLATION_STREAM_PROGRESS_INTERVAL;
                    let shape = translation_response_shape(&response);
                    tracing::debug!(
                        target: "ngy_ai",
                        parent: &span,
                        stage = "translation_stream_progress",
                        block_ordinal = scope.ordinal,
                        request_attempt,
                        elapsed_ms = (Instant::now() - started).as_millis() as u64,
                        since_last_event_ms = stats.since_last_event_ms(),
                        events = stats.events,
                        content_events = stats.content_events,
                        empty_content_events = stats.empty_content_events,
                        content_bytes = stats.content_bytes,
                        response_bytes = response.len(),
                        containers = shape.containers,
                        open_container = shape.open_container,
                        segment_markers = shape.segment_markers,
                        thinking_open = shape.thinking_open,
                        repeated_container = shape.repeated_container,
                        "Translation response stream progress"
                    );
                }
            }
        }
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match collected {
        Ok(None) => {
            let shape = translation_response_shape(&response);
            tracing::debug!(
                target: "ngy_ai",
                parent: &span,
                stage = "translation_stream",
                outcome = "collected",
                end = stats.end,
                events = stats.events,
                content_events = stats.content_events,
                empty_content_events = stats.empty_content_events,
                response_bytes = stats.content_bytes,
                first_event_ms = stats.first_event_ms,
                since_last_event_ms = stats.since_last_event_ms(),
                finish_reason = stats.finish_reason,
                prompt_tokens = stats.usage.as_ref().map(|usage| usage.prompt_tokens),
                completion_tokens = stats.usage.as_ref().map(|usage| usage.completion_tokens),
                total_tokens = stats.usage.as_ref().map(|usage| usage.total_tokens),
                done = stats.done,
                containers = shape.containers,
                open_container = shape.open_container,
                segment_markers = shape.segment_markers,
                thinking_open = shape.thinking_open,
                repeated_container = shape.repeated_container,
                elapsed_ms,
                "Translation response stream collected"
            );
            Ok(BlockStep::Value(response))
        }
        Ok(Some(stop)) => {
            tracing::info!(
                target: "ngy_ai",
                parent: &span,
                stage = "translation_stream",
                outcome = "interrupted",
                end = stats.end,
                events = stats.events,
                content_events = stats.content_events,
                empty_content_events = stats.empty_content_events,
                response_bytes = stats.content_bytes,
                first_event_ms = stats.first_event_ms,
                since_last_event_ms = stats.since_last_event_ms(),
                finish_reason = stats.finish_reason,
                prompt_tokens = stats.usage.as_ref().map(|usage| usage.prompt_tokens),
                completion_tokens = stats.usage.as_ref().map(|usage| usage.completion_tokens),
                total_tokens = stats.usage.as_ref().map(|usage| usage.total_tokens),
                done = stats.done,
                elapsed_ms,
                "Translation response stream interrupted"
            );
            Ok(BlockStep::Stopped(stop))
        }
        Err(error) => {
            let shape = translation_response_shape(&response);
            tracing::warn!(
                target: "ngy_ai",
                parent: &span,
                stage = "translation_stream",
                outcome = "failed",
                error_kind = crate::ai::ai_diagnostics::error_kind(&error),
                end = stats.end,
                events = stats.events,
                content_events = stats.content_events,
                empty_content_events = stats.empty_content_events,
                response_bytes = stats.content_bytes,
                first_event_ms = stats.first_event_ms,
                since_last_event_ms = stats.since_last_event_ms(),
                finish_reason = stats.finish_reason,
                prompt_tokens = stats.usage.as_ref().map(|usage| usage.prompt_tokens),
                completion_tokens = stats.usage.as_ref().map(|usage| usage.completion_tokens),
                total_tokens = stats.usage.as_ref().map(|usage| usage.total_tokens),
                done = stats.done,
                parsed_as = shape.parsed_as,
                containers = shape.containers,
                open_container = shape.open_container,
                segment_markers = shape.segment_markers,
                thinking_open = shape.thinking_open,
                repeated_container = shape.repeated_container,
                elapsed_ms,
                "Translation response stream failed"
            );
            dump_raw_translation_response(
                &span,
                scope.ordinal,
                request_attempt,
                request_body,
                Some(&response),
                stats.end,
            );
            // 流没有正常结束，但已经收到的字节可能包含一个完整、能通过全部片段校验的
            // 答案（模型答完继续啰嗦、或服务端在收尾前断开）。整段文本仍走原来的严格
            // 解码：只有一个完整容器、id 不多不少时才会被采用，不会猜模型意图。
            if crate::ai::translation::parse_response(source, &response, identity).is_ok() {
                tracing::warn!(
                    target: "ngy_ai",
                    parent: &span,
                    stage = "translation_response_salvaged",
                    block_ordinal = scope.ordinal,
                    request_attempt,
                    end = stats.end,
                    response_bytes = response.len(),
                    containers = shape.containers,
                    "Translation response salvaged from an unfinished stream"
                );
                return Ok(BlockStep::Value(response));
            }
            record_index_event(
                inner,
                job,
                JobLogEvent::StepFailed,
                JobLogMetrics {
                    ordinal: Some(scope.ordinal as u64),
                    request_attempt: Some(request_attempt as u64),
                    response_bytes: Some(stats.content_bytes as u64),
                    actual_count: Some(stats.events as u64),
                    duration_ms: Some(elapsed_ms),
                    // 截断已经是一条按 `ResponseError` 分好类的失败，直接用它的类别；
                    // 其余按流的结束方式归类（`unexpected_finish_reason` 里除截断外都是
                    // 流协议问题）。
                    error_kind: Some(if error.is::<crate::ai::translation::ResponseError>() {
                        classify_error(&error)
                    } else {
                        match stats.end {
                            "response_too_large" => JobLogErrorKind::InvalidData,
                            "tool_call" | "unexpected_finish_reason" => {
                                JobLogErrorKind::StreamProtocol
                            }
                            "control_error" => JobLogErrorKind::Database,
                            _ => classify_error(&error),
                        }
                    }),
                    ..JobLogMetrics::for_error(&error)
                },
            );
            Err(error)
        }
    }
}

/// One awaited provider step of a translation block, under its own control check.
///
/// This is the block-level counterpart of [`await_provider_step`]: it returns the
/// reason to stop instead of a `RunOutcome`, because the walk — not the request —
/// owns the cursor and therefore has to publish the outcome.
pub(crate) async fn await_translation_step<T>(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    scope: &TranslationBlockScope,
    future: Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<BlockStep<T>> {
    let started = Instant::now();
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result.map(BlockStep::Value),
            _ = tokio::time::sleep(CONTROL_POLL_INTERVAL) => {
                if let Some(stop) = block_stop_request(inner, job, &scope.committed_json()).await? {
                    return Ok(BlockStep::Stopped(stop));
                }
                ensure!(started.elapsed() < PROVIDER_STEP_TIMEOUT, "模型请求超时");
            }
        }
    }
}
