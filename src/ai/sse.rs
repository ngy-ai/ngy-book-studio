//! Bounding and reassembling the server-sent event stream.

use super::*;

pub(crate) struct BoundedSseState<S, T> {
    pub(crate) input: Pin<Box<S>>,
    pub(crate) chunk: Option<T>,
    pub(crate) chunk_offset: usize,
    pub(crate) line: Vec<u8>,
    pub(crate) data: Vec<u8>,
    pub(crate) event_wire_bytes: usize,
    pub(crate) total_bytes: usize,
    pub(crate) event_count: usize,
    pub(crate) eof: bool,
    pub(crate) terminated: bool,
    pub(crate) limits: SseLimits,
    /// Maximum silence between two transport chunks. The reply is not bounded by
    /// a total deadline: a slow model that keeps producing data must be allowed
    /// to finish, while a silent connection still fails.
    pub(crate) idle_timeout: Duration,
}

impl<S, T> BoundedSseState<S, T>
where
    S: Stream<Item = std::result::Result<T, reqwest::Error>> + Send + 'static,
    T: AsRef<[u8]> + Send + 'static,
{
    pub(super) fn new(input: S, limits: SseLimits, idle_timeout: Duration) -> Self {
        Self {
            input: Box::pin(input),
            chunk: None,
            chunk_offset: 0,
            line: Vec::new(),
            data: Vec::new(),
            event_wire_bytes: 0,
            total_bytes: 0,
            event_count: 0,
            eof: false,
            terminated: false,
            limits,
            idle_timeout,
        }
    }

    pub(super) async fn next_event(&mut self) -> Result<Option<ChatStreamEvent>> {
        if self.terminated {
            return Ok(None);
        }
        loop {
            if let Some(byte) = self.next_buffered_byte() {
                if let Some(data) = self.push_byte(byte)? {
                    return self.parse_event(data).map(Some);
                }
                continue;
            }
            if self.eof {
                let Some(data) = self.finish_eof()? else {
                    return Ok(None);
                };
                return self.parse_event(data).map(Some);
            }
            match tokio::time::timeout(self.idle_timeout, self.input.next()).await {
                Err(_) => {
                    return Err(anyhow::Error::new(ProviderTimeout::stream(
                        self.idle_timeout.as_secs(),
                    )))
                    .context("failed to read AI chat stream");
                }
                Ok(Some(Ok(chunk))) => {
                    let next_total = self
                        .total_bytes
                        .checked_add(chunk.as_ref().len())
                        .context("AI chat stream size overflow")?;
                    ensure!(
                        next_total <= self.limits.total_bytes,
                        "AI chat stream exceeds the {}-byte limit",
                        self.limits.total_bytes
                    );
                    self.total_bytes = next_total;
                    self.chunk = Some(chunk);
                    self.chunk_offset = 0;
                }
                Ok(Some(Err(error))) => {
                    return Err(error).context("failed to read AI chat stream");
                }
                Ok(None) => self.eof = true,
            }
        }
    }

    pub(super) fn next_buffered_byte(&mut self) -> Option<u8> {
        let chunk = self.chunk.as_ref()?;
        let bytes = chunk.as_ref();
        if self.chunk_offset >= bytes.len() {
            self.chunk = None;
            self.chunk_offset = 0;
            return None;
        }
        let byte = bytes[self.chunk_offset];
        self.chunk_offset += 1;
        Some(byte)
    }

    pub(super) fn push_byte(&mut self, byte: u8) -> Result<Option<Vec<u8>>> {
        if byte != b'\n' {
            self.line.push(byte);
            ensure!(
                self.line.len() <= self.limits.event_bytes,
                "AI chat event exceeds the {}-byte limit",
                self.limits.event_bytes
            );
            return Ok(None);
        }

        let line_len = self.line.len();
        let line_without_cr = self
            .line
            .strip_suffix(b"\r")
            .unwrap_or(self.line.as_slice());
        if line_without_cr.is_empty() {
            self.line.clear();
            self.event_wire_bytes = 0;
            if self.data.is_empty() {
                return Ok(None);
            }
            return Ok(Some(std::mem::take(&mut self.data)));
        }

        self.event_wire_bytes = self
            .event_wire_bytes
            .checked_add(line_len + 1)
            .context("AI chat event size overflow")?;
        ensure!(
            self.event_wire_bytes <= self.limits.event_bytes,
            "AI chat event exceeds the {}-byte limit",
            self.limits.event_bytes
        );
        append_sse_data_line(&mut self.data, line_without_cr, self.limits.event_bytes)?;
        self.line.clear();
        Ok(None)
    }

    pub(super) fn finish_eof(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.line.is_empty() {
            let line_len = self.line.len();
            let line_without_cr = self
                .line
                .strip_suffix(b"\r")
                .unwrap_or(self.line.as_slice());
            self.event_wire_bytes = self
                .event_wire_bytes
                .checked_add(line_len)
                .context("AI chat event size overflow")?;
            ensure!(
                self.event_wire_bytes <= self.limits.event_bytes,
                "AI chat event exceeds the {}-byte limit",
                self.limits.event_bytes
            );
            append_sse_data_line(&mut self.data, line_without_cr, self.limits.event_bytes)?;
            self.line.clear();
        }
        self.event_wire_bytes = 0;
        Ok((!self.data.is_empty()).then(|| std::mem::take(&mut self.data)))
    }

    pub(super) fn parse_event(&mut self, data: Vec<u8>) -> Result<ChatStreamEvent> {
        self.event_count = self
            .event_count
            .checked_add(1)
            .context("AI chat event count overflow")?;
        ensure!(
            self.event_count <= self.limits.events,
            "AI chat stream exceeds the {}-event limit",
            self.limits.events
        );
        let data = String::from_utf8(data).context("AI chat event data is not valid UTF-8")?;
        if data.trim() == "[DONE]" {
            self.terminated = true;
            return Ok(ChatStreamEvent {
                done: true,
                ..Default::default()
            });
        }
        parse_chat_stream_event(&data)
    }
}

pub(crate) fn append_sse_data_line(
    output: &mut Vec<u8>,
    line: &[u8],
    max_bytes: usize,
) -> Result<()> {
    let Some(value) = line.strip_prefix(b"data:") else {
        return Ok(());
    };
    let value = value.strip_prefix(b" ").unwrap_or(value);
    let separator = usize::from(!output.is_empty());
    let next_len = output
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(value.len()))
        .context("AI chat event data size overflow")?;
    ensure!(
        next_len <= max_bytes,
        "AI chat event exceeds the {max_bytes}-byte limit"
    );
    if separator != 0 {
        output.push(b'\n');
    }
    output.extend_from_slice(value);
    Ok(())
}

pub(crate) fn bounded_chat_sse_stream(
    response: Response,
    limits: SseLimits,
    idle_timeout: Duration,
) -> ChatEventStream {
    let state = BoundedSseState::new(response.bytes_stream(), limits, idle_timeout);
    let diagnostics = SseDiagnostics::new();
    let span = tracing::Span::current();
    let stream = stream::try_unfold((state, diagnostics), move |(mut state, mut diagnostics)| {
        let span = span.clone();
        async move {
            let result = state.next_event().await;
            match &result {
                Ok(Some(event)) => {
                    diagnostics.observe(event, state.total_bytes, state.event_count);
                    // Consumers can stop polling as soon as [DONE] arrives.
                    if event.done {
                        diagnostics.finish("done", state.total_bytes, state.event_count, None);
                    }
                }
                Ok(None) => {
                    diagnostics.finish("eof", state.total_bytes, state.event_count, None);
                }
                Err(error) => {
                    diagnostics.finish("failed", state.total_bytes, state.event_count, Some(error));
                }
            }
            Ok(result?.map(|event| (event, (state, diagnostics))))
        }
        .instrument(span)
    });
    Box::pin(stream)
}
