//! Content-free diagnostics shared by the AI request layers.
//!
//! Never format arbitrary errors, requests, tool arguments or endpoint URLs into
//! logs: providers may echo prompts or credentials in those values.

use crate::{
    agent::AgentError,
    agent_runtime::AgentRequestCancelled,
    ai::{ContextWindowExceeded, IncompleteToolArguments},
};

/// Labels such as model IDs are bounded and single-line. This is not a general
/// secret scrubber; callers must never pass credentials or content to it.
pub fn safe_label(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'/'))
    {
        value.to_string()
    } else {
        "<redacted>".to_string()
    }
}

/// Keep model-supplied unknown names out of logs, including argument errors.
pub fn tool_label(value: &str) -> &'static str {
    match value {
        "search_books" => "search_books",
        "read_passages" => "read_passages",
        "get_outline" => "get_outline",
        _ => "unknown_tool",
    }
}

pub fn finish_reason_label(value: &str) -> &'static str {
    match value {
        "stop" => "stop",
        "length" => "length",
        "tool_calls" => "tool_calls",
        "content_filter" => "content_filter",
        "function_call" => "function_call",
        _ => "other",
    }
}

pub fn error_kind(error: &anyhow::Error) -> &'static str {
    if error.is::<AgentRequestCancelled>() {
        return "cancelled";
    }
    if error.is::<ContextWindowExceeded>() {
        return "context_window_exceeded";
    }
    if error.is::<IncompleteToolArguments>() {
        return "incomplete_tool_arguments";
    }
    if let Some(error) = error.downcast_ref::<AgentError>() {
        return match error {
            AgentError::InvalidConfiguration(_) => "agent_configuration",
            AgentError::InvalidArguments(_) => "tool_arguments",
            AgentError::ScopeViolation => "scope_violation",
            AgentError::LimitExceeded(_) => "agent_limit",
            AgentError::ToolTimeout => "tool_timeout",
            AgentError::Backend(_) => "tool_backend",
            AgentError::StreamProtocol(_) => "stream_protocol",
            AgentError::UnknownCitation(_) => "unknown_citation",
        };
    }
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        return if error.is_timeout() {
            "http_timeout"
        } else if error.is_connect() {
            "http_connect"
        } else if error.is_body() {
            "http_body"
        } else if error.is_decode() {
            "http_decode"
        } else {
            "http_transport"
        };
    }
    if let Some(error) = error.downcast_ref::<serde_json::Error>() {
        return match error.classify() {
            serde_json::error::Category::Io => "json_io",
            serde_json::error::Category::Syntax => "json_syntax",
            serde_json::error::Category::Data => "json_data",
            serde_json::error::Category::Eof => "json_eof",
        };
    }
    if error.is::<rusqlite::Error>() {
        return "database";
    }
    if error.is::<std::io::Error>() {
        return "io";
    }
    "unclassified"
}

/// Shape and sizes only; JSON field names and string values can contain book
/// text, IDs or instructions, so none of them are included in the output.
#[derive(Debug)]
pub struct ToolArgumentsSummary {
    pub bytes: usize,
    pub json_kind: &'static str,
    pub fields: usize,
    pub passage_ids: Option<usize>,
    pub book_ids: Option<usize>,
    pub query_bytes: Option<usize>,
    pub error_category: Option<serde_json::error::Category>,
    pub error_line: Option<usize>,
    pub error_column: Option<usize>,
}

impl ToolArgumentsSummary {
    pub fn new(arguments: &str) -> Self {
        let mut summary = Self {
            bytes: arguments.len(),
            json_kind: "invalid",
            fields: 0,
            passage_ids: None,
            book_ids: None,
            query_bytes: None,
            error_category: None,
            error_line: None,
            error_column: None,
        };
        match serde_json::from_str::<serde_json::Value>(arguments) {
            Ok(value) => {
                summary.json_kind = match &value {
                    serde_json::Value::Object(_) => "object",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::Bool(_) => "boolean",
                    serde_json::Value::Null => "null",
                };
                summary.fields = value.as_object().map_or(0, |object| object.len());
                summary.passage_ids = value
                    .get("passage_ids")
                    .and_then(|v| v.as_array())
                    .map(Vec::len);
                summary.book_ids = value
                    .get("book_ids")
                    .and_then(|v| v.as_array())
                    .map(Vec::len);
                summary.query_bytes = value.get("query").and_then(|v| v.as_str()).map(str::len);
            }
            Err(error) => {
                summary.error_category = Some(error.classify());
                summary.error_line = Some(error.line());
                summary.error_column = Some(error.column());
            }
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_cancellation_preserves_display_and_context_classification() {
        let error = anyhow::Error::new(AgentRequestCancelled);
        assert_eq!(error.to_string(), "AI request was cancelled");
        assert_eq!(error_kind(&error), "cancelled");
        let wrapped = error.context("frozen selection authorization worker stopped");
        assert!(wrapped.is::<AgentRequestCancelled>());
        assert_eq!(error_kind(&wrapped), "cancelled");
        assert_eq!(
            error_kind(&anyhow::anyhow!("AI request was cancelled")),
            "unclassified",
            "arbitrary backend text must not masquerade as host cancellation"
        );
    }

    #[test]
    fn diagnostics_keep_structure_without_content_or_unknown_tool_names() {
        let summary = ToolArgumentsSummary::new(
            r#"{"query":"PRIVATE_BODY","passage_ids":["PRIVATE_ID"],"PRIVATE_KEY":"PRIVATE_VALUE"}"#,
        );
        assert_eq!(summary.json_kind, "object");
        assert_eq!(summary.fields, 3);
        assert_eq!(summary.passage_ids, Some(1));
        assert_eq!(summary.query_bytes, Some(12));
        assert!(!format!("{summary:?}").contains("PRIVATE"));
        let incomplete = ToolArgumentsSummary::new("{\n\"passage_ids\": [\"PRIVATE_ID\"");
        assert!(matches!(
            incomplete.error_category,
            Some(serde_json::error::Category::Eof)
        ));
        assert_eq!(incomplete.error_line, Some(2));
        assert!(!format!("{incomplete:?}").contains("PRIVATE"));
        assert_eq!(tool_label("PRIVATE_TOOL"), "unknown_tool");
        assert_eq!(finish_reason_label("PRIVATE_REASON"), "other");
        assert_eq!(finish_reason_label("length"), "length");
        assert_eq!(safe_label("model:7b"), "model:7b");
        assert_eq!(safe_label("label\nforged log"), "<redacted>");
        assert_eq!(safe_label("http://host/path?key=PRIVATE"), "<redacted>");
        assert_eq!(safe_label(&"x".repeat(97)), "<redacted>");
    }
}
