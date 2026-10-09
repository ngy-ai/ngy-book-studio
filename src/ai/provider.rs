//! The HTTP provider and its implementation of the provider trait.

use super::*;

#[derive(Clone)]
pub struct OpenAiHttpProvider {
    pub(crate) config: ProviderConfig,
    pub(crate) client: Client,
    pub(crate) base_url: Url,
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
        Self::new_with_proxy(config, DEFAULT_AI_USE_PROXY)
    }

    /// `use_proxy` is the system-wide network preference rather than an endpoint
    /// property, so it is not part of [`ProviderConfig`]. When false the client
    /// carries an explicit `no_proxy()`, which overrides the environment
    /// variables reqwest would otherwise pick up.
    pub fn new_with_proxy(config: ProviderConfig, use_proxy: bool) -> Result<Self> {
        let base_url = config.validated_base_url()?;
        // No client-wide `.timeout()`: reqwest applies that deadline to the whole
        // request *including the response body*, so it cut healthy generations in
        // half. Non-streaming calls are bounded by `within_request_timeout` and
        // streaming replies by the silence between two chunks.
        let mut builder = Client::builder().connect_timeout(AI_CONNECT_TIMEOUT);
        if !use_proxy {
            builder = builder.no_proxy();
        }
        let client = builder.build().context("failed to build AI HTTP client")?;
        Ok(Self {
            config,
            client,
            base_url,
        })
    }

    /// Bounds one non-streaming provider call with the configured request
    /// timeout. Streaming replies use [`ProviderTimeout::stream`] instead: their
    /// total duration is bounded by the output-token, byte and event limits.
    pub(super) async fn within_request_timeout<T>(
        &self,
        future: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let secs = self.config.request_timeout_secs;
        match tokio::time::timeout(Duration::from_secs(secs), future).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::Error::new(ProviderTimeout::request(secs))),
        }
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    pub(super) fn endpoint(&self, relative: &str) -> Result<Url> {
        self.base_url
            .join(relative)
            .with_context(|| format!("failed to resolve AI endpoint {relative}"))
    }

    pub(super) fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.config.api_key.as_deref() {
            Some(key) if !key.is_empty() => request.bearer_auth(key),
            _ => request,
        }
    }
}

/// The JSON body one chat request is sent with: the serialized request plus the
/// streaming fields this transport adds.
///
/// The send path and the `NGY_DUMP_TRANSLATION_RAW` diagnostic build the body
/// through here, so a dumped body is byte-for-byte the one that goes on the wire
/// instead of a second copy of this shape that can drift away from the transport.
pub(crate) fn chat_request_body(request: &ChatRequest) -> Result<serde_json::Value> {
    let mut body = serde_json::to_value(request).context("failed to encode chat request")?;
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
    Ok(body)
}

impl OpenAiCompatibleProvider for OpenAiHttpProvider {
    /// 这一发实际的端点来源（`scheme://host[:port]`，路径见
    /// [`endpoint_origin_label`] 的说明）。从 `base_url` 现取，所以保存设置换端点后
    /// 的下一发请求立刻标出新地址。
    fn endpoint_origin(&self) -> Option<String> {
        Some(endpoint_origin_label(&self.base_url))
    }

    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async move {
            #[derive(Deserialize)]
            struct ModelsResponse {
                data: Vec<ModelInfo>,
            }

            self.within_request_timeout(async {
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
            })
            .await
        }
        .boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        // Record only the endpoint origin: paths can contain tenant names or
        // credentials even when query strings and userinfo are prohibited.
        let span = tracing::info_span!(
            target: "ngy_ai",
            "ai_http",
            http_id = NEXT_HTTP_ID.fetch_add(1, Ordering::Relaxed),
            operation = "chat_stream",
            model = %safe_label(&request.model),
            endpoint_scheme = self.base_url.scheme(),
            endpoint_host = self.base_url.host_str().unwrap_or(""),
            endpoint_port = self.base_url.port_or_known_default(),
            timeout_mode = "stream_idle",
            timeout_secs = self.config.request_timeout_secs,
            max_stream_bytes = MAX_CHAT_STREAM_BYTES,
            max_stream_events = MAX_CHAT_EVENTS,
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            max_tokens = request.max_tokens,
            temperature = request.temperature,
            reasoning_effort = request
                .reasoning_effort
                .map(ReasoningEffort::label)
                .unwrap_or("unset"),
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

                let body = chat_request_body(&request)?;

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
                tracing::debug!(target: "ngy_ai", stage = "http_send", "Sending AI chat request");
                // Only the wait for the response headers is bounded by the
                // configured request timeout; the body that follows is bounded by
                // the silence between its chunks.
                let response = self
                    .within_request_timeout(async {
                        let response = self
                            .client
                            .execute(http_request)
                            .await
                            .context("failed to start streaming chat completion")?;
                        checked_response(response).await
                    })
                    .await?;
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_CHAT_STREAM_BYTES as u64)
                {
                    bail!("AI chat stream exceeds the {MAX_CHAT_STREAM_BYTES}-byte limit");
                }
                tracing::debug!(
                    target: "ngy_ai",
                    stage = "http_started",
                    http_status = response.status().as_u16(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "AI chat response stream started"
                );
                Ok(bounded_chat_sse_stream(
                    response,
                    SseLimits::CHAT,
                    Duration::from_secs(self.config.request_timeout_secs),
                ))
            }
            .await;
            if let Err(error) = &result {
                tracing::warn!(
                    target: "ngy_ai",
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
            self.within_request_timeout(async {
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
            })
            .await
        }
        .boxed()
    }
}
