//! Diagnostics collected while a stream is consumed.

use super::*;

/// Constant-size stream diagnostics. No event payloads or generated arguments
/// are retained here, and there is no log entry for each token/chunk. One
/// progress line every [`STREAM_PROGRESS_INTERVAL`] describes the *transport*
/// (chunks, bytes, gaps) so a healthy but slow generation can be told apart
/// from a stalled or silent one.
pub(crate) struct SseDiagnostics {
    pub(crate) started_at: Instant,
    pub(crate) first_event_ms: Option<u64>,
    pub(crate) content_bytes: usize,
    /// Chunks that carried answer text.
    pub(crate) content_events: usize,
    /// Chunks whose `content` delta was present but empty. Providers that hide a
    /// hidden reasoning trace server-side (for example an Ollama thinking model
    /// on the OpenAI-compatible endpoint) stream a long run of these without any
    /// error, so they must be distinguishable from a slow but healthy answer.
    pub(crate) empty_content_events: usize,
    /// Chunks that carried nothing this client consumes: no content delta, no
    /// tool call, no finish reason and no usage. A provider-specific reasoning
    /// field would be counted here instead of being silently discarded.
    pub(crate) unrecognized_events: usize,
    pub(crate) tool_delta_count: usize,
    pub(crate) tool_argument_bytes: usize,
    pub(crate) finish_reason: Option<&'static str>,
    pub(crate) usage: Option<Usage>,
    pub(crate) finished: bool,
    pub(crate) last_event_at: Instant,
    /// Largest silence between two SSE events, in milliseconds.
    pub(crate) max_event_gap_ms: u64,
    pub(crate) next_progress_at: Duration,
}

impl SseDiagnostics {
    pub(super) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            first_event_ms: None,
            content_bytes: 0,
            content_events: 0,
            empty_content_events: 0,
            unrecognized_events: 0,
            tool_delta_count: 0,
            tool_argument_bytes: 0,
            finish_reason: None,
            usage: None,
            finished: false,
            last_event_at: Instant::now(),
            max_event_gap_ms: 0,
            next_progress_at: STREAM_PROGRESS_INTERVAL,
        }
    }

    pub(super) fn observe(
        &mut self,
        event: &ChatStreamEvent,
        wire_bytes: usize,
        event_count: usize,
    ) {
        let now = Instant::now();
        let since_last_event_ms = (now - self.last_event_at).as_millis() as u64;
        self.max_event_gap_ms = self.max_event_gap_ms.max(since_last_event_ms);
        self.last_event_at = now;
        let elapsed = now - self.started_at;
        self.first_event_ms
            .get_or_insert(elapsed.as_millis() as u64);
        match event.content_delta.as_deref() {
            Some("") => {
                self.empty_content_events = self.empty_content_events.saturating_add(1);
            }
            Some(delta) => {
                self.content_events = self.content_events.saturating_add(1);
                self.content_bytes = self.content_bytes.saturating_add(delta.len());
            }
            None => {}
        }
        if event.content_delta.is_none()
            && !event.done
            && event.tool_call_deltas.is_empty()
            && event.finish_reason.is_none()
            && event.usage.is_none()
        {
            self.unrecognized_events = self.unrecognized_events.saturating_add(1);
        }
        self.tool_delta_count = self
            .tool_delta_count
            .saturating_add(event.tool_call_deltas.len());
        for delta in &event.tool_call_deltas {
            self.tool_argument_bytes = self
                .tool_argument_bytes
                .saturating_add(delta.arguments_delta.as_ref().map_or(0, String::len));
        }
        if let Some(reason) = &event.finish_reason {
            self.finish_reason = Some(finish_reason_label(reason));
        }
        if let Some(usage) = &event.usage {
            self.usage = Some(usage.clone());
        }
        if elapsed >= self.next_progress_at {
            self.next_progress_at = elapsed + STREAM_PROGRESS_INTERVAL;
            tracing::debug!(
                target: "ngy_ai",
                stage = "sse_progress",
                wire_bytes,
                event_count,
                content_bytes = self.content_bytes,
                content_events = self.content_events,
                empty_content_events = self.empty_content_events,
                unrecognized_events = self.unrecognized_events,
                tool_delta_count = self.tool_delta_count,
                since_last_event_ms,
                max_event_gap_ms = self.max_event_gap_ms,
                finish_reason = ?self.finish_reason,
                elapsed_ms = elapsed.as_millis() as u64,
                "AI response stream progress"
            );
        }
    }

    pub(super) fn finish(
        &mut self,
        outcome: &'static str,
        wire_bytes: usize,
        event_count: usize,
        error: Option<&anyhow::Error>,
    ) {
        if self.finished {
            return;
        }
        self.finished = true;
        let json_error = error.and_then(|error| {
            error
                .chain()
                .find_map(|cause| cause.downcast_ref::<serde_json::Error>())
        });
        if let Some(error) = error {
            tracing::warn!(
                target: "ngy_ai",
                stage = "sse_failed",
                error_kind = error_kind(error),
                json_error_category = ?json_error.map(serde_json::Error::classify),
                json_error_line = json_error.map(serde_json::Error::line),
                json_error_column = json_error.map(serde_json::Error::column),
                wire_bytes,
                event_count,
                content_bytes = self.content_bytes,
                content_events = self.content_events,
                empty_content_events = self.empty_content_events,
                unrecognized_events = self.unrecognized_events,
                max_event_gap_ms = self.max_event_gap_ms,
                finish_reason = ?self.finish_reason,
                elapsed_ms = self.started_at.elapsed().as_millis() as u64,
                "AI response stream failed"
            );
        }
        tracing::debug!(
            target: "ngy_ai",
            stage = "sse_finished",
            outcome,
            wire_bytes,
            event_count,
            content_bytes = self.content_bytes,
            content_events = self.content_events,
            empty_content_events = self.empty_content_events,
            unrecognized_events = self.unrecognized_events,
            tool_delta_count = self.tool_delta_count,
            tool_argument_bytes = self.tool_argument_bytes,
            finish_reason = ?self.finish_reason,
            first_event_ms = self.first_event_ms,
            max_event_gap_ms = self.max_event_gap_ms,
            elapsed_ms = self.started_at.elapsed().as_millis() as u64,
            prompt_tokens = self.usage.as_ref().map(|usage| usage.prompt_tokens),
            completion_tokens = self.usage.as_ref().map(|usage| usage.completion_tokens),
            total_tokens = self.usage.as_ref().map(|usage| usage.total_tokens),
            "AI response stream summary"
        );
    }
}
