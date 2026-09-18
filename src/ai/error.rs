//! The typed provider errors the UI and the agents reason about.

use super::*;

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

/// An HTTP rejection that did not match a more specific provider protocol
/// error. Diagnostics may read its numeric status, never its response detail.
#[derive(Debug)]
pub struct ProviderHttpError {
    pub status: u16,
    pub(crate) detail: String,
}

impl fmt::Display for ProviderHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = StatusCode::from_u16(self.status).map_err(|_| fmt::Error)?;
        write!(
            formatter,
            "AI endpoint returned {}: {}",
            display_status(status),
            self.detail
        )
    }
}

impl std::error::Error for ProviderHttpError {}

/// A configured provider deadline expired without usable data.
///
/// `scope` distinguishes the two policies: a whole non-streaming call
/// (`request`) or the silence between two chunks of one streaming reply
/// (`stream`). A streaming reply is deliberately **not** bounded by a total
/// deadline: a local model can stream a healthy answer for minutes, and killing
/// it at the request timeout turned "slow" into "failed" (observed 2026-09-11:
/// 120 s, 1579 content chunks, 7024 bytes of answer and no `[DONE]`). Only a
/// silence longer than the configured timeout is an error; the reply stays
/// bounded by the output-token, byte and event limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderTimeout {
    pub(crate) scope: &'static str,
    pub(crate) secs: u64,
}

impl ProviderTimeout {
    /// Deadline of one non-streaming provider call.
    pub fn request(secs: u64) -> Self {
        Self {
            scope: "request",
            secs,
        }
    }

    /// Maximum silence between two chunks of one streaming reply.
    pub fn stream(secs: u64) -> Self {
        Self {
            scope: "stream",
            secs,
        }
    }

    pub fn scope(&self) -> &'static str {
        self.scope
    }

    pub fn secs(&self) -> u64 {
        self.secs
    }
}

impl fmt::Display for ProviderTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.scope {
            "stream" => write!(
                formatter,
                "模型在 {} 秒内没有继续返回响应数据，连接已中断；请重试，或在该 Endpoint 设置中提高请求超时",
                self.secs
            ),
            _ => write!(
                formatter,
                "模型请求在 {} 秒内没有完成，已中断；请重试，或在该 Endpoint 设置中提高请求超时",
                self.secs
            ),
        }
    }
}

impl std::error::Error for ProviderTimeout {}
