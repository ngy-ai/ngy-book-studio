//! 把多个文本块装进一次请求：批信封、拆半重发与逐块回退单块。
//!
//! 批只是**传输信封**（形状、打包预算与信封解析见 `src/ai/translation/batch.rs`）：
//! 块自己的校验、重试、协议纠正与逐片段回退仍走单块路径，所以这一层只做四件事 ——
//! 发出一发批请求、把回复按块还原、信封不合格或整批被截断时拆半重发、某一块自己
//! 不合格时只把那一块交回单块路径。
//!
//! 三条边界：
//!
//! - **只装同一个内容单元（同一章）的块**：`book`/`chapter` 语境字段是每批一份，
//!   规则由 walk 在攒批时保证（[`TranslationBatchBlock::unit_id`]）。
//! - **拆半是单调的**：块数每层减半，递归到一块时交给单块路径，因此「拆半重发」
//!   不可能变成无限重发。
//! - **整发失败只跳过这一批的块**：流自己失败（连接中断、静默、工具调用）时拆半
//!   只会对同一个坏端点多发几发请求，这一批的每一块都按请求级失败处理，连续失败照旧
//!   由 walk 的冷却兜住。

use super::*;

use std::{sync::Arc, time::Instant};

use anyhow::Result;

use crate::{
    ai::job_diagnostics::{JobLogErrorKind, JobLogEvent, JobLogMetrics, classify_error},
    ai::translation::{ResponseError, TranslationBatchItem, parse_batch_response},
    db,
};

/// 批里的一个成员：块号、它所在的单元，以及它自己的冻结输入与本次运行里的坐标。
pub(crate) struct TranslationBatchBlock<'a> {
    pub(crate) ordinal: usize,
    /// 这一块所在的内容单元。同一个单元的块才能进同一批：`book`/`chapter` 语境字段
    /// 是每批一份，混进另一章的块会让这两个字段撒谎（见
    /// [`crate::ai::translation::batch_request_input`]）。
    pub(crate) unit_id: &'a str,
    pub(crate) source: &'a TranslationSource,
    pub(crate) context: TranslationContext<'a>,
    pub(crate) scope: TranslationBlockScope,
}

/// 一发批请求的结论。
enum BatchRound {
    /// 整批都还原出来了：每一项对应输入里同位置的块。`Err` 是**这一块自己**的片段
    /// 形状问题（片段数量、编号、`keep`、空白、NUL），只影响那一块。
    Members {
        members: Vec<std::result::Result<StoredTranslation, ResponseError>>,
        duration_ms: u64,
    },
    /// 这一批不能用了，拆半重发。
    Split(&'static str),
}

/// 这一批接下来该怎么办。
enum BatchVerdict {
    /// 拆半重发。理由只进诊断（固定分类）。
    Split(&'static str),
    /// 拆不了：这一发请求自己失败了，整批按请求级失败处理。
    Failed,
}

/// 按批翻译一组连续的文本块。
///
/// 返回的每一项是 `(块号, 那一块的结果)`，顺序与 `blocks` 相同：`Err` 表示这一块没有
/// 译文（协议级或请求级失败），由 walk 按既有的 `block_skip_cause` 逐块处理。外层
/// `Err` 表示整发请求的失败（传输、超时、流、数据库）—— 这一批的每一块都没有译文。
pub(crate) async fn translate_block_batch(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    source_language: Option<&str>,
    blocks: &[TranslationBatchBlock<'_>],
) -> Result<BlockStep<Vec<(usize, Result<BlockWalk>)>>> {
    debug_assert!(!blocks.is_empty(), "批请求至少要有一个文本块");
    send_batch_rounds(
        inner,
        job,
        translation,
        target_language,
        source_language,
        blocks,
        1,
    )
    .await
}

/// 一发批请求（含拆半重发）。`round` 是这一批在拆半树里的第几发，只进诊断。
///
/// 递归到一块时交给单块路径：一条请求只装一块时批信封没有任何意义，而单块路径带着
/// 它自己的严格尝试、一次协议纠正、`TRANSLATION_BLOCK_RETRIES` 轮重发与逐片段回退。
fn send_batch_rounds<'a>(
    inner: &'a Arc<IndexingInner>,
    job: &'a db::index_jobs::IndexJob,
    translation: &'a TranslationServices,
    target_language: &'a str,
    source_language: Option<&'a str>,
    blocks: &'a [TranslationBatchBlock<'a>],
    round: usize,
) -> BoxFuture<'a, Result<BlockStep<Vec<(usize, Result<BlockWalk>)>>>> {
    async move {
        if blocks.len() <= 1 {
            let block = &blocks[0];
            let walk = translate_block(
                inner,
                job,
                &block.scope,
                translation,
                target_language,
                source_language,
                block.source,
                block.context,
            )
            .await;
            return Ok(match walk {
                // 单块路径的停止照旧作为这一发的停止交给 walk：它负责清空窗口并按终态
                // 发布，绝不提交任何在飞块。
                Ok(BlockWalk::Stopped(stop)) => BlockStep::Stopped(stop),
                walk => BlockStep::Value(vec![(block.ordinal, walk)]),
            });
        }
        match send_batch_round(
            inner,
            job,
            translation,
            target_language,
            source_language,
            blocks,
            round,
        )
        .await?
        {
            BlockStep::Stopped(stop) => Ok(BlockStep::Stopped(stop)),
            BlockStep::Value(BatchRound::Members {
                members,
                duration_ms,
            }) => {
                restore_batch_members(
                    inner,
                    job,
                    translation,
                    target_language,
                    source_language,
                    blocks,
                    members,
                    duration_ms,
                )
                .await
            }
            BlockStep::Value(BatchRound::Split(reason)) => {
                let middle = blocks.len() / 2;
                let (left, right) = blocks.split_at(middle);
                tracing::info!(
                    target: "ngy_ai",
                    stage = "translation_batch_split",
                    block_ordinal = blocks[0].ordinal,
                    batch_blocks = blocks.len(),
                    left_blocks = left.len(),
                    right_blocks = right.len(),
                    reason,
                    round,
                    "Translation batch split in half after a rejected answer"
                );
                let mut results = match send_batch_rounds(
                    inner,
                    job,
                    translation,
                    target_language,
                    source_language,
                    left,
                    round + 1,
                )
                .await?
                {
                    BlockStep::Stopped(stop) => return Ok(BlockStep::Stopped(stop)),
                    BlockStep::Value(results) => results,
                };
                match send_batch_rounds(
                    inner,
                    job,
                    translation,
                    target_language,
                    source_language,
                    right,
                    round + 1,
                )
                .await?
                {
                    BlockStep::Stopped(stop) => return Ok(BlockStep::Stopped(stop)),
                    BlockStep::Value(right_results) => results.extend(right_results),
                }
                Ok(BlockStep::Value(results))
            }
        }
    }
    .boxed()
}

/// 把一发批回复变成逐块结果：答对的直接收，**块自己**不合格的退回单块路径。
///
/// 单块路径一次只处理一块，带着它自己的重试、协议纠正与逐片段回退，所以一处坏形状
/// 不会影响同一批里已经答对的块 —— 同批其余块照常落库。
async fn restore_batch_members(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    source_language: Option<&str>,
    blocks: &[TranslationBatchBlock<'_>],
    members: Vec<std::result::Result<StoredTranslation, ResponseError>>,
    duration_ms: u64,
) -> Result<BlockStep<Vec<(usize, Result<BlockWalk>)>>> {
    let mut results = Vec::with_capacity(blocks.len());
    for (block, member) in blocks.iter().zip(members) {
        match member {
            Ok(stored) => {
                // 批请求的耗时是这一批共有的读数：按块能记的只有它。
                results.push((
                    block.ordinal,
                    Ok(BlockWalk::Stored(
                        stored,
                        BlockStats {
                            duration_ms,
                            ..BlockStats::default()
                        },
                    )),
                ));
            }
            Err(error) => {
                tracing::warn!(
                    target: "ngy_ai",
                    stage = "translation_batch_block_fallback",
                    block_ordinal = block.ordinal,
                    batch_blocks = blocks.len(),
                    error_kind = error.kind(),
                    expected_segments = error.expected_segments(),
                    actual_segments = error.actual_segments(),
                    "Translation batch block rejected; retrying it on its own"
                );
                let walk = translate_block(
                    inner,
                    job,
                    &block.scope,
                    translation,
                    target_language,
                    source_language,
                    block.source,
                    block.context,
                )
                .await;
                match walk {
                    // 单块回退里的停止与主路径一样：整发停下来，什么都不提交。
                    Ok(BlockWalk::Stopped(stop)) => return Ok(BlockStep::Stopped(stop)),
                    walk => results.push((block.ordinal, walk)),
                }
            }
        }
    }
    Ok(BlockStep::Value(results))
}

/// 一发批请求：控制检查、请求体、收流与信封解析。块级结论与拆半都由调用方处理。
async fn send_batch_round(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    translation: &TranslationServices,
    target_language: &str,
    source_language: Option<&str>,
    blocks: &[TranslationBatchBlock<'_>],
    round: usize,
) -> Result<BlockStep<BatchRound>> {
    // 与单块路径同一套控制检查：批里的每一块都在窗口里，暂停、取消与换版本必须能在
    // 两发请求之间收回去。
    {
        let _transition = inner.transitions.lock().await;
        if let Some(stop) =
            block_stop_request(inner, job, &blocks[0].scope.committed_json()).await?
        {
            return Ok(BlockStep::Stopped(stop));
        }
        if !source_is_current(inner, job, blocks[0].scope.revision).await? {
            return Ok(BlockStep::Stopped(StopRequest::SourceChanged));
        }
    }
    let items = blocks
        .iter()
        .map(|block| TranslationBatchItem {
            source: block.source,
            context: block.context,
        })
        .collect::<Vec<_>>();
    let request = translation_batch_request(
        &translation.model,
        &translation.generation,
        target_language,
        source_language,
        &items,
    );
    // 本次尝试真正要发出去的 body，供 NGY_DUMP_TRANSLATION_RAW 原样打印。
    let request_body = translation_request_body(&request);
    // 这一发发给谁：转储行上的端点地址（执行身份只是哈希），与单块路径同一条规则。
    let endpoint = translation.provider.endpoint_origin();
    let started = Instant::now();
    let span = tracing::info_span!(
        target: "ngy_ai",
        "translation_batch",
        block_ordinal = blocks[0].ordinal,
        batch_blocks = blocks.len(),
        round,
    );
    // 每一块都有一条 `ModelRequested`：这一发请求确实替它们发出了。批里的块因此在任务
    // 日志里和单块一样有「已发起模型请求」的读数。
    for block in blocks {
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelRequested,
            JobLogMetrics {
                ordinal: Some(block.ordinal as u64),
                request_attempt: Some(round as u64),
                expected_count: Some(block.source.segments.len() as u64),
                ..JobLogMetrics::default()
            },
        );
    }
    tracing::debug!(
        target: "ngy_ai",
        parent: &span,
        stage = "translation_batch_start",
        first_ordinal = blocks[0].ordinal,
        segments = blocks.iter().map(|block| block.source.segments.len()).sum::<usize>(),
        source_chars = blocks.iter().map(|block| block.source.text.chars().count()).sum::<usize>(),
        request_bytes = request_body.len(),
        max_output_tokens = translation.generation.max_output_tokens,
        "Translation batch request started"
    );
    let response = async {
        let stream = match await_translation_step(
            inner,
            job,
            &blocks[0].scope,
            translation.provider.chat_stream(request),
        )
        .await
        {
            Ok(BlockStep::Value(stream)) => stream,
            Ok(BlockStep::Stopped(stop)) => return Ok(BlockStep::Stopped(stop)),
            // 与单块路径同一条规则：传输层失败没有响应流，但请求体仍要打出来；端点
            // 拒绝时返回的错误封套同时进转储行与 debug 预览。
            Err(error) => {
                let error_body = provider_error_body(&error);
                dump_raw_translation_response(
                    &span,
                    blocks[0].ordinal,
                    round,
                    endpoint.as_deref(),
                    &request_body,
                    error_body,
                    crate::ai::ai_diagnostics::error_kind(&error),
                );
                if let Some(body) = error_body {
                    log_raw_model_output(
                        &span,
                        blocks[0].ordinal,
                        round,
                        endpoint.as_deref(),
                        "http_error_body",
                        body,
                    );
                }
                return Err(error);
            }
        };
        let read = TranslationStreamRead {
            ordinal: blocks[0].ordinal,
            blocks: blocks.len(),
            request_attempt: round,
            max_bytes: MAX_TRANSLATION_BATCH_RESPONSE_BYTES,
            endpoint: endpoint.as_deref(),
        };
        read_translation_stream(inner, job, &blocks[0].scope, read, stream).await
    }
    .instrument(span.clone())
    .await;
    let response = match response {
        Ok(response) => response,
        // 端点收流之前直接拒绝，且原因是超出上下文长度（HTTP 400 的 context-length
        // 封套）：这是**容量**问题，不是端点坏了 —— 与 `batch_verdict` 里流内报同一
        // 理由的处理同一条规则：拆半重发，递归到一块时交给单块路径。整批按 `Err` 返回
        // 的话，walk 会把这一批的每一块都按请求级失败逐块跳过：小批本来能翻完的块白白
        // 丢译文。
        Err(error) if classify_error(&error) == JobLogErrorKind::ContextWindowExceeded => {
            tracing::warn!(
                target: "ngy_ai",
                parent: &span,
                stage = "translation_batch_rejected",
                reason = "context_window_exceeded",
                batch_blocks = blocks.len(),
                "Translation batch request was rejected for context length; splitting it in half"
            );
            return Ok(BlockStep::Value(BatchRound::Split(
                "context_window_exceeded",
            )));
        }
        Err(error) => return Err(error),
    };
    let read = match response {
        BlockStep::Value(read) => read,
        BlockStep::Stopped(stop) => return Ok(BlockStep::Stopped(stop)),
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    let end = read.stats.end;
    let response_bytes = read.text.len();
    // 流结束之后再查一次控制请求：暂停/取消到达时，这一发的结果不该再写进任何读数。
    {
        let _transition = inner.transitions.lock().await;
        if let Some(stop) =
            block_stop_request(inner, job, &blocks[0].scope.committed_json()).await?
        {
            return Ok(BlockStep::Stopped(stop));
        }
    }
    // 信封：整批按块还原。`Err` 是信封不合格（块数不符、块编号对不上、不是批形状）。
    let members = match parse_batch_response(&items, &read.text, &translation.execution_identity) {
        Ok(members) => members,
        Err(envelope_error) => {
            // 这一发的原始读数先留一份：信封坏与流坏都得看得到模型答了什么。
            dump_raw_translation_response(
                &span,
                blocks[0].ordinal,
                round,
                endpoint.as_deref(),
                &request_body,
                Some(&read.text),
                "batch_rejected",
            );
            log_raw_model_output(
                &span,
                blocks[0].ordinal,
                round,
                endpoint.as_deref(),
                "rejected",
                &read.text,
            );
            let verdict = batch_verdict(&envelope_error, read.error.as_ref(), &read.stats);
            return match verdict {
                BatchVerdict::Split(reason) => {
                    tracing::warn!(
                        target: "ngy_ai",
                        parent: &span,
                        stage = "translation_batch_rejected",
                        envelope_kind = envelope_error.kind(),
                        reason,
                        end,
                        response_bytes,
                        stream_error = read
                            .error
                            .as_ref()
                            .map(crate::ai::ai_diagnostics::error_kind)
                            .unwrap_or("-"),
                        "Translation batch response rejected; splitting it in half"
                    );
                    Ok(BlockStep::Value(BatchRound::Split(reason)))
                }
                // 拆不了：这一发请求自己失败了（连接中断、静默、工具调用）。整批按
                // 请求级失败交给 walk，由它逐块跳过 —— 拆半只会对同一个坏端点多发几发请求。
                BatchVerdict::Failed => {
                    tracing::warn!(
                        target: "ngy_ai",
                        parent: &span,
                        stage = "translation_batch_failed",
                        envelope_kind = envelope_error.kind(),
                        end,
                        response_bytes,
                        "Translation batch request failed"
                    );
                    Err(read
                        .error
                        .unwrap_or_else(|| anyhow::Error::new(envelope_error)))
                }
            };
        }
    };
    for block in blocks {
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelCompleted,
            JobLogMetrics {
                ordinal: Some(block.ordinal as u64),
                request_attempt: Some(round as u64),
                response_bytes: Some(response_bytes as u64),
                duration_ms: Some(duration_ms),
                ..JobLogMetrics::default()
            },
        );
    }
    if read.error.is_some() {
        // 流没正常收尾，但整批仍还原出来了（模型答完继续啰嗦、服务端在收尾前断开）。
        tracing::warn!(
            target: "ngy_ai",
            parent: &span,
            stage = "translation_batch_salvaged",
            block_ordinal = blocks[0].ordinal,
            batch_blocks = blocks.len(),
            end,
            response_bytes,
            "Translation batch response salvaged from an unfinished stream"
        );
    }
    dump_raw_translation_response(
        &span,
        blocks[0].ordinal,
        round,
        endpoint.as_deref(),
        &request_body,
        Some(&read.text),
        "accepted",
    );
    tracing::debug!(
        target: "ngy_ai",
        parent: &span,
        stage = "translation_batch_accepted",
        blocks = members.len(),
        response_bytes,
        duration_ms,
        "Translation batch response accepted"
    );
    Ok(BlockStep::Value(BatchRound::Members {
        members,
        duration_ms,
    }))
}

/// 信封不合格时这一批该怎么办。
///
/// 拆半只对「这一批太大、或答不完」有意义，两种情况都在这里判：流正常收尾但信封不
/// 合格（块数不符、块编号对不上、不是批形状）是形状问题，越小的批 —— 最后是单块路径
/// 的纠正与重试 —— 才有机会答对；输出上限截断、响应超过字节上限、端点报超出上下文
/// 长度则是**容量**问题，同样只有更小的批装得下。流自己失败（连接中断、静默、工具
/// 调用）时拆半只是对同一个坏端点连发请求，整批按请求级失败处理。
fn batch_verdict(
    envelope_error: &ResponseError,
    stream_error: Option<&anyhow::Error>,
    stats: &TranslationStreamStats,
) -> BatchVerdict {
    let Some(stream_error) = stream_error else {
        return BatchVerdict::Split(envelope_error.kind());
    };
    if stream_error
        .downcast_ref::<ResponseError>()
        .is_some_and(|error| error.kind() == "response_truncated")
    {
        // 整批没在输出上限里写完（摘要块尤其容易撞上）。
        return BatchVerdict::Split("response_truncated");
    }
    if classify_error(stream_error) == JobLogErrorKind::ContextWindowExceeded {
        // 端点明确说这一发超出上下文长度：这是批太大，不是端点坏了。
        return BatchVerdict::Split("context_window_exceeded");
    }
    if stats.end == "response_too_large" {
        return BatchVerdict::Split("response_too_large");
    }
    BatchVerdict::Failed
}
