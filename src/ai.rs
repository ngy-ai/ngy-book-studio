//! OpenAI-compatible model access.
//!
//! The domain and UI only depend on [`OpenAiCompatibleProvider`].  In
//! particular, nothing in this module assumes that the endpoint is Ollama;
//! Ollama is merely the safe, loopback default.

use std::{
    fmt,
    net::IpAddr,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::{FutureExt as _, Stream, StreamExt as _, future::BoxFuture, stream};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;

use crate::ai_diagnostics::{error_kind, finish_reason_label, safe_label, tool_label};

pub const DEFAULT_OLLAMA_OPENAI_BASE_URL: &str = "http://127.0.0.1:11434/v1/";
pub const DEFAULT_AI_REQUEST_TIMEOUT_SECS: u64 = 120;
pub const MIN_AI_REQUEST_TIMEOUT_SECS: u64 = 1;
pub const MAX_AI_REQUEST_TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_CHAT_OUTPUT_TOKENS: u32 = 4096;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_ERROR_DEPTH: usize = 8;
const MAX_MODELS_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODEL_ENTRIES: usize = 4096;
const MAX_MODEL_FIELD_BYTES: usize = 4096;
const MAX_EMBEDDINGS_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_EMBEDDING_COUNT: usize = 2048;
pub const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;
const MAX_CHAT_STREAM_BYTES: usize = 32 * 1024 * 1024;
const MAX_CHAT_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHAT_EVENTS: usize = 65_536;
static NEXT_HTTP_ID: AtomicU64 = AtomicU64::new(1);

pub type ChatEventStream = Pin<Box<dyn Stream<Item = Result<ChatStreamEvent>> + Send>>;

/// A provider rejection received before a completion stream starts. Token
/// counts describe the endpoint's tokenizer and active context configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextWindowExceeded {
    pub prompt_tokens: Option<usize>,
    pub context_tokens: Option<usize>,
}

impl fmt::Display for ContextWindowExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "模型服务的上下文容量不足")?;
        match (self.prompt_tokens, self.context_tokens) {
            (Some(prompt), Some(context)) => {
                write!(formatter, "（输入 {prompt} tokens，上限 {context} tokens）")?;
            }
            (_, Some(context)) => write!(formatter, "（上限 {context} tokens）")?,
            (Some(prompt), _) => write!(formatter, "（输入 {prompt} tokens）")?,
            (None, None) => {}
        }
        write!(
            formatter,
            "。请缩短问题或选区、开启新对话，或在模型服务中增大上下文容量后重试。"
        )
    }
}

impl std::error::Error for ContextWindowExceeded {}

/// The endpoint rejected model-generated arguments before starting SSE. This
/// never authorizes the host to complete or execute the malformed arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncompleteToolArguments {
    pub tool_name: String,
}

impl fmt::Display for IncompleteToolArguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "模型服务生成的 {} 工具参数不是完整 JSON（HTTP 500，unexpected end of JSON input）。请在 AI 设置中切换到支持工具调用的模型，或检查模型服务的工具调用模板与输出长度设置后重试。",
            self.tool_name
        )
    }
}

impl std::error::Error for IncompleteToolArguments {}

/// Optional sampling controls are omitted from requests when unset so the
/// endpoint can use its own defaults. The user chooses a positive output limit
/// appropriate for the model; the host does not impose a model-specific ceiling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatGenerationSettings {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: u32,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
}

impl Default for ChatGenerationSettings {
    fn default() -> Self {
        Self {
            temperature: Some(0.1),
            top_p: None,
            max_output_tokens: DEFAULT_CHAT_OUTPUT_TOKENS,
            presence_penalty: None,
            frequency_penalty: None,
        }
    }
}

impl ChatGenerationSettings {
    pub fn validate(&self) -> Result<()> {
        for (label, value, minimum, maximum) in [
            ("温度（Temperature）", self.temperature, 0.0, 2.0),
            ("核采样（Top P）", self.top_p, 0.0, 1.0),
            (
                "出现惩罚（Presence Penalty）",
                self.presence_penalty,
                -2.0,
                2.0,
            ),
            (
                "频率惩罚（Frequency Penalty）",
                self.frequency_penalty,
                -2.0,
                2.0,
            ),
        ] {
            ensure!(
                value.is_none_or(|value| value.is_finite() && (minimum..=maximum).contains(&value)),
                "{label} 必须是 {minimum}～{maximum} 之间的有限数字，或留空使用服务默认值"
            );
        }
        ensure!(self.max_output_tokens > 0, "最大输出 token 数必须是正整数");
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub remote_content_confirmed: bool,
    pub allow_insecure_remote_http: bool,
    pub request_timeout_secs: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_OPENAI_BASE_URL.to_string(),
            api_key: None,
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            request_timeout_secs: DEFAULT_AI_REQUEST_TIMEOUT_SECS,
        }
    }
}

impl ProviderConfig {
    pub fn validated_base_url(&self) -> Result<Url> {
        let url = normalize_provider_base_url(&self.base_url)?;

        let loopback = endpoint_is_loopback(&url);
        if !loopback && !self.remote_content_confirmed {
            bail!("sending book content to a remote AI endpoint has not been confirmed");
        }
        if !loopback && url.scheme() != "https" && !self.allow_insecure_remote_http {
            bail!("remote AI endpoints must use HTTPS unless insecure HTTP is explicitly allowed");
        }
        if !(MIN_AI_REQUEST_TIMEOUT_SECS..=MAX_AI_REQUEST_TIMEOUT_SECS)
            .contains(&self.request_timeout_secs)
        {
            bail!(
                "AI request timeout must be between {MIN_AI_REQUEST_TIMEOUT_SECS} and {MAX_AI_REQUEST_TIMEOUT_SECS} seconds"
            );
        }

        Ok(url)
    }

    pub fn is_remote(&self) -> Result<bool> {
        Ok(!endpoint_is_loopback(&self.validated_base_url()?))
    }
}

/// Validates and canonicalizes the endpoint identity without granting either
/// remote-content or insecure-transport permission. Persisted confirmations
/// bind to this exact value so changing endpoint A to endpoint B cannot reuse
/// a checkbox that was accepted for A.
pub fn normalize_provider_base_url(base_url: &str) -> Result<Url> {
    let mut url = Url::parse(base_url).context("AI endpoint is not a valid URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("AI endpoint must use http or https");
    }
    if url.host_str().is_none() {
        bail!("AI endpoint must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("AI endpoint credentials must not be embedded in the URL");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("AI endpoint must not contain a query or fragment");
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn endpoint_is_loopback(url: &Url) -> bool {
    match url.host_str().map(|host| host.trim_matches(['[', ']'])) {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

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
struct WireEmbedding {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct WireEmbeddingResponse {
    model: String,
    data: Vec<WireEmbedding>,
    usage: Option<Usage>,
}

#[derive(Clone, Copy)]
struct SseLimits {
    total_bytes: usize,
    event_bytes: usize,
    events: usize,
}

impl SseLimits {
    const CHAT: Self = Self {
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

#[derive(Clone)]
pub struct OpenAiHttpProvider {
    config: ProviderConfig,
    client: Client,
    base_url: Url,
}

impl std::fmt::Debug for OpenAiHttpProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiHttpProvider")
            .field("base_url", &self.base_url)
            .field("is_remote", &!endpoint_is_loopback(&self.base_url))
            .field("has_api_key", &self.config.api_key.is_some())
            .finish_non_exhaustive()
    }
}

impl OpenAiHttpProvider {
    pub fn new(config: ProviderConfig) -> Result<Self> {
        let base_url = config.validated_base_url()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .build()
            .context("failed to build AI HTTP client")?;
        Ok(Self {
            config,
            client,
            base_url,
        })
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn endpoint(&self, relative: &str) -> Result<Url> {
        self.base_url
            .join(relative)
            .with_context(|| format!("failed to resolve AI endpoint {relative}"))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.config.api_key.as_deref() {
            Some(key) if !key.is_empty() => request.bearer_auth(key),
            _ => request,
        }
    }
}

impl OpenAiCompatibleProvider for OpenAiHttpProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async move {
            #[derive(Deserialize)]
            struct ModelsResponse {
                data: Vec<ModelInfo>,
            }

            let response = self
                .authorized(self.client.get(self.endpoint("models")?))
                .send()
                .await
                .context("failed to query AI models")?;
            let response = checked_response(response).await?;
            let bytes = read_success_body_limited(
                response,
                MAX_MODELS_RESPONSE_BYTES,
                "AI models response",
            )
            .await?;
            let data = serde_json::from_slice::<ModelsResponse>(&bytes)
                .context("AI models response is not valid OpenAI-compatible JSON")?
                .data;
            validate_and_normalize_models(data)
        }
        .boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        // Record only the endpoint origin: paths can contain tenant names or
        // credentials even when query strings and userinfo are prohibited.
        let span = tracing::info_span!(
            target: "moye_ai",
            "ai_http",
            http_id = NEXT_HTTP_ID.fetch_add(1, Ordering::Relaxed),
            operation = "chat_stream",
            model = %safe_label(&request.model),
            endpoint_scheme = self.base_url.scheme(),
            endpoint_host = self.base_url.host_str().unwrap_or(""),
            endpoint_port = self.base_url.port_or_known_default(),
            timeout_secs = self.config.request_timeout_secs,
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            max_tokens = request.max_tokens,
            temperature = request.temperature,
            reasoning_effort = ?request.reasoning_effort,
            request_body_bytes = tracing::field::Empty,
        );
        async move {
            let started_at = Instant::now();
            let result = async {
                if request.model.trim().is_empty() {
                    bail!("chat model is required");
                }
                if request.messages.is_empty() {
                    bail!("at least one chat message is required");
                }

                let mut body =
                    serde_json::to_value(&request).context("failed to encode chat request")?;
                let object = body
                    .as_object_mut()
                    .context("chat request must be an object")?;
                object.insert("stream".to_string(), serde_json::Value::Bool(true));
                object.insert(
                    "stream_options".to_string(),
                    serde_json::json!({ "include_usage": true }),
                );
                if !request.tools.is_empty() {
                    object.insert(
                        "tool_choice".to_string(),
                        serde_json::Value::String("auto".into()),
                    );
                }

                let http_request = self
                    .authorized(self.client.post(self.endpoint("chat/completions")?))
                    .json(&body)
                    .build()
                    .context("failed to start streaming chat completion")?;
                let request_body_bytes = http_request
                    .body()
                    .and_then(reqwest::Body::as_bytes)
                    .map_or(0, <[u8]>::len);
                tracing::Span::current().record("request_body_bytes", request_body_bytes);
                tracing::debug!(target: "moye_ai", stage = "http_send", "Sending AI chat request");
                let response = self
                    .client
                    .execute(http_request)
                    .await
                    .context("failed to start streaming chat completion")?;
                let response = checked_response(response).await?;
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_CHAT_STREAM_BYTES as u64)
                {
                    bail!("AI chat stream exceeds the {MAX_CHAT_STREAM_BYTES}-byte limit");
                }
                tracing::debug!(
                    target: "moye_ai",
                    stage = "http_started",
                    http_status = response.status().as_u16(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "AI chat response stream started"
                );
                Ok(bounded_chat_sse_stream(response, SseLimits::CHAT))
            }
            .await;
            if let Err(error) = &result {
                tracing::warn!(
                    target: "moye_ai",
                    stage = "http_start_failed",
                    error_kind = error_kind(error),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "AI chat response stream failed to start"
                );
            }
            result
        }
        .instrument(span)
        .boxed()
    }

    fn embeddings(&self, request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async move {
            if request.model.trim().is_empty() {
                bail!("embedding model is required");
            }
            if request.input.is_empty() {
                bail!("embedding input is empty");
            }

            let expected = request.input.len();
            ensure!(
                expected <= MAX_EMBEDDING_COUNT,
                "embedding request exceeds the {MAX_EMBEDDING_COUNT}-input limit"
            );
            let response = self
                .authorized(self.client.post(self.endpoint("embeddings")?))
                .json(&request)
                .send()
                .await
                .context("failed to request embeddings")?;
            let response = checked_response(response).await?;
            let bytes = read_success_body_limited(
                response,
                MAX_EMBEDDINGS_RESPONSE_BYTES,
                "embedding response",
            )
            .await?;
            let body = serde_json::from_slice::<WireEmbeddingResponse>(&bytes)
                .context("embedding response is not valid OpenAI-compatible JSON")?;
            validate_embedding_response(body, expected)
        }
        .boxed()
    }
}

async fn checked_response(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = read_error_body_prefix(response, MAX_ERROR_BODY_BYTES).await;
    let envelope = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let provider_field = |field| {
        envelope
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get(field))
            .and_then(serde_json::Value::as_str)
            .map(provider_error_label)
    };
    tracing::warn!(
        target: "moye_ai",
        stage = "http_rejected",
        http_status = status.as_u16(),
        error_body_bytes = body.len(),
        error_body_at_limit = body.len() >= MAX_ERROR_BODY_BYTES,
        error_json_valid = envelope.is_some(),
        provider_code = ?provider_field("code"),
        provider_type = ?provider_field("type"),
        "AI endpoint rejected the request"
    );
    if let Some(error) = parse_context_window_error(status, &body) {
        tracing::debug!(
            target: "moye_ai",
            stage = "http_error_classified",
            error_kind = "context_window_exceeded",
            prompt_tokens = error.prompt_tokens,
            context_tokens = error.context_tokens,
            "AI endpoint reported insufficient context capacity"
        );
        return Err(error.into());
    }
    if let Some(error) = parse_incomplete_tool_arguments(status, &body) {
        tracing::debug!(
            target: "moye_ai",
            stage = "http_error_classified",
            error_kind = "incomplete_tool_arguments",
            tool = tool_label(&error.tool_name),
            "AI endpoint reported incomplete tool arguments"
        );
        return Err(error.into());
    }
    let detail = String::from_utf8_lossy(&body);
    bail!(
        "AI endpoint returned {}: {}",
        display_status(status),
        detail.trim()
    );
}

/// Only protocol identifiers can leave the error envelope through diagnostics.
/// An arbitrary identifier-shaped string can still contain private data.
fn provider_error_label(value: &str) -> &'static str {
    match value {
        "api_error" => "api_error",
        "invalid_request_error" => "invalid_request_error",
        "authentication_error" => "authentication_error",
        "permission_error" => "permission_error",
        "not_found_error" => "not_found_error",
        "rate_limit_error" => "rate_limit_error",
        "server_error" => "server_error",
        "internal_server_error" => "internal_server_error",
        "overloaded_error" => "overloaded_error",
        "context_length_exceeded" => "context_length_exceeded",
        "exceed_context_size_error" => "exceed_context_size_error",
        "model_not_found" => "model_not_found",
        "invalid_api_key" => "invalid_api_key",
        "rate_limit_exceeded" => "rate_limit_exceeded",
        "insufficient_quota" => "insufficient_quota",
        _ => "other",
    }
}

fn parse_incomplete_tool_arguments(
    status: StatusCode,
    body: &[u8],
) -> Option<IncompleteToolArguments> {
    if status != StatusCode::INTERNAL_SERVER_ERROR || body.len() > MAX_ERROR_BODY_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    // Match only the known endpoint error envelope, never echoed requests or
    // arbitrary JSON-error substrings in another server failure.
    let message = value.get("error")?.get("message")?.as_str()?;
    let tool_name = message
        .strip_prefix("llama-server returned invalid tool call arguments for \"")?
        .strip_suffix("\": unexpected end of JSON input")?;
    if tool_name.is_empty()
        || tool_name.len() > 64
        || !tool_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    Some(IncompleteToolArguments {
        tool_name: tool_name.to_string(),
    })
}

fn parse_context_window_error(status: StatusCode, body: &[u8]) -> Option<ContextWindowExceeded> {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE
    ) || body.len() > MAX_ERROR_BODY_BYTES
    {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?.trim();
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => context_error_value(&value, 0),
        Err(_) if text.starts_with(['{', '[', '"']) => None,
        Err(_) => context_error_message(text),
    }
}

fn context_error_value(value: &serde_json::Value, depth: usize) -> Option<ContextWindowExceeded> {
    if depth >= MAX_CONTEXT_ERROR_DEPTH {
        return None;
    }
    match value {
        serde_json::Value::Object(object) => {
            // Follow only error envelopes. Arbitrary request echoes must not
            // turn an unrelated 400 into a retryable context rejection.
            let mut error = ["error", "message"]
                .iter()
                .filter_map(|key| object.get(*key))
                .find_map(|nested| context_error_value(nested, depth + 1))
                .or_else(|| {
                    ["code", "type"]
                        .iter()
                        .filter_map(|key| object.get(*key)?.as_str())
                        .any(|code| {
                            matches!(
                                code,
                                "context_length_exceeded" | "exceed_context_size_error"
                            )
                        })
                        .then_some(ContextWindowExceeded {
                            prompt_tokens: None,
                            context_tokens: None,
                        })
                })?;
            error.prompt_tokens =
                context_token_count(object, &["n_prompt_tokens", "prompt_tokens"])
                    .or(error.prompt_tokens);
            error.context_tokens = context_token_count(
                object,
                &[
                    "n_ctx",
                    "context_tokens",
                    "context_length",
                    "max_context_length",
                    "max_context_tokens",
                ],
            )
            .or(error.context_tokens);
            Some(error)
        }
        serde_json::Value::String(message) => {
            let message = message.trim();
            if message.starts_with(['{', '[', '"']) {
                let nested = serde_json::from_str::<serde_json::Value>(message).ok()?;
                context_error_value(&nested, depth + 1)
            } else {
                context_error_message(message)
            }
        }
        _ => None,
    }
}

fn context_token_count(
    object: &serde_json::Map<String, serde_json::Value>,
    names: &[&str],
) -> Option<usize> {
    names
        .iter()
        .filter_map(|name| object.get(*name)?.as_u64())
        .filter_map(|count| usize::try_from(count).ok())
        .find(|count| *count > 0)
}

fn context_error_message(message: &str) -> Option<ContextWindowExceeded> {
    let message = message.to_ascii_lowercase();
    let is_context_error = message.contains("exceeds the available context size")
        || (message.contains("maximum context length")
            && ["exceed", "requested", "resulted in"]
                .iter()
                .any(|phrase| message.contains(phrase)))
        || (message.contains("context window")
            && ["exceeded", "too large", "too long"]
                .iter()
                .any(|phrase| message.contains(phrase)));
    if !is_context_error {
        return None;
    }
    Some(ContextWindowExceeded {
        prompt_tokens: tokens_after_phrase(
            &message,
            &["request (", "messages resulted in", "prompt contains"],
        ),
        context_tokens: tokens_after_phrase(
            &message,
            &[
                "available context size (",
                "maximum context length is",
                "maximum context length:",
                "context window is",
            ],
        ),
    })
}

fn tokens_after_phrase(message: &str, phrases: &[&str]) -> Option<usize> {
    phrases.iter().find_map(|phrase| {
        let start = message.find(phrase)? + phrase.len();
        let suffix = message[start..].trim_start();
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        // Never interpret a fractional, scientific, or grouped value as a
        // smaller integer token count.
        if suffix
            .as_bytes()
            .get(digits)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b',' | b'_'))
        {
            return None;
        }
        suffix[..digits].parse::<usize>().ok().filter(|n| *n > 0)
    })
}

async fn read_success_body_limited(
    response: Response,
    max_bytes: usize,
    label: &'static str,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!("{label} exceeds the {max_bytes}-byte limit");
    }
    collect_limited(response.bytes_stream(), max_bytes, label).await
}

async fn collect_limited<S, T>(mut input: S, max_bytes: usize, label: &str) -> Result<Vec<u8>>
where
    S: Stream<Item = std::result::Result<T, reqwest::Error>> + Unpin,
    T: AsRef<[u8]>,
{
    let mut body = Vec::new();
    while let Some(chunk) = input.next().await {
        let chunk = chunk.with_context(|| format!("failed to read {label}"))?;
        let next_len = body
            .len()
            .checked_add(chunk.as_ref().len())
            .with_context(|| format!("{label} size overflow"))?;
        ensure!(
            next_len <= max_bytes,
            "{label} exceeds the {max_bytes}-byte limit"
        );
        body.extend_from_slice(chunk.as_ref());
    }
    Ok(body)
}

async fn read_error_body_prefix(response: Response, max_bytes: usize) -> Vec<u8> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while body.len() < max_bytes {
        let chunk = match stream.next().await {
            Some(Ok(chunk)) => chunk,
            Some(Err(error)) => {
                let error = anyhow::Error::from(error);
                tracing::warn!(
                    target: "moye_ai",
                    stage = "http_error_body_read_failed",
                    error_kind = error_kind(&error),
                    error_body_bytes = body.len(),
                    "AI endpoint error body could not be read completely"
                );
                break;
            }
            None => break,
        };
        let remaining = max_bytes - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if chunk.len() > remaining {
            break;
        }
    }
    body
}

fn validate_and_normalize_models(mut models: Vec<ModelInfo>) -> Result<Vec<ModelInfo>> {
    ensure!(
        models.len() <= MAX_MODEL_ENTRIES,
        "AI models response exceeds the {MAX_MODEL_ENTRIES}-entry limit"
    );
    ensure!(
        models.iter().all(|model| {
            !model.id.trim().is_empty()
                && model.id.len() <= MAX_MODEL_FIELD_BYTES
                && model
                    .owned_by
                    .as_ref()
                    .is_none_or(|owner| owner.len() <= MAX_MODEL_FIELD_BYTES)
        }),
        "AI models response contains an empty or oversized model field"
    );
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

fn validate_embedding_response(
    mut body: WireEmbeddingResponse,
    expected: usize,
) -> Result<EmbeddingBatch> {
    ensure!(
        body.data.len() <= MAX_EMBEDDING_COUNT,
        "embedding response exceeds the {MAX_EMBEDDING_COUNT}-vector limit"
    );
    body.data.sort_by_key(|entry| entry.index);
    ensure!(
        body.data.len() == expected
            && body
                .data
                .iter()
                .enumerate()
                .all(|(index, entry)| index == entry.index),
        "embedding response indices do not match the request"
    );
    let dimensions = body
        .data
        .first()
        .map(|entry| entry.embedding.len())
        .unwrap_or(0);
    ensure!(
        dimensions != 0 && dimensions <= MAX_EMBEDDING_DIMENSIONS,
        "embedding response dimensions must be between 1 and {MAX_EMBEDDING_DIMENSIONS}"
    );
    ensure!(
        body.data.iter().all(|entry| {
            entry.embedding.len() == dimensions
                && entry.embedding.iter().all(|value| value.is_finite())
        }),
        "embedding response contains invalid or inconsistent vectors"
    );
    Ok(EmbeddingBatch {
        model: body.model,
        vectors: body.data.into_iter().map(|entry| entry.embedding).collect(),
        usage: body.usage,
    })
}

struct BoundedSseState<S, T> {
    input: Pin<Box<S>>,
    chunk: Option<T>,
    chunk_offset: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    event_wire_bytes: usize,
    total_bytes: usize,
    event_count: usize,
    eof: bool,
    terminated: bool,
    limits: SseLimits,
}

impl<S, T> BoundedSseState<S, T>
where
    S: Stream<Item = std::result::Result<T, reqwest::Error>> + Send + 'static,
    T: AsRef<[u8]> + Send + 'static,
{
    fn new(input: S, limits: SseLimits) -> Self {
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
        }
    }

    async fn next_event(&mut self) -> Result<Option<ChatStreamEvent>> {
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
            match self.input.next().await {
                Some(Ok(chunk)) => {
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
                Some(Err(error)) => return Err(error).context("failed to read AI chat stream"),
                None => self.eof = true,
            }
        }
    }

    fn next_buffered_byte(&mut self) -> Option<u8> {
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

    fn push_byte(&mut self, byte: u8) -> Result<Option<Vec<u8>>> {
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

    fn finish_eof(&mut self) -> Result<Option<Vec<u8>>> {
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

    fn parse_event(&mut self, data: Vec<u8>) -> Result<ChatStreamEvent> {
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

fn append_sse_data_line(output: &mut Vec<u8>, line: &[u8], max_bytes: usize) -> Result<()> {
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

fn bounded_chat_sse_stream(response: Response, limits: SseLimits) -> ChatEventStream {
    let state = BoundedSseState::new(response.bytes_stream(), limits);
    let diagnostics = SseDiagnostics::new();
    let span = tracing::Span::current();
    let stream = stream::try_unfold((state, diagnostics), move |(mut state, mut diagnostics)| {
        let span = span.clone();
        async move {
            let result = state.next_event().await;
            match &result {
                Ok(Some(event)) => {
                    diagnostics.observe(event);
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

/// Constant-size stream diagnostics. No event payloads or generated arguments
/// are retained here, and there is no log entry for each token/chunk.
struct SseDiagnostics {
    started_at: Instant,
    first_event_ms: Option<u64>,
    content_bytes: usize,
    tool_delta_count: usize,
    tool_argument_bytes: usize,
    finish_reason: Option<&'static str>,
    usage: Option<Usage>,
    finished: bool,
}

impl SseDiagnostics {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            first_event_ms: None,
            content_bytes: 0,
            tool_delta_count: 0,
            tool_argument_bytes: 0,
            finish_reason: None,
            usage: None,
            finished: false,
        }
    }

    fn observe(&mut self, event: &ChatStreamEvent) {
        self.first_event_ms
            .get_or_insert_with(|| self.started_at.elapsed().as_millis() as u64);
        self.content_bytes = self
            .content_bytes
            .saturating_add(event.content_delta.as_ref().map_or(0, String::len));
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
    }

    fn finish(
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
                target: "moye_ai",
                stage = "sse_failed",
                error_kind = error_kind(error),
                json_error_category = ?json_error.map(serde_json::Error::classify),
                json_error_line = json_error.map(serde_json::Error::line),
                json_error_column = json_error.map(serde_json::Error::column),
                wire_bytes,
                event_count,
                elapsed_ms = self.started_at.elapsed().as_millis() as u64,
                "AI response stream failed"
            );
        }
        tracing::debug!(
            target: "moye_ai",
            stage = "sse_finished",
            outcome,
            wire_bytes,
            event_count,
            content_bytes = self.content_bytes,
            tool_delta_count = self.tool_delta_count,
            tool_argument_bytes = self.tool_argument_bytes,
            finish_reason = ?self.finish_reason,
            first_event_ms = self.first_event_ms,
            elapsed_ms = self.started_at.elapsed().as_millis() as u64,
            prompt_tokens = self.usage.as_ref().map(|usage| usage.prompt_tokens),
            completion_tokens = self.usage.as_ref().map(|usage| usage.completion_tokens),
            total_tokens = self.usage.as_ref().map(|usage| usage.total_tokens),
            "AI response stream summary"
        );
    }
}

fn display_status(status: StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => status.as_u16().to_string(),
    }
}

fn parse_chat_stream_event(data: &str) -> Result<ChatStreamEvent> {
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

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_generation_settings_validate_boundaries_and_reject_non_finite_numbers() {
        ChatGenerationSettings::default().validate().unwrap();
        for settings in [
            ChatGenerationSettings {
                temperature: None,
                top_p: None,
                max_output_tokens: 1,
                presence_penalty: None,
                frequency_penalty: None,
            },
            ChatGenerationSettings {
                temperature: Some(0.0),
                top_p: Some(0.0),
                max_output_tokens: 1,
                presence_penalty: Some(-2.0),
                frequency_penalty: Some(-2.0),
            },
            ChatGenerationSettings {
                temperature: Some(2.0),
                top_p: Some(1.0),
                max_output_tokens: DEFAULT_CHAT_OUTPUT_TOKENS,
                presence_penalty: Some(2.0),
                frequency_penalty: Some(2.0),
            },
        ] {
            settings.validate().unwrap();
        }
        for (field, minimum, maximum) in
            [(0, 0.0, 2.0), (1, 0.0, 1.0), (2, -2.0, 2.0), (3, -2.0, 2.0)]
        {
            for value in [
                minimum - 0.1,
                maximum + 0.1,
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ] {
                let mut settings = ChatGenerationSettings::default();
                match field {
                    0 => settings.temperature = Some(value),
                    1 => settings.top_p = Some(value),
                    2 => settings.presence_penalty = Some(value),
                    _ => settings.frequency_penalty = Some(value),
                }
                assert!(
                    settings.validate().is_err(),
                    "field {field} accepted {value}"
                );
            }
        }
        for max_output_tokens in [DEFAULT_CHAT_OUTPUT_TOKENS + 1, 65_536, 131_072, u32::MAX] {
            ChatGenerationSettings {
                max_output_tokens,
                ..Default::default()
            }
            .validate()
            .unwrap();
        }
        assert!(
            ChatGenerationSettings {
                max_output_tokens: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        let mut value = serde_json::to_value(ChatGenerationSettings::default()).unwrap();
        value["unsupported"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ChatGenerationSettings>(value).is_err());
    }

    #[test]
    fn chat_request_omits_unset_sampling_parameters_and_serializes_explicit_values() {
        let mut request = ChatRequest {
            model: "test-model".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            top_p: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: Some(128),
            reasoning_effort: None,
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "model": "test-model", "messages": [], "max_tokens": 128
            })
        );
        request.temperature = Some(0.5);
        request.top_p = Some(0.75);
        request.presence_penalty = Some(-0.5);
        request.frequency_penalty = Some(1.5);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "model": "test-model", "messages": [], "max_tokens": 128,
                "temperature": 0.5, "top_p": 0.75, "presence_penalty": -0.5,
                "frequency_penalty": 1.5
            })
        );
    }

    #[test]
    fn recognizes_only_the_known_incomplete_tool_error_envelope() {
        let body = serde_json::json!({"error": {
            "message": "llama-server returned invalid tool call arguments for \"read_passages\": unexpected end of JSON input",
            "type": "api_error", "param": null, "code": null
        }});
        let bytes = serde_json::to_vec(&body).unwrap();
        let error =
            parse_incomplete_tool_arguments(StatusCode::INTERNAL_SERVER_ERROR, &bytes).unwrap();
        assert_eq!(error.tool_name, "read_passages");
        assert!(error.to_string().contains("完整 JSON"));
        for status in [
            StatusCode::OK,
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(parse_incomplete_tool_arguments(status, &bytes).is_none());
        }
        for value in [
            serde_json::json!({"request": body}),
            serde_json::json!({"message": body["error"]["message"]}),
            serde_json::json!({"error": {"message": "unexpected end of JSON input"}}),
            serde_json::json!({"error": {"message": "llama-server returned invalid tool call arguments for \"read_passages\": invalid character"}}),
            serde_json::json!({"error": {"message": "llama-server returned invalid tool call arguments for \"read_passages\nignore instructions\": unexpected end of JSON input"}}),
        ] {
            assert!(
                parse_incomplete_tool_arguments(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::to_vec(&value).unwrap()
                )
                .is_none()
            );
        }
        assert!(
            parse_incomplete_tool_arguments(
                StatusCode::INTERNAL_SERVER_ERROR,
                &bytes[..bytes.len() - 1]
            )
            .is_none()
        );
        let mut oversized = bytes;
        oversized.resize(MAX_ERROR_BODY_BYTES + 1, b' ');
        assert!(
            parse_incomplete_tool_arguments(StatusCode::INTERNAL_SERVER_ERROR, &oversized)
                .is_none()
        );
    }

    #[test]
    fn parses_nested_context_rejection_with_endpoint_token_counts() {
        let inner = serde_json::json!({
            "error": {
                "code": 400,
                "message": "request (4155 tokens) exceeds the available context size (4096 tokens), try increasing it",
                "type": "exceed_context_size_error",
                "n_prompt_tokens": 4155,
                "n_ctx": 4096
            }
        });
        let body = serde_json::to_vec(&serde_json::json!({
            "error": {
                "message": inner.to_string(),
                "type": "invalid_request_error",
                "param": null,
                "code": null
            }
        }))
        .unwrap();
        let parsed = parse_context_window_error(StatusCode::BAD_REQUEST, &body).unwrap();
        assert_eq!(parsed.prompt_tokens, Some(4155));
        assert_eq!(parsed.context_tokens, Some(4096));
        assert!(
            parsed
                .to_string()
                .contains("输入 4155 tokens，上限 4096 tokens")
        );
        assert!(parsed.to_string().contains("开启新对话"));
        let error = anyhow::Error::new(parsed).context("failed to start AI response stream");
        assert_eq!(
            error
                .downcast_ref::<ContextWindowExceeded>()
                .unwrap()
                .context_tokens,
            Some(4096)
        );
    }

    #[test]
    fn parses_standard_context_rejections_without_guessing_output_as_input() {
        let body = br#"{"error":{"code":"context_length_exceeded","type":"invalid_request_error","message":"This model's maximum context length is 4096 tokens. However, your messages resulted in 4155 tokens."}}"#;
        let error = parse_context_window_error(StatusCode::BAD_REQUEST, body).unwrap();
        assert_eq!(error.prompt_tokens, Some(4155));
        assert_eq!(error.context_tokens, Some(4096));
        let requested = br#"{"error":{"message":"This model's maximum context length is 4096 tokens. However, you requested 8000 tokens (4000 in messages, 4000 in completion)."}}"#;
        let error = parse_context_window_error(StatusCode::BAD_REQUEST, requested).unwrap();
        assert_eq!(error.prompt_tokens, None);
        assert_eq!(error.context_tokens, Some(4096));
        let plain = b"request (4155 tokens) exceeds the available context size (4096 tokens)";
        assert_eq!(
            parse_context_window_error(StatusCode::PAYLOAD_TOO_LARGE, plain),
            Some(ContextWindowExceeded {
                prompt_tokens: Some(4155),
                context_tokens: Some(4096),
            })
        );
    }

    #[test]
    fn does_not_reclassify_unrelated_http_errors_or_request_echoes() {
        for body in [
            br#"{"error":{"code":"invalid_request_error","message":"Unsupported reasoning_effort value"}}"#.as_slice(),
            br#"{"error":{"message":"Invalid context_length parameter"}}"#.as_slice(),
            br#"{"error":{"message":"Payload too large"}}"#.as_slice(),
            br#"{"request":{"message":"request (4155 tokens) exceeds the available context size (4096 tokens)"},"error":{"message":"Invalid model"}}"#.as_slice(),
            br#"{"error":{"code":"prefix_context_length_exceeded_suffix"}}"#.as_slice(),
        ] {
            assert!(parse_context_window_error(StatusCode::BAD_REQUEST, body).is_none());
            assert!(parse_context_window_error(StatusCode::PAYLOAD_TOO_LARGE, body).is_none());
        }
        let body = br#"{"error":{"code":"context_length_exceeded"}}"#;
        for status in [
            StatusCode::OK,
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(parse_context_window_error(status, body).is_none());
        }
    }

    #[test]
    fn context_error_parsing_rejects_invalid_or_excessively_nested_envelopes() {
        for body in [
            b"{broken JSON: exceeds the available context size".as_slice(),
            br#"{"error":{"message":"{not JSON: exceeds the available context size"}}"#.as_slice(),
            br#"[{"code":"context_length_exceeded"}]"#.as_slice(),
            b"\xffexceeds the available context size".as_slice(),
        ] {
            assert!(parse_context_window_error(StatusCode::BAD_REQUEST, body).is_none());
        }
        let mut nested = serde_json::json!({"code": "context_length_exceeded"});
        for _ in 0..MAX_CONTEXT_ERROR_DEPTH {
            nested = serde_json::json!({"error": nested});
        }
        assert!(
            parse_context_window_error(
                StatusCode::BAD_REQUEST,
                &serde_json::to_vec(&nested).unwrap()
            )
            .is_none()
        );
        let mut encoded = serde_json::json!({"code": "context_length_exceeded"});
        for _ in 0..MAX_CONTEXT_ERROR_DEPTH {
            encoded = serde_json::json!({"message": encoded.to_string()});
        }
        assert!(
            parse_context_window_error(
                StatusCode::BAD_REQUEST,
                &serde_json::to_vec(&encoded).unwrap()
            )
            .is_none()
        );
        let oversized = format!(
            "request (4155 tokens) exceeds the available context size (4096 tokens){}",
            " ".repeat(MAX_ERROR_BODY_BYTES)
        );
        assert!(
            parse_context_window_error(StatusCode::BAD_REQUEST, oversized.as_bytes()).is_none()
        );
    }

    #[test]
    fn context_token_counts_reject_invalid_numeric_values_without_overflow() {
        for value in [
            "0",
            "-1",
            "1.5",
            "1e30",
            "18446744073709551616",
            "null",
            "\"4096\"",
        ] {
            let body = format!(
                "{{\"error\":{{\"type\":\"exceed_context_size_error\",\"n_prompt_tokens\":{value},\"n_ctx\":{value}}}}}"
            );
            let error =
                parse_context_window_error(StatusCode::BAD_REQUEST, body.as_bytes()).unwrap();
            assert_eq!(error.prompt_tokens, None, "{value}");
            assert_eq!(error.context_tokens, None, "{value}");
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "error": {
                "code": "context_length_exceeded",
                "prompt_tokens": usize::MAX,
                "context_tokens": 1
            }
        }))
        .unwrap();
        let error = parse_context_window_error(StatusCode::BAD_REQUEST, &body).unwrap();
        assert_eq!(error.prompt_tokens, Some(usize::MAX));
        assert_eq!(error.context_tokens, Some(1));
        for value in ["0", "-1", "1.5", "1e30", "4,096", "18446744073709551616"] {
            let body = format!(
                "request ({value} tokens) exceeds the available context size ({value} tokens)"
            );
            let error =
                parse_context_window_error(StatusCode::BAD_REQUEST, body.as_bytes()).unwrap();
            assert_eq!(error.prompt_tokens, None, "{value}");
            assert_eq!(error.context_tokens, None, "{value}");
        }
    }

    fn sse_state(
        bytes: Vec<u8>,
        limits: SseLimits,
    ) -> BoundedSseState<
        impl Stream<Item = std::result::Result<Vec<u8>, reqwest::Error>> + Send + 'static,
        Vec<u8>,
    > {
        BoundedSseState::new(
            stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(bytes)]),
            limits,
        )
    }

    #[test]
    fn defaults_to_local_ollama_without_remote_confirmation() {
        let config = ProviderConfig::default();
        let url = config.validated_base_url().unwrap();
        assert_eq!(url.as_str(), DEFAULT_OLLAMA_OPENAI_BASE_URL);
        assert!(!config.is_remote().unwrap());
        assert_eq!(config.request_timeout_secs, DEFAULT_AI_REQUEST_TIMEOUT_SECS);
    }

    #[test]
    fn request_timeout_accepts_only_the_published_bounds() {
        for timeout in [MIN_AI_REQUEST_TIMEOUT_SECS, MAX_AI_REQUEST_TIMEOUT_SECS] {
            let config = ProviderConfig {
                request_timeout_secs: timeout,
                ..Default::default()
            };
            assert!(config.validated_base_url().is_ok());
        }
        for timeout in [
            MIN_AI_REQUEST_TIMEOUT_SECS - 1,
            MAX_AI_REQUEST_TIMEOUT_SECS + 1,
        ] {
            let config = ProviderConfig {
                request_timeout_secs: timeout,
                ..Default::default()
            };
            assert!(config.validated_base_url().is_err());
        }
    }

    #[test]
    fn remote_endpoint_requires_confirmation_and_https() {
        let mut config = ProviderConfig {
            base_url: "https://models.example.test/v1".to_string(),
            ..Default::default()
        };
        assert!(config.validated_base_url().is_err());
        config.remote_content_confirmed = true;
        assert!(config.validated_base_url().is_ok());

        config.base_url = "http://models.example.test/v1".to_string();
        assert!(config.validated_base_url().is_err());
        config.allow_insecure_remote_http = true;
        assert!(config.validated_base_url().is_ok());
    }

    #[test]
    fn parses_streamed_content_tools_and_usage() {
        let event = parse_chat_stream_event(
            r#"{"choices":[{"delta":{"content":"answer","tool_calls":[{"index":0,"id":"call-1","function":{"name":"search_books","arguments":"{\"query\":"}}]},"finish_reason":null}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
        )
        .unwrap();
        assert_eq!(event.content_delta.as_deref(), Some("answer"));
        assert_eq!(
            event.tool_call_deltas[0].name.as_deref(),
            Some("search_books")
        );
        assert_eq!(event.usage.unwrap().total_tokens, 3);
    }

    #[test]
    fn rejects_credentials_in_endpoint_url() {
        let config = ProviderConfig {
            base_url: "https://secret@example.test/v1".to_string(),
            remote_content_confirmed: true,
            ..Default::default()
        };
        assert!(config.validated_base_url().is_err());
    }

    #[tokio::test]
    async fn successful_body_limit_accepts_exact_size_and_rejects_one_more_byte() {
        let exact = stream::iter(vec![
            Ok::<Vec<u8>, reqwest::Error>(b"12".to_vec()),
            Ok::<Vec<u8>, reqwest::Error>(b"34".to_vec()),
        ]);
        assert_eq!(
            collect_limited(exact, 4, "test response").await.unwrap(),
            b"1234"
        );

        let over = stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(b"12345".to_vec())]);
        let error = collect_limited(over, 4, "test response")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("4-byte limit"));
    }

    #[tokio::test]
    async fn sse_limits_apply_before_event_json_is_parsed() {
        let bytes = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},",
            "\"finish_reason\":null}]}\n\n"
        )
        .as_bytes()
        .to_vec();
        let event_wire_bytes = bytes.len() - 1;
        let exact_limits = SseLimits {
            total_bytes: bytes.len(),
            event_bytes: event_wire_bytes,
            events: 1,
        };
        let mut exact = sse_state(bytes.clone(), exact_limits);
        assert_eq!(
            exact
                .next_event()
                .await
                .unwrap()
                .unwrap()
                .content_delta
                .as_deref(),
            Some("x")
        );

        let mut event_over = sse_state(
            bytes.clone(),
            SseLimits {
                event_bytes: event_wire_bytes - 1,
                ..exact_limits
            },
        );
        assert!(
            event_over
                .next_event()
                .await
                .unwrap_err()
                .to_string()
                .contains("chat event exceeds")
        );

        let mut total_over = sse_state(
            bytes.clone(),
            SseLimits {
                total_bytes: bytes.len() - 1,
                ..exact_limits
            },
        );
        assert!(
            total_over
                .next_event()
                .await
                .unwrap_err()
                .to_string()
                .contains("chat stream exceeds")
        );
    }

    #[tokio::test]
    async fn sse_event_count_rejects_the_first_event_above_the_limit() {
        let event = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},",
            "\"finish_reason\":null}]}\n\n"
        );
        let bytes = format!("{event}{event}").into_bytes();
        let mut state = sse_state(
            bytes.clone(),
            SseLimits {
                total_bytes: bytes.len(),
                event_bytes: event.len() - 1,
                events: 1,
            },
        );
        assert!(state.next_event().await.unwrap().is_some());
        assert!(
            state
                .next_event()
                .await
                .unwrap_err()
                .to_string()
                .contains("1-event limit")
        );
    }

    #[test]
    fn model_entry_limit_is_checked_before_deduplication() {
        let model = ModelInfo {
            id: "model".to_string(),
            owned_by: None,
        };
        assert_eq!(
            validate_and_normalize_models(vec![model.clone(); MAX_MODEL_ENTRIES])
                .unwrap()
                .len(),
            1
        );
        let error = validate_and_normalize_models(vec![model; MAX_MODEL_ENTRIES + 1])
            .unwrap_err()
            .to_string();
        assert!(error.contains("entry limit"));
    }

    #[test]
    fn embedding_count_dimension_and_numeric_limits_are_enforced() {
        let exact_count = WireEmbeddingResponse {
            model: "embed".into(),
            data: (0..MAX_EMBEDDING_COUNT)
                .map(|index| WireEmbedding {
                    index,
                    embedding: vec![0.0],
                })
                .collect(),
            usage: None,
        };
        assert_eq!(
            validate_embedding_response(exact_count, MAX_EMBEDDING_COUNT)
                .unwrap()
                .vectors
                .len(),
            MAX_EMBEDDING_COUNT
        );

        let over_count = WireEmbeddingResponse {
            model: "embed".into(),
            data: (0..=MAX_EMBEDDING_COUNT)
                .map(|index| WireEmbedding {
                    index,
                    embedding: vec![0.0],
                })
                .collect(),
            usage: None,
        };
        assert!(
            validate_embedding_response(over_count, MAX_EMBEDDING_COUNT + 1)
                .unwrap_err()
                .to_string()
                .contains("vector limit")
        );

        let exact_dimensions = WireEmbeddingResponse {
            model: "embed".into(),
            data: vec![WireEmbedding {
                index: 0,
                embedding: vec![0.0; MAX_EMBEDDING_DIMENSIONS],
            }],
            usage: None,
        };
        assert_eq!(
            validate_embedding_response(exact_dimensions, 1)
                .unwrap()
                .vectors[0]
                .len(),
            MAX_EMBEDDING_DIMENSIONS
        );

        let over_dimensions = WireEmbeddingResponse {
            model: "embed".into(),
            data: vec![WireEmbedding {
                index: 0,
                embedding: vec![0.0; MAX_EMBEDDING_DIMENSIONS + 1],
            }],
            usage: None,
        };
        assert!(
            validate_embedding_response(over_dimensions, 1)
                .unwrap_err()
                .to_string()
                .contains("dimensions")
        );

        let non_finite = WireEmbeddingResponse {
            model: "embed".into(),
            data: vec![WireEmbedding {
                index: 0,
                embedding: vec![f32::NAN],
            }],
            usage: None,
        };
        assert!(
            validate_embedding_response(non_finite, 1)
                .unwrap_err()
                .to_string()
                .contains("invalid or inconsistent")
        );
    }
}
