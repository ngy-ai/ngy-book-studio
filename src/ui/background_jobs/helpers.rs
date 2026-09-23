//! Small formatters used by the detail panes.

use super::*;

/// Text of the open tab. `None` while the payload is still loading or failed,
/// which is exactly when the panel shows a status line instead of content.
pub(crate) fn block_detail_text(
    state: &BlockDetailState,
    detail: Option<&TranslationBlockDetail>,
) -> Option<String> {
    // The replayed answer is not part of the pinned payload — it only exists
    // after the user asked the model again — so it is answered before the
    // payload gate below rather than through it.
    if state.tab == BlockDetailTab::Response {
        return state
            .probe
            .result
            .as_ref()
            .map(|probe| prettify_json(&probe.response_text));
    }
    let detail = detail?;
    let request = &detail.request;
    Some(match state.tab {
        BlockDetailTab::Overview => {
            let target = translation_language_label(&request.target_language)
                .unwrap_or(request.target_language.as_str());
            [
                format!("文本块序号: #{}", detail.ordinal),
                format!("文本块标识: {}", detail.block_id),
                format!("所属单元: {}", detail.unit_id),
                format!(
                    "单元序号: 第 {} 章{}",
                    detail.unit_ordinal + 1,
                    match detail.unit_title.trim() {
                        "" => String::new(),
                        title => format!("（{title}）"),
                    }
                ),
                format!("单元版本: {}", detail.unit_revision),
                format!("原文长度: {} 字符", detail.source_text.chars().count()),
                format!("片段数量: {}", detail.segments.len()),
                format!("片段字符数: {}", segment_char_label(&detail.segments)),
                String::new(),
                format!("调用模型: {}", request.model),
                format!("目标语言: {target}（{}）", request.target_language),
                format!(
                    "原文语言标记: {}",
                    request
                        .source_language
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or("未设置")
                ),
                match request.temperature {
                    Some(temperature) => format!("temperature: {temperature}"),
                    // 设置里留空 = 请求体里不带这个键，面板必须照实说，而不是显示 0。
                    None => "temperature: 未设置（服务端默认）".to_string(),
                },
                format!("max_tokens: {}", request.max_tokens),
                "reasoning_effort: none".to_string(),
                "messages: 2（system + user）".to_string(),
                format!(
                    "system 长度: {} 字符",
                    request.system_prompt.chars().count()
                ),
                format!("user 长度: {} 字符", request.user_content.chars().count()),
                format!("body 长度: {} 字节", request.body.len()),
            ]
            .join("\n")
        }
        BlockDetailTab::Source => detail.source_text.clone(),
        BlockDetailTab::Body => prettify_json(&request.body),
        BlockDetailTab::Prompt => request.system_prompt.clone(),
        BlockDetailTab::Message => prettify_json(&request.user_content),
        // The replayed answer itself, never the replayed *result*: a valid JSON
        // object is indented for reading, anything else (a fenced block, a
        // truncated object, provider prose) is shown byte for byte, because that
        // shape is the finding.
        BlockDetailTab::Response => unreachable!("the replayed answer is returned above"),
    })
}

/// How one replayed stream ended, in the window's words. Ending early is the
/// most common reason a block "fails" while the model looks healthy, so the
/// model's own `finish_reason` is named whenever there is one.
pub(crate) fn probe_end_label(probe: &TranslationBlockProbe) -> String {
    match probe.end {
        "done" => "模型正常收尾".to_string(),
        "unexpected_finish_reason" => format!(
            "模型提前结束响应（finish_reason={}）",
            probe.finish_reason.unwrap_or("other")
        ),
        "stream_eof" => "响应流在没有收尾标记的情况下关闭".to_string(),
        "size_limit" => "响应超过大小上限，只显示已收到的部分".to_string(),
        "tool_call" => "模型意外请求了工具".to_string(),
        other => other.to_string(),
    }
}

/// What the worker's own strict decoder made of the replayed answer. The window
/// renders the category label instead of re-implementing the protocol rules.
pub(crate) fn probe_decode_label(probe: &TranslationBlockProbe) -> String {
    match probe.decode_error_kind {
        None => format!(
            "严格解码通过：{} 个片段",
            probe.decoded_segments.unwrap_or(0)
        ),
        Some(kind) => {
            let coordinates = match (probe.decode_error_line, probe.decode_error_column) {
                (Some(line), Some(column)) => format!("（第 {line} 行第 {column} 列）"),
                _ => String::new(),
            };
            format!("严格解码失败：{}{coordinates}", kind.label())
        }
    }
}

/// Per-segment character counts of one block. Numbers only, so it can be read
/// without printing the source text again.
pub(crate) fn segment_char_label(segments: &[String]) -> String {
    if segments.is_empty() {
        return "无片段".to_string();
    }
    segments
        .iter()
        .map(|segment| segment.chars().count().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pretty-prints a JSON payload for display. A value that cannot be re-parsed
/// is shown verbatim rather than silently dropped.
pub(crate) fn prettify_json(raw: &str) -> String {
    let parsed = match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => value,
        Err(_) => return raw.to_string(),
    };
    match serde_json::to_string_pretty(&parsed) {
        Ok(pretty) => pretty,
        Err(_) => raw.to_string(),
    }
}
