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

pub(crate) fn parse_chat_generation(
    temperature: &str,
    top_p: &str,
    max_output_tokens: &str,
    presence_penalty: &str,
    frequency_penalty: &str,
) -> Result<ChatGenerationSettings> {
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
    let settings = ChatGenerationSettings {
        temperature: optional_number("温度（Temperature）", temperature)?,
        top_p: optional_number("核采样（Top P）", top_p)?,
        max_output_tokens: max_output_tokens
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("最大输出 token 数必须是有效的正整数"))?,
        presence_penalty: optional_number("出现惩罚（Presence Penalty）", presence_penalty)?,
        frequency_penalty: optional_number("频率惩罚（Frequency Penalty）", frequency_penalty)?,
    };
    settings.validate()?;
    Ok(settings)
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
