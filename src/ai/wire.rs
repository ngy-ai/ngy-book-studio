//! The OpenAI-compatible wire shapes: messages, tools, requests and
//! streaming events.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl From<String> for MessageContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for MessageContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl ChatMessage {
    pub fn text(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(MessageContent::Text(content.into())),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// Stable diagnostics label. `None` here means "explicitly send `none`", which
    /// is how a request turns thinking off; an omitted parameter is reported
    /// separately as `unset`, so a Debug dump like `Some(None)` never appears in
    /// the logs where it reads like "nothing was sent".
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatStreamEvent {
    pub content_delta: Option<String>,
    pub tool_call_deltas: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub done: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: Vec<String>,
    /// Optional dimension override sent to the provider. When `Some`, the
    /// request includes `"dimensions": N` so models that support it return
    /// vectors of exactly this size. `None` omits the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingBatch {
    pub model: String,
    pub vectors: Vec<Vec<f32>>,
    pub usage: Option<Usage>,
}

#[derive(Deserialize)]
pub(crate) struct WireEmbedding {
    pub(crate) index: usize,
    pub(crate) embedding: Vec<f32>,
}

#[derive(Deserialize)]
pub(crate) struct WireEmbeddingResponse {
    pub(crate) model: String,
    pub(crate) data: Vec<WireEmbedding>,
    pub(crate) usage: Option<Usage>,
}

#[derive(Clone, Copy)]
pub(crate) struct SseLimits {
    pub(crate) total_bytes: usize,
    pub(crate) event_bytes: usize,
    pub(crate) events: usize,
}

impl SseLimits {
    pub(super) const CHAT: Self = Self {
        total_bytes: MAX_CHAT_STREAM_BYTES,
        event_bytes: MAX_CHAT_EVENT_BYTES,
        events: MAX_CHAT_EVENTS,
    };
}

/// Narrow provider contract used by search, vision and the read-only agent.
pub trait OpenAiCompatibleProvider: Send + Sync {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>>;
    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>>;
    fn embeddings(&self, request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>>;
}
