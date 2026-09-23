//! Parsing the optional parameters the window collects.

use super::*;

pub(crate) fn optional_parameter_text(value: Option<f32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

pub(crate) fn optional_parameter_input<V>(
    value: Option<f32>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(optional_parameter_text(value))
            .placeholder("留空使用服务端默认值")
    })
}

/// 一组生成参数的当前取值。问答与翻译的字段完全相同、默认值与目标类型不同，所以界面
/// 用这一份形状统一驱动：构造输入框、读回草稿、恢复默认，最后交给 `chat()` 或
/// `translation()` 决定它属于哪个模型并做边界校验。
pub(crate) struct GenerationValues {
    pub(crate) temperature: Option<f32>,
    pub(crate) top_p: Option<f32>,
    pub(crate) max_output_tokens: u32,
    pub(crate) presence_penalty: Option<f32>,
    pub(crate) frequency_penalty: Option<f32>,
}

impl From<&ChatGenerationSettings> for GenerationValues {
    fn from(settings: &ChatGenerationSettings) -> Self {
        Self {
            temperature: settings.temperature,
            top_p: settings.top_p,
            max_output_tokens: settings.max_output_tokens,
            presence_penalty: settings.presence_penalty,
            frequency_penalty: settings.frequency_penalty,
        }
    }
}

impl From<&TranslationGenerationSettings> for GenerationValues {
    fn from(settings: &TranslationGenerationSettings) -> Self {
        Self {
            temperature: settings.temperature,
            top_p: settings.top_p,
            max_output_tokens: settings.max_output_tokens,
            presence_penalty: settings.presence_penalty,
            frequency_penalty: settings.frequency_penalty,
        }
    }
}

impl GenerationValues {
    /// 读回一组输入框里的草稿。留空表示不给这个参数，交给服务端默认值。
    pub(crate) fn from_inputs(inputs: &GenerationInputs, cx: &App) -> Result<Self> {
        let [
            temperature,
            top_p,
            max_output_tokens,
            presence_penalty,
            frequency_penalty,
        ] = inputs.values(cx);
        values_from_text(
            temperature.as_ref(),
            top_p.as_ref(),
            max_output_tokens.as_ref(),
            presence_penalty.as_ref(),
            frequency_penalty.as_ref(),
        )
    }

    pub(crate) fn chat(self) -> Result<ChatGenerationSettings> {
        let settings = ChatGenerationSettings {
            temperature: self.temperature,
            top_p: self.top_p,
            max_output_tokens: self.max_output_tokens,
            presence_penalty: self.presence_penalty,
            frequency_penalty: self.frequency_penalty,
        };
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn translation(self) -> Result<TranslationGenerationSettings> {
        let settings = TranslationGenerationSettings {
            temperature: self.temperature,
            top_p: self.top_p,
            max_output_tokens: self.max_output_tokens,
            presence_penalty: self.presence_penalty,
            frequency_penalty: self.frequency_penalty,
        };
        settings.validate()?;
        Ok(settings)
    }
}

pub(crate) fn parse_chat_generation(
    temperature: &str,
    top_p: &str,
    max_output_tokens: &str,
    presence_penalty: &str,
    frequency_penalty: &str,
) -> Result<ChatGenerationSettings> {
    values_from_text(
        temperature,
        top_p,
        max_output_tokens,
        presence_penalty,
        frequency_penalty,
    )?
    .chat()
}

/// 翻译模型自己的生成参数。字段与问答相同，默认值（温度 0）与存储位置不同。
pub(crate) fn parse_translation_generation(
    temperature: &str,
    top_p: &str,
    max_output_tokens: &str,
    presence_penalty: &str,
    frequency_penalty: &str,
) -> Result<TranslationGenerationSettings> {
    values_from_text(
        temperature,
        top_p,
        max_output_tokens,
        presence_penalty,
        frequency_penalty,
    )?
    .translation()
}

/// 五段输入文本 → 一组取值。边界检查留给 `ChatGenerationSettings::validate` /
/// `TranslationGenerationSettings::validate`，两边的取值范围完全一致。
fn values_from_text(
    temperature: &str,
    top_p: &str,
    max_output_tokens: &str,
    presence_penalty: &str,
    frequency_penalty: &str,
) -> Result<GenerationValues> {
    fn optional_number(label: &str, value: &str) -> Result<Option<f32>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        value
            .parse::<f32>()
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{label}必须是数字，或留空使用服务端默认值"))
    }
    Ok(GenerationValues {
        temperature: optional_number("温度（Temperature）", temperature)?,
        top_p: optional_number("核采样（Top P）", top_p)?,
        max_output_tokens: max_output_tokens
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("最大输出 token 数必须是有效的正整数"))?,
        presence_penalty: optional_number("出现惩罚（Presence Penalty）", presence_penalty)?,
        frequency_penalty: optional_number("频率惩罚（Frequency Penalty）", frequency_penalty)?,
    })
}

pub(crate) fn parse_embedding_dimensions(value: &str) -> Result<usize> {
    let value = value.trim();
    let dimensions = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("embedding 维度必须是 1 到 {MAX_EMBEDDING_DIMENSIONS} 之间的正整数")
    })?;
    anyhow::ensure!(
        (1..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions),
        "embedding 维度必须是 1 到 {MAX_EMBEDDING_DIMENSIONS} 之间的正整数"
    );
    Ok(dimensions)
}
