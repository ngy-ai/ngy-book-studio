//! Fitting the request history, tool results and citations into budget.

pub(super) use super::*;

pub(crate) fn mark_stage(stage: &mut &'static str, next: &'static str) {
    *stage = next;
}

pub(crate) fn reduce_request_history(
    request: &mut ChatRequest,
    context: &mut RequestContext,
    exceeded: &ContextWindowExceeded,
    attempt: usize,
) -> bool {
    let reserve = exceeded
        .context_tokens
        .map_or(1024, |size| (size / 4).clamp(1, 1024));
    let previous_output = context.max_output_tokens;
    context.max_output_tokens = context.max_output_tokens.min(reserve as u32);
    request.max_tokens = Some(context.max_output_tokens);

    // Endpoint token counts are authoritative; serialized byte ratios are
    // only a hint for how much old history to omit. Different tokenizers can
    // disagree, so retries are bounded and the last retry omits all history.
    let wire_bytes =
        serde_json::to_vec(&(&request.messages, &request.tools)).map_or(0, |bytes| bytes.len());
    let bytes_to_remove = match (exceeded.prompt_tokens, exceeded.context_tokens) {
        (Some(prompt), Some(size)) if prompt > 0 => {
            let target = size.saturating_sub(reserve).saturating_mul(9) / 10;
            wire_bytes.saturating_mul(prompt.saturating_sub(target)) / prompt
        }
        _ => wire_bytes / 2,
    };
    let mut removed = 0;
    let mut removed_bytes = 0usize;
    while removed < context.history_messages
        && (removed_bytes < bytes_to_remove || attempt + 1 == MAX_CONTEXT_RETRIES)
    {
        // Drop one complete old exchange (or a leading orphan assistant from
        // a bounded persisted history), stopping before the next user turn.
        loop {
            removed_bytes = removed_bytes.saturating_add(
                serde_json::to_vec(&request.messages[1 + removed]).map_or(0, |bytes| bytes.len()),
            );
            removed += 1;
            if removed == context.history_messages
                || request.messages[1 + removed].role == ChatRole::User
            {
                break;
            }
        }
    }
    if removed > 0 {
        request.messages.drain(1..1 + removed);
        context.history_messages -= removed;
    }
    let reduced_sources = reduce_tool_results(
        &mut request.messages,
        bytes_to_remove.saturating_sub(removed_bytes),
    );
    if removed > 0 || reduced_sources {
        return true;
    }
    // Lowering the output budget cannot repair an input that already exceeds
    // the entire window. Keep the current question, exact frozen selections,
    // system policy and all current tool-call/result pairs intact and explain
    // that the user must shorten their input or enlarge the server window.
    previous_output != context.max_output_tokens
        && !matches!(
            (exceeded.prompt_tokens, exceeded.context_tokens),
            (Some(prompt), Some(size)) if prompt >= size
        )
}

/// Omit lower-ranked, whole passages only. Keep each tool call/result pair,
/// valid JSON, and at least one exact passage per result. Never truncate the
/// question, frozen selection, safety policy, or an individual source quote.
pub(crate) fn reduce_tool_results(messages: &mut [ChatMessage], bytes_to_remove: usize) -> bool {
    let mut removed = 0usize;
    while removed < bytes_to_remove {
        let candidate = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.role == ChatRole::Tool)
            .filter_map(|(index, message)| {
                let Some(MessageContent::Text(content)) = &message.content else {
                    return None;
                };
                let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
                let tool = value.get("tool")?.as_str()?;
                if !matches!(tool, "search_books" | "read_passages")
                    || value.get("results")?.as_array()?.len() <= 1
                {
                    return None;
                }
                Some((index, content.len(), value))
            })
            .max_by_key(|(_, bytes, _)| *bytes);
        let Some((index, original_bytes, mut value)) = candidate else {
            break;
        };
        value["results"].as_array_mut().unwrap().pop();
        value["truncated"] = serde_json::Value::Bool(true);
        let Ok(content) = serde_json::to_string(&value) else {
            break;
        };
        if content.len() >= original_bytes {
            break;
        }
        removed += original_bytes - content.len();
        messages[index].content = Some(MessageContent::Text(content));
    }
    removed > 0
}

pub(crate) fn retain_request_citations(
    citations: &mut CitationRegistry,
    messages: &[ChatMessage],
) -> Result<()> {
    let mut markers = HashSet::new();
    for message in messages
        .iter()
        .filter(|message| message.role == ChatRole::Tool)
    {
        let Some(MessageContent::Text(content)) = &message.content else {
            continue;
        };
        let value: serde_json::Value =
            serde_json::from_str(content).context("host tool result is not valid JSON")?;
        if let Some(results) = value.get("results").and_then(serde_json::Value::as_array) {
            for result in results {
                if let Some(marker) = result
                    .get("citation_id")
                    .and_then(serde_json::Value::as_str)
                {
                    markers.insert(marker.to_string());
                }
            }
        }
    }
    citations.retain_passage_markers(&markers);
    Ok(())
}
