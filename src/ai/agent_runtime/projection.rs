//! Projecting a streaming answer while hiding protocol markers.

pub(super) use super::*;

/// Incrementally removes the private answer protocol while retaining enough
/// trailing text to recognize a marker split across arbitrary SSE chunks.
#[derive(Debug, Default)]
pub(crate) struct StreamingAnswerProjection {
    pub(crate) pending: String,
    pub(crate) emitted: String,
}

impl StreamingAnswerProjection {
    pub(super) fn push(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut visible = String::new();

        loop {
            let Some(start) = self.pending.find(PROTOCOL_MARKER_START) else {
                let retained = trailing_marker_prefix_len(&self.pending);
                let emit_len = self.pending.len() - retained;
                visible.push_str(&self.pending[..emit_len]);
                self.pending.drain(..emit_len);
                break;
            };

            visible.push_str(&self.pending[..start]);
            self.pending.drain(..start);

            if self.pending.starts_with(NO_SOURCE_MARKER) {
                self.pending.drain(..NO_SOURCE_MARKER.len());
                continue;
            }
            if NO_SOURCE_MARKER.starts_with(&self.pending)
                || CITATION_MARKER_PREFIX.starts_with(&self.pending)
            {
                break;
            }
            if self.pending.starts_with(CITATION_MARKER_PREFIX) {
                let marker_body = &self.pending[CITATION_MARKER_PREFIX.len()..];
                let Some(end) = marker_body.find(CITATION_MARKER_SUFFIX) else {
                    break;
                };
                let marker_len = CITATION_MARKER_PREFIX.len() + end + CITATION_MARKER_SUFFIX.len();
                self.pending.drain(..marker_len);
                continue;
            }

            // `[[ngy-...` that cannot become a protocol marker is ordinary
            // answer text. Release one scalar and continue scanning so a
            // valid marker beginning at the following byte is still found.
            let scalar_len = self.pending.chars().next().map(char::len_utf8).unwrap_or(0);
            visible.push_str(&self.pending[..scalar_len]);
            self.pending.drain(..scalar_len);
        }

        self.emitted.push_str(&visible);
        visible
    }

    pub(super) fn finish(self, validated_markdown: &str) -> Result<String> {
        ensure!(
            validated_markdown.starts_with(&self.emitted),
            "streamed answer projection does not match the validated final answer"
        );
        Ok(validated_markdown[self.emitted.len()..].to_string())
    }
}

pub(crate) fn trailing_marker_prefix_len(value: &str) -> usize {
    let maximum = value.len().min(PROTOCOL_MARKER_START.len());
    (1..=maximum)
        .rev()
        .find(|length| {
            let start = value.len() - length;
            value.is_char_boundary(start) && PROTOCOL_MARKER_START.starts_with(&value[start..])
        })
        .unwrap_or(0)
}
