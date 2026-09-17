//! Log-safe diagnostics of an answer: shape, container counts, silence and
//! the ad-hoc replay collector.

use super::*;

use std::time::Instant;

use anyhow::Result;

use crate::ai::translation::{TranslationContext, TranslationSource};

/// Constant-size diagnostics for one translation SSE response. Neither the
/// prompt, the document text nor any provider field name is retained here.
pub(crate) struct TranslationStreamStats {
    pub(crate) events: usize,
    /// 携带正文的增量块数。
    pub(crate) content_events: usize,
    /// `content` 字段存在但为空的增量块数：服务端隐藏推理（例如 Ollama 兼容层对
    /// 思考模型的处理）会长时间只回这类空块，必须与“慢但正常的回答”区分开。
    pub(crate) empty_content_events: usize,
    pub(crate) content_bytes: usize,
    pub(crate) first_event_ms: Option<u64>,
    /// 最近一个流的到达时间：周期进度行用它区分“仍在慢慢产出”和“已经静默”。
    pub(crate) last_event_at: Instant,
    pub(crate) finish_reason: Option<&'static str>,
    pub(crate) usage: Option<crate::ai::Usage>,
    pub(crate) done: bool,
    pub(crate) end: &'static str,
}

impl TranslationStreamStats {
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            last_event_at: started,
            end: "unknown",
            ..Self::default()
        }
    }

    /// 距离上一个流事件过去了多少毫秒。
    pub(crate) fn since_last_event_ms(&self) -> u64 {
        self.last_event_at.elapsed().as_millis() as u64
    }
}

impl Default for TranslationStreamStats {
    fn default() -> Self {
        Self {
            events: 0,
            content_events: 0,
            empty_content_events: 0,
            content_bytes: 0,
            first_event_ms: None,
            last_event_at: Instant::now(),
            finish_reason: None,
            usage: None,
            done: false,
            end: "unknown",
        }
    }
}

/// Safe shape summary of one translation response. Only the JSON container kind,
/// structural flags and counts are retained; field names, 原文 and 译文 are never
/// exposed (see the translation diagnostics constraints in AGENTS.md). The extra
/// counters exist to tell apart the three ways a block can fail to produce an
/// answer: the model never wrote a container, it wrote an answer and then kept
/// generating (repetition), or it never closed the thinking block it opened.
#[derive(Debug, Default)]
pub(crate) struct TranslationResponseShape {
    /// `object`, `array`, `scalar` or `none` for a response that is not JSON.
    pub(crate) parsed_as: &'static str,
    pub(crate) top_level_keys: Option<usize>,
    pub(crate) longest_string_leaf: Option<usize>,
    /// Complete top-level JSON containers in document order.
    pub(crate) containers: usize,
    /// A structural container was still open when the stream stopped.
    pub(crate) open_container: bool,
    /// Segment objects the model already wrote, counted by the protocol key.
    pub(crate) segment_markers: usize,
    /// An explicitly opened thinking block that was never closed.
    pub(crate) thinking_open: bool,
    /// The first complete container appears again later in the stream, i.e. the
    /// model repeated an answer instead of finishing.
    pub(crate) repeated_container: bool,
}

pub(crate) fn translation_response_shape(response: &str) -> TranslationResponseShape {
    let mut shape = TranslationResponseShape {
        parsed_as: "none",
        ..TranslationResponseShape::default()
    };
    match serde_json::from_str::<serde_json::Value>(response) {
        Ok(serde_json::Value::Object(map)) => {
            shape.parsed_as = "object";
            shape.top_level_keys = Some(map.len());
            shape.longest_string_leaf = Some(
                map.values()
                    .filter_map(|value| value.as_str())
                    .map(str::len)
                    .max()
                    .unwrap_or(0),
            );
        }
        Ok(serde_json::Value::Array(items)) => {
            shape.parsed_as = "array";
            shape.top_level_keys = Some(items.len());
        }
        Ok(_) => shape.parsed_as = "scalar",
        Err(_) => {}
    }
    let (containers, open_container, first_container) = top_level_containers(response);
    shape.containers = containers;
    shape.open_container = open_container;
    if let Some(first) = first_container.filter(|container| !container.is_empty())
        && response.matches(first).count() > 1
    {
        shape.repeated_container = true;
    }
    shape.segment_markers = response.matches("\"id\"").count();
    shape.thinking_open = unclosed_thinking_block(response);
    shape
}

/// Count every complete top-level `{…}`/`[…]` container and report the first one
/// together with whether one was still open at the end. A byte that is inside a
/// string literal never opens or closes a container.
pub(crate) fn top_level_containers(response: &str) -> (usize, bool, Option<&str>) {
    let mut stack = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut containers = 0;
    let mut first = None;
    for (offset, byte) in response.bytes().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                if stack.is_empty() {
                    start = offset;
                }
                stack.push(byte);
            }
            b'}' | b']' if !stack.is_empty() => {
                let expected = if byte == b'}' { b'{' } else { b'[' };
                if stack.pop() != Some(expected) {
                    return (containers, true, first);
                }
                if stack.is_empty() {
                    containers += 1;
                    if first.is_none() {
                        first = Some(&response[start..=offset]);
                    }
                }
            }
            _ => {}
        }
    }
    (containers, !stack.is_empty(), first)
}

/// Whether the response opens a `<think>` block it never closes. The protocol
/// accepts a closed leading thinking block and nothing else, so an unclosed one
/// is the signature of a reasoning model that spent the whole output budget.
pub(crate) fn unclosed_thinking_block(response: &str) -> bool {
    let mut body = response.trim_matches(crate::ai::translation::is_matching_whitespace);
    loop {
        let Some(thinking) = body.strip_prefix("<think>") else {
            return false;
        };
        match thinking.find("</think>") {
            Some(end) => {
                body = thinking[end + "</think>".len()..]
                    .trim_matches(crate::ai::translation::is_matching_whitespace)
            }
            None => return true,
        }
    }
}

/// TEMPORARY debugging aid: only emits when `NGY_DUMP_TRANSLATION_RAW` is set,
/// so default diagnostics never log response content. It prints the frozen model
/// input as well, so one failure can be replayed against the same model. Remove
/// once the provider failure modes are settled.
pub(crate) fn dump_raw_translation_response(
    span: &tracing::Span,
    block_ordinal: usize,
    attempt: usize,
    response: &str,
    source: &TranslationSource,
    context: &TranslationContext<'_>,
    reason: &'static str,
) {
    if std::env::var_os("NGY_DUMP_TRANSLATION_RAW").is_none() {
        return;
    }
    tracing::warn!(
        target: "ngy_ai",
        parent: span,
        stage = "translation_response_dump",
        block_ordinal,
        attempt,
        reason,
        response_bytes = response.len(),
        response = %response,
        "Raw rejected translation response (NGY_DUMP_TRANSLATION_RAW)"
    );
    tracing::warn!(
        target: "ngy_ai",
        parent: span,
        stage = "translation_request_dump",
        block_ordinal,
        attempt,
        reason,
        source_chars = source.text.chars().count(),
        request_bytes = source.request_input(context).len(),
        input = %source.request_input(context),
        "Frozen translation request input (NGY_DUMP_TRANSLATION_RAW)"
    );
}

/// One ad-hoc replay's collected answer.
pub(crate) struct ProbeStream {
    pub(crate) text: String,
    pub(crate) end: &'static str,
    pub(crate) finish_reason: Option<&'static str>,
}

/// Collects the answer of one ad-hoc replay.
///
/// The contract is deliberately *not* [`collect_translation_response`]'s. That
/// one belongs to a run and must reject every answer without the required shape,
/// because the rejection is what makes the block retryable and roughly classifies
/// it in the durable log. This one exists to show the user what the model
/// actually answered, so it rejects nothing that arrived: a truncated, fenced,
/// prose-padded or oversized answer comes back byte for byte, with `end` naming
/// how the stream finished and `finish_reason` carrying the model's own reason.
/// Only a provider or transport error is an error, because then there is no
/// answer to show at all.
///
/// There is also no control polling: a replay is not part of a run, so it reads
/// no cursor, records no diagnostic and stores no translation.
pub(crate) async fn collect_probe_response(
    mut stream: crate::ai::ChatEventStream,
) -> Result<ProbeStream> {
    let mut text = String::new();
    let mut end = "stream_eof";
    let mut finish_reason = None;
    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(event) => event,
            // 已经收到的正文仍然有价值（部分答案往往正是要看的那个残缺 JSON），
            // 但流中途失败说明这次重放本身没跑完，按错误上抛让窗口说明原因。
            Err(error) => return Err(error.context("翻译模型流式响应失败")),
        };
        if let Some(reason) = event.finish_reason.as_deref() {
            finish_reason = Some(crate::ai::ai_diagnostics::finish_reason_label(reason));
        }
        if !event.tool_call_deltas.is_empty() {
            end = "tool_call";
            break;
        }
        if let Some(reason) = finish_reason
            && reason != "stop"
        {
            end = "unexpected_finish_reason";
            break;
        }
        if let Some(delta) = event.content_delta {
            if text.len().saturating_add(delta.len()) > MAX_TRANSLATION_RESPONSE_BYTES {
                end = "size_limit";
                break;
            }
            text.push_str(&delta);
        }
        if event.done {
            end = "done";
            break;
        }
    }
    Ok(ProbeStream {
        text,
        end,
        finish_reason,
    })
}

/// 收流时的静默判定：到达 [`TRANSLATION_STREAM_SILENCE_LIMIT`] 即算超时。
///
/// 单独成函数是为了把边界钉在测试里——这是「任务还能不能自己走出来」的那条线，
/// 差一毫秒就该继续等，到达就该报错并让出 `transitions` 门闩。
pub(crate) fn stream_silence_exceeded(since_last_event_ms: u64) -> bool {
    since_last_event_ms >= TRANSLATION_STREAM_SILENCE_LIMIT.as_millis() as u64
}
