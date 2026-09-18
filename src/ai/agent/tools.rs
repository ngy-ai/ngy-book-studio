//! The tool definitions offered to the model.

use super::*;

/// OpenAI-compatible JSON schemas for the only tools the model can invoke.
pub fn agent_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::function(
            SEARCH_BOOKS_TOOL,
            "Search passages inside the host-authorized books. book_ids can only narrow scope.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "book_ids": {
                        "type": "array", "minItems": 1, "uniqueItems": true,
                        "items": { "type": "string", "minLength": 1, "maxLength": 512 }
                    },
                    "mode": { "type": "string", "enum": ["keyword", "semantic", "hybrid"] },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                }
            }),
        ),
        ToolDefinition::function(
            READ_PASSAGES_TOOL,
            "Read complete passages previously identified by search. book_ids can only narrow scope.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["passage_ids"],
                "properties": {
                    "passage_ids": {
                        "type": "array", "minItems": 1, "maxItems": 100, "uniqueItems": true,
                        "items": { "type": "string", "minLength": 1, "maxLength": 512 }
                    },
                    "book_ids": {
                        "type": "array", "minItems": 1, "uniqueItems": true,
                        "items": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                }
            }),
        ),
        ToolDefinition::function(
            GET_OUTLINE_TOOL,
            "Get table-of-contents nodes for host-authorized books. book_ids can only narrow scope.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "book_ids": {
                        "type": "array", "minItems": 1, "uniqueItems": true,
                        "items": { "type": "string", "minLength": 1, "maxLength": 512 }
                    },
                    "max_depth": { "type": "integer", "minimum": 1, "maximum": 16 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                }
            }),
        ),
    ]
}
