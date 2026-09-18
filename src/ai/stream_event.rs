//! Parsing one streamed chat event and formatting statuses.

use super::*;

pub(crate) fn display_status(status: StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => status.as_u16().to_string(),
    }
}

pub(crate) fn parse_chat_stream_event(data: &str) -> Result<ChatStreamEvent> {
    #[derive(Deserialize)]
    struct WireResponse {
        #[serde(default)]
        choices: Vec<WireChoice>,
        usage: Option<Usage>,
    }
    #[derive(Deserialize)]
    struct WireChoice {
        #[serde(default)]
        delta: WireDelta,
        finish_reason: Option<String>,
    }
    #[derive(Default, Deserialize)]
    struct WireDelta {
        content: Option<String>,
        #[serde(default)]
        tool_calls: Vec<WireToolCall>,
    }
    #[derive(Deserialize)]
    struct WireToolCall {
        index: usize,
        id: Option<String>,
        function: Option<WireFunction>,
    }
    #[derive(Deserialize)]
    struct WireFunction {
        name: Option<String>,
        arguments: Option<String>,
    }

    let body: WireResponse = serde_json::from_str(data)
        .with_context(|| format!("invalid chat completion event: {}", truncate(data, 256)))?;
    let choice = body.choices.into_iter().next();
    Ok(ChatStreamEvent {
        content_delta: choice
            .as_ref()
            .and_then(|choice| choice.delta.content.clone()),
        tool_call_deltas: choice
            .as_ref()
            .map(|choice| {
                choice
                    .delta
                    .tool_calls
                    .iter()
                    .map(|call| ToolCallDelta {
                        index: call.index,
                        id: call.id.clone(),
                        name: call
                            .function
                            .as_ref()
                            .and_then(|function| function.name.clone()),
                        arguments_delta: call
                            .function
                            .as_ref()
                            .and_then(|function| function.arguments.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        finish_reason: choice.and_then(|choice| choice.finish_reason),
        usage: body.usage,
        done: false,
    })
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
