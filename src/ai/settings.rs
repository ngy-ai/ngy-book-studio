//! Generation settings and the provider configuration.

use super::*;

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
        validate_generation_bounds(
            self.temperature,
            self.top_p,
            self.presence_penalty,
            self.frequency_penalty,
        )?;
        ensure!(self.max_output_tokens > 0, "最大输出 token 数必须是正整数");
        Ok(())
    }
}

/// 整本翻译自己的生成参数。与问答参数分开存储、分开配置：翻译一路默认是确定性的
/// （`temperature = 0.0`），因为一个文本块只有一个可接受的答案，采样只会给严格 JSON
/// 协议增加方差；默认输出上限取翻译任务一直使用的那个量。
///
/// 它**不属于执行身份**（`translation_execution_identity` 只由端点与模型派生）：生成
/// 参数不改变输出契约，调它不会让已翻好的译文过期，也不会让任务重跑。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationGenerationSettings {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: u32,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
}

impl Default for TranslationGenerationSettings {
    fn default() -> Self {
        Self {
            temperature: Some(0.0),
            top_p: None,
            max_output_tokens: DEFAULT_TRANSLATION_OUTPUT_TOKENS,
            presence_penalty: None,
            frequency_penalty: None,
        }
    }
}

impl TranslationGenerationSettings {
    pub fn validate(&self) -> Result<()> {
        validate_generation_bounds(
            self.temperature,
            self.top_p,
            self.presence_penalty,
            self.frequency_penalty,
        )?;
        ensure!(self.max_output_tokens > 0, "最大输出 token 数必须是正整数");
        Ok(())
    }
}

/// 采样参数的公共边界：留空表示「用服务端默认值」，给了值就必须是区间内的有限数字。
/// 问答与翻译只有各自的默认值不同，取值范围完全一致。
fn validate_generation_bounds(
    temperature: Option<f32>,
    top_p: Option<f32>,
    presence_penalty: Option<f32>,
    frequency_penalty: Option<f32>,
) -> Result<()> {
    for (label, value, minimum, maximum) in [
        ("温度（Temperature）", temperature, 0.0, 2.0),
        ("核采样（Top P）", top_p, 0.0, 1.0),
        ("出现惩罚（Presence Penalty）", presence_penalty, -2.0, 2.0),
        (
            "频率惩罚（Frequency Penalty）",
            frequency_penalty,
            -2.0,
            2.0,
        ),
    ] {
        ensure!(
            value.is_none_or(|value| value.is_finite() && (minimum..=maximum).contains(&value)),
            "{label} 必须是 {minimum}～{maximum} 之间的有限数字，或留空使用服务默认值"
        );
    }
    Ok(())
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
