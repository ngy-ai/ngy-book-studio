//! Accumulating streamed assistant turns and tool call deltas.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct PartialToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// Accumulates fragmented `delta.tool_calls` fields from an SSE completion.
pub struct ToolCallDeltaAccumulator {
    pub(crate) calls: BTreeMap<usize, PartialToolCall>,
    pub(crate) max_calls: usize,
    pub(crate) max_argument_bytes: usize,
    pub(crate) argument_bytes: usize,
}

impl ToolCallDeltaAccumulator {
    pub fn new(max_calls: usize, max_argument_bytes: usize) -> AgentResult<Self> {
        if max_calls == 0 || max_calls > 32 {
            return Err(AgentError::InvalidConfiguration(
                "stream max_calls must be between 1 and 32".to_string(),
            ));
        }
        if !(2..=1024 * 1024).contains(&max_argument_bytes) {
            return Err(AgentError::InvalidConfiguration(
                "stream max_argument_bytes must be between 2 and 1048576".to_string(),
            ));
        }
        Ok(Self {
            calls: BTreeMap::new(),
            max_calls,
            max_argument_bytes,
            argument_bytes: 0,
        })
    }

    pub fn push_event(&mut self, event: &ChatStreamEvent) -> AgentResult<()> {
        for delta in &event.tool_call_deltas {
            self.push_delta(delta)?;
        }
        Ok(())
    }

    pub fn push_delta(&mut self, delta: &ToolCallDelta) -> AgentResult<()> {
        if delta.index >= self.max_calls {
            return Err(AgentError::LimitExceeded("streamed tool call count"));
        }
        let call = self.calls.entry(delta.index).or_insert(PartialToolCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
        if let Some(id) = delta.id.as_deref() {
            merge_stream_field(&mut call.id, id, 512, "tool call ID")?;
        }
        if let Some(name) = delta.name.as_deref() {
            merge_stream_field(&mut call.name, name, 128, "tool name")?;
        }
        if let Some(arguments) = delta.arguments_delta.as_deref() {
            self.argument_bytes = self
                .argument_bytes
                .checked_add(arguments.len())
                .ok_or(AgentError::LimitExceeded("streamed tool argument size"))?;
            if self.argument_bytes > self.max_argument_bytes {
                return Err(AgentError::LimitExceeded("streamed tool argument size"));
            }
            call.arguments.push_str(arguments);
        }
        Ok(())
    }

    pub fn finish(self) -> AgentResult<Vec<ToolCall>> {
        let mut output = Vec::with_capacity(self.calls.len());
        for (_, call) in self.calls {
            validate_identifier("tool_call_id", &call.id)
                .map_err(|error| AgentError::StreamProtocol(error.to_string()))?;
            validate_identifier("tool_name", &call.name)
                .map_err(|error| AgentError::StreamProtocol(error.to_string()))?;
            let arguments = if call.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                call.arguments
            };
            let parsed: Value = serde_json::from_str(&arguments).map_err(|error| {
                tracing::warn!(target: "ngy_ai", stage = "tool_arguments_json",
                    tool = crate::ai::ai_diagnostics::tool_label(&call.name),
                    argument_bytes = arguments.len(), json_category = ?error.classify(),
                    json_line = error.line(), json_column = error.column(),
                    "AI streamed tool arguments are invalid JSON");
                AgentError::StreamProtocol(format!("tool arguments are not valid JSON: {error}"))
            })?;
            if !parsed.is_object() {
                tracing::warn!(target: "ngy_ai", stage = "tool_arguments_shape",
                    tool = crate::ai::ai_diagnostics::tool_label(&call.name),
                    arguments = ?crate::ai::ai_diagnostics::ToolArgumentsSummary::new(&arguments),
                    "AI streamed tool arguments are not an object");
                return Err(AgentError::StreamProtocol(
                    "tool arguments must be a JSON object".to_string(),
                ));
            }
            output.push(ToolCall {
                id: call.id,
                kind: "function".to_string(),
                function: FunctionCall {
                    name: call.name,
                    arguments,
                },
            });
        }
        Ok(output)
    }
}

pub(crate) fn merge_stream_field(
    destination: &mut String,
    fragment: &str,
    max_bytes: usize,
    field: &str,
) -> AgentResult<()> {
    if destination == fragment || destination.starts_with(fragment) {
        return Ok(());
    }
    if fragment.starts_with(destination.as_str()) {
        destination.clear();
        destination.push_str(fragment);
    } else {
        destination.push_str(fragment);
    }
    if destination.len() > max_bytes {
        return Err(AgentError::StreamProtocol(format!(
            "{field} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssistantTurn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub completed: bool,
}

/// Collects both answer text and tool-call fragments from one SSE response.
pub struct AgentStreamAccumulator {
    pub(crate) content: String,
    pub(crate) max_content_bytes: usize,
    pub(crate) tool_calls: ToolCallDeltaAccumulator,
    pub(crate) finish_reason: Option<String>,
    pub(crate) usage: Option<Usage>,
    pub(crate) completed: bool,
}

impl AgentStreamAccumulator {
    pub fn new(
        max_content_bytes: usize,
        max_tool_calls: usize,
        max_tool_argument_bytes: usize,
    ) -> AgentResult<Self> {
        if max_content_bytes == 0 || max_content_bytes > 16 * 1024 * 1024 {
            return Err(AgentError::InvalidConfiguration(
                "max_content_bytes must be between 1 and 16777216".to_string(),
            ));
        }
        Ok(Self {
            content: String::new(),
            max_content_bytes,
            tool_calls: ToolCallDeltaAccumulator::new(max_tool_calls, max_tool_argument_bytes)?,
            finish_reason: None,
            usage: None,
            completed: false,
        })
    }

    pub fn push(&mut self, event: &ChatStreamEvent) -> AgentResult<()> {
        if let Some(delta) = event.content_delta.as_deref() {
            if self.content.len().saturating_add(delta.len()) > self.max_content_bytes {
                return Err(AgentError::LimitExceeded("answer size"));
            }
            self.content.push_str(delta);
        }
        self.tool_calls.push_event(event)?;
        if event.finish_reason.is_some() {
            self.finish_reason = event.finish_reason.clone();
        }
        if event.usage.is_some() {
            self.usage = event.usage.clone();
        }
        self.completed |= event.done;
        Ok(())
    }

    pub fn finish(self) -> AgentResult<AssistantTurn> {
        Ok(AssistantTurn {
            content: self.content,
            tool_calls: self.tool_calls.finish()?,
            finish_reason: self.finish_reason,
            usage: self.usage,
            completed: self.completed,
        })
    }
}
