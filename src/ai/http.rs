//! Reading, bounding and classifying provider responses.

use super::*;

pub(crate) async fn checked_response(response: reqwest::Response) -> Result<reqwest::Response> {
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
        target: "ngy_ai",
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
            target: "ngy_ai",
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
            target: "ngy_ai",
            stage = "http_error_classified",
            error_kind = "incomplete_tool_arguments",
            tool = tool_label(&error.tool_name),
            "AI endpoint reported incomplete tool arguments"
        );
        return Err(error.into());
    }
    let detail = String::from_utf8_lossy(&body);
    Err(ProviderHttpError {
        status: status.as_u16(),
        detail: detail.trim().to_owned(),
    }
    .into())
}

/// Only protocol identifiers can leave the error envelope through diagnostics.
/// An arbitrary identifier-shaped string can still contain private data.
pub(crate) fn provider_error_label(value: &str) -> &'static str {
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

pub(crate) fn parse_incomplete_tool_arguments(
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

pub(crate) fn parse_context_window_error(
    status: StatusCode,
    body: &[u8],
) -> Option<ContextWindowExceeded> {
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

pub(crate) fn context_error_value(
    value: &serde_json::Value,
    depth: usize,
) -> Option<ContextWindowExceeded> {
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

pub(crate) fn context_token_count(
    object: &serde_json::Map<String, serde_json::Value>,
    names: &[&str],
) -> Option<usize> {
    names
        .iter()
        .filter_map(|name| object.get(*name)?.as_u64())
        .filter_map(|count| usize::try_from(count).ok())
        .find(|count| *count > 0)
}

pub(crate) fn context_error_message(message: &str) -> Option<ContextWindowExceeded> {
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

pub(crate) fn tokens_after_phrase(message: &str, phrases: &[&str]) -> Option<usize> {
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

pub(crate) async fn read_success_body_limited(
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

pub(crate) async fn collect_limited<S, T>(
    mut input: S,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>>
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

pub(crate) async fn read_error_body_prefix(response: Response, max_bytes: usize) -> Vec<u8> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while body.len() < max_bytes {
        let chunk = match stream.next().await {
            Some(Ok(chunk)) => chunk,
            Some(Err(error)) => {
                let error = anyhow::Error::from(error);
                tracing::warn!(
                    target: "ngy_ai",
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
