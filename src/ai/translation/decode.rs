//! Finding the answer inside whatever wrapper the model produced.

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponseSegment {
    pub(crate) id: usize,
    pub(crate) text: String,
    /// 模型对「这个片段不需要翻译」的显式声明：`{"id":0,"text":"","keep":true}`。
    ///
    /// 缺失即为 `false`，所以留空 text 而不声明 keep 的答案仍按 `empty_segment_text`
    /// 拒绝 —— 协议要的是肯定性确认，不是「没写」。
    #[serde(default)]
    pub(crate) keep: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(crate) translations: Vec<ResponseSegment>,
}

/// Accept formatting around one answer, while keeping its actual JSON schema
/// strict. A leading, explicitly closed thinking block is not an answer. Other
/// prose and Markdown fences may surround exactly one complete JSON container.
///
/// A response that does not parse untouched is retried once with structural
/// full-width punctuation normalized. That repair can only rewrite characters
/// which must be structural in JSON, so a valid answer is never modified and a
/// response whose missing characters would have to be invented is still
/// rejected with the coordinates of the untouched text.
pub(crate) fn decode_response(response: &str) -> std::result::Result<Response, ResponseError> {
    match decode_untouched_response(response) {
        Ok(response) => Ok(response),
        Err(error) => {
            let (repaired, replaced) = repair_structural_punctuation(response);
            if replaced == 0 {
                return Err(error);
            }
            match decode_untouched_response(&repaired) {
                Ok(answer) => {
                    tracing::debug!(
                        target: "ngy_ai",
                        stage = "translation_response_repaired",
                        replaced_chars = replaced,
                        response_bytes = response.len(),
                        "Translation response repaired"
                    );
                    Ok(answer)
                }
                Err(_) => Err(error),
            }
        }
    }
}

pub(crate) fn decode_untouched_response(
    response: &str,
) -> std::result::Result<Response, ResponseError> {
    let mut body = response.trim_matches(is_matching_whitespace);
    while let Some(thinking) = body.strip_prefix("<think>") {
        let end = thinking
            .find("</think>")
            .ok_or_else(|| ResponseError::new("incomplete_reasoning"))?;
        body = thinking[end + "</think>".len()..].trim_matches(is_matching_whitespace);
    }
    match serde_json::from_str::<Response>(body) {
        Ok(response) if body.starts_with('{') => {
            require_segment_objects(body)?;
            return Ok(response);
        }
        // Serde structs can accept positional sequences. The protocol requires
        // one answer envelope, so a valid object/array inside another container
        // is never unwrapped.
        Ok(_) => return Err(ResponseError::new("invalid_schema")),
        Err(error) if error.is_data() => {
            // Small models may drop the `translations` wrapper; the segment list
            // itself is still accepted at the element level below.
            if let Some(response) = decode_bare_segment_list(body) {
                return Ok(response);
            }
            // 容器扫描的结论优先于笼统的 invalid_schema，使重复的数组答案与重复的
            // 对象答案一样报 ambiguous_json。
            if body.starts_with('[')
                && let Err(container) = single_json_container(body)
            {
                return Err(container);
            }
            return Err(ResponseError::json(&error));
        }
        Err(_) => {}
    }
    let candidate = single_json_container(body)?;
    if !candidate.starts_with('{') {
        // 顶层数组只可能是被丢掉的片段列表；包裹别的答案的数组仍在这里被拒绝。
        return decode_bare_segment_list(candidate)
            .ok_or_else(|| ResponseError::new("invalid_schema"));
    }
    match serde_json::from_str::<Response>(candidate) {
        Ok(response) => {
            require_segment_objects(candidate)?;
            return Ok(response);
        }
        Err(error) => {
            if let Some(response) = decode_bare_segment_list(candidate) {
                return Ok(response);
            }
            return Err(ResponseError::json(&error));
        }
    }
}

/// Serde also accepts positional sequences for nested structs. Reject that
/// representation explicitly, while decoding the original JSON into Response
/// above continues to reject duplicate fields instead of losing them in Value.
///
/// 位置数组（`{"translations":[[0,"译文"],…]}`）单独报 `positional_segments`：
/// 它和「结构无效」不是同一件事 —— 模型确实按段答了、段数往往也对，只是把
/// `{"id","text"}` 写成了 serde 的位置序列。多片段块遇到这种情况时，逐片段回退
/// 仍然是能救回来的（单片段只要求一个位置对，形状压力最小），因此需要与真正
/// 没答出答案的 `invalid_schema` 区分开，由 `fragment_fallback_applies` 决定。
pub(crate) fn require_segment_objects(candidate: &str) -> std::result::Result<(), ResponseError> {
    let value: serde_json::Value =
        serde_json::from_str(candidate).map_err(|error| ResponseError::json(&error))?;
    let segments = value
        .get("translations")
        .and_then(serde_json::Value::as_array);
    let Some(segments) = segments else {
        return Err(ResponseError::new("invalid_schema"));
    };
    if segments.iter().all(serde_json::Value::is_object) {
        return Ok(());
    }
    if segments.iter().all(serde_json::Value::is_array) {
        return Err(ResponseError::new("positional_segments"));
    }
    Err(ResponseError::new("invalid_schema"))
}

/// Some providers/models drop the `translations` wrapper and return the segment
/// list itself: either one bare segment object, or the array of segment objects
/// that should have been the value of `translations` (`qwen3.5:0.8b`, 2026-09-12:
/// one library run was rejected as `invalid_schema` on every block for exactly
/// that array). Fold only that shape into the expected envelope; every element
/// must still be a strict segment object, so a container that wraps a different
/// answer (for example `[{"translations":[…]}]`) or a positional segment
/// (`[0,"译文"]`) is left for the caller to reject rather than searched for a
/// nested result.
pub(crate) fn decode_bare_segment_list(payload: &str) -> Option<Response> {
    if payload
        .trim_matches(is_matching_whitespace)
        .starts_with('[')
    {
        return decode_segment_array(payload);
    }
    serde_json::from_str::<ResponseSegment>(payload)
        .ok()
        .map(|segment| Response {
            translations: vec![segment],
        })
}

pub(crate) fn decode_segment_array(payload: &str) -> Option<Response> {
    let translations = object_array_elements(payload)?
        .iter()
        .map(|element| serde_json::from_str::<ResponseSegment>(element).ok())
        .collect::<Option<Vec<_>>>()?;
    Some(Response { translations })
}

/// Splits one complete top-level array into its element slices, requiring every
/// element to be an object. Separators are validated here as well, so a missing,
/// doubled or trailing comma and a positional `[0,"译文"]` all stay rejected.
pub(crate) fn object_array_elements(payload: &str) -> Option<Vec<&str>> {
    let payload = payload.trim_matches(is_matching_whitespace);
    let inner = payload.strip_prefix('[')?.strip_suffix(']')?;
    let mut elements = Vec::new();
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    // 下一个非空白字节必须是元素起点：缺分隔符、多余逗号或尾随逗号都在这里失败。
    let mut expecting_element = true;
    for (offset, byte) in inner.bytes().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        if stack.is_empty() {
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' => {}
                b',' if !expecting_element => expecting_element = true,
                b'{' if expecting_element => {
                    expecting_element = false;
                    start = offset;
                    stack.push(byte);
                }
                _ => return None,
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => stack.push(byte),
            b'}' | b']' => {
                if stack.pop() != Some(if byte == b'}' { b'{' } else { b'[' }) {
                    return None;
                }
                if stack.is_empty() {
                    elements.push(&inner[start..=offset]);
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() || quoted || expecting_element {
        return None;
    }
    Some(elements)
}

/// Find only top-level containers. Braces inside strings and nested containers
/// cannot become alternate candidates; an array is kept as an entire candidate
/// so it cannot smuggle an otherwise valid answer through a wrong envelope.
pub(crate) fn single_json_container(response: &str) -> std::result::Result<&str, ResponseError> {
    let mut stack = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut candidate = None;
    for (offset, byte) in response.bytes().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                if stack.is_empty() {
                    start = offset;
                }
                stack.push(byte);
            }
            b'}' | b']' if !stack.is_empty() => {
                let expected = if byte == b'}' { b'{' } else { b'[' };
                if stack.pop() != Some(expected) {
                    return Err(ResponseError::new("invalid_json"));
                }
                if stack.is_empty() {
                    if candidate.is_some() {
                        return Err(ResponseError::new("ambiguous_json"));
                    }
                    candidate = Some(&response[start..=offset]);
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(ResponseError::new("incomplete_json"));
    }
    candidate.ok_or_else(|| ResponseError::new("missing_json"))
}
