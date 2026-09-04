//! OpenAI-compatible model access.
//!
//! The domain and UI only depend on [`OpenAiCompatibleProvider`].  In
//! particular, nothing in this module assumes that the endpoint is Ollama;
//! Ollama is merely the safe, loopback default.

use std::{net::IpAddr, pin::Pin, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::{FutureExt as _, Stream, StreamExt as _, future::BoxFuture, stream};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};

pub const DEFAULT_OLLAMA_OPENAI_BASE_URL: &str = "http://127.0.0.1:11434/v1/";
pub const DEFAULT_AI_REQUEST_TIMEOUT_SECS: u64 = 120;
pub const MIN_AI_REQUEST_TIMEOUT_SECS: u64 = 1;
pub const MAX_AI_REQUEST_TIMEOUT_SECS: u64 = 600;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_MODELS_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODEL_ENTRIES: usize = 4096;
const MAX_MODEL_FIELD_BYTES: usize = 4096;
const MAX_EMBEDDINGS_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_EMBEDDING_COUNT: usize = 2048;
const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;
const MAX_CHAT_STREAM_BYTES: usize = 32 * 1024 * 1024;
const MAX_CHAT_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHAT_EVENTS: usize = 65_536;

pub type ChatEventStream = Pin<Box<dyn Stream<Item = Result<ChatStreamEvent>> + Send>>;

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
        async move {
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

            let response = self
                .authorized(self.client.post(self.endpoint("chat/completions")?))
                .json(&body)
                .send()
                .await
                .context("failed to start streaming chat completion")?;
            let response = checked_response(response).await?;
            if response
                .content_length()
                .is_some_and(|length| length > MAX_CHAT_STREAM_BYTES as u64)
            {
                bail!("AI chat stream exceeds the {MAX_CHAT_STREAM_BYTES}-byte limit");
            }
            Ok(bounded_chat_sse_stream(response, SseLimits::CHAT))
        }
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
    let detail = String::from_utf8_lossy(&body);
    bail!(
        "AI endpoint returned {}: {}",
        display_status(status),
        detail.trim()
    );
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
        let Some(Ok(chunk)) = stream.next().await else {
            break;
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
    let stream = stream::try_unfold(state, |mut state| async move {
        Ok(state.next_event().await?.map(|event| (event, state)))
    });
    Box::pin(stream)
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
