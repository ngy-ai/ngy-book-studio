//! Extracting citation markers and validating the history shape.

pub(super) use super::*;

/// Extracts the only citation syntax accepted from the model. The markers are
/// removed from user-visible Markdown; the returned IDs still have to pass
/// [`CitationRegistry::finish`], so text copied from an untrusted passage
/// cannot manufacture a source that was not served by a tool or registered
/// from a host-authorized frozen selection.
pub(crate) fn extract_citation_markers(markdown: &str) -> Result<(String, Vec<String>, bool)> {
    let no_source_count = markdown.matches(NO_SOURCE_MARKER).count();
    ensure!(
        no_source_count <= 1,
        "AI answer contains more than one no-source marker"
    );
    let no_source = no_source_count == 1;
    let markdown = markdown.replace(NO_SOURCE_MARKER, "");
    let mut clean = String::with_capacity(markdown.len());
    let mut citation_ids = Vec::new();
    let mut remaining = markdown.as_str();

    while let Some(start) = remaining.find(CITATION_MARKER_PREFIX) {
        clean.push_str(&remaining[..start]);
        let marker = &remaining[start + CITATION_MARKER_PREFIX.len()..];
        let Some(end) = marker.find(CITATION_MARKER_SUFFIX) else {
            bail!("AI answer contains an incomplete citation marker");
        };
        ensure!(
            citation_ids.len() < MAX_CITATION_MARKERS,
            "AI answer exceeds {MAX_CITATION_MARKERS} citation markers"
        );
        let citation_id = &marker[..end];
        ensure!(
            !citation_id.is_empty()
                && citation_id.len() <= 512
                && citation_id
                    .chars()
                    .all(|character| !character.is_whitespace() && !character.is_control()),
            "AI answer contains an invalid citation marker"
        );
        citation_ids.push(citation_id.to_string());
        remaining = &marker[end + CITATION_MARKER_SUFFIX.len()..];
    }
    clean.push_str(remaining);
    Ok((clean, citation_ids, no_source))
}

pub(crate) fn validate_history(history: &[ChatMessage]) -> Result<()> {
    ensure!(
        history.len() <= MAX_HISTORY_MESSAGES,
        "chat history exceeds {MAX_HISTORY_MESSAGES} messages"
    );
    let mut total = 0usize;
    for message in history {
        ensure!(
            matches!(message.role, ChatRole::User | ChatRole::Assistant),
            "persisted history may contain only user and assistant messages"
        );
        ensure!(
            message.name.is_none()
                && message.tool_call_id.is_none()
                && message.tool_calls.is_empty(),
            "persisted history cannot inject tool protocol fields"
        );
        let Some(MessageContent::Text(content)) = message.content.as_ref() else {
            bail!("persisted history must contain plain text");
        };
        total = total
            .checked_add(content.len())
            .context("chat history size overflow")?;
    }
    ensure!(
        total <= MAX_HISTORY_BYTES,
        "chat history exceeds {MAX_HISTORY_BYTES} bytes"
    );
    Ok(())
}
