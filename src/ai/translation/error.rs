//! The typed response errors the retry loop and the UI react to.

use super::*;

/// A provider-output failure whose diagnostics contain only fixed categories
/// and numeric positions/counts. It is safe to log this type, never the JSON
/// deserializer's error message (which can contain provider-controlled fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseError {
    pub(crate) kind: &'static str,
    pub(crate) json_line: Option<usize>,
    pub(crate) json_column: Option<usize>,
    pub(crate) expected_segments: Option<usize>,
    pub(crate) actual_segments: Option<usize>,
}

impl ResponseError {
    pub(crate) fn new(kind: &'static str) -> Self {
        Self {
            kind,
            json_line: None,
            json_column: None,
            expected_segments: None,
            actual_segments: None,
        }
    }

    pub(crate) fn json(error: &serde_json::Error) -> Self {
        let kind = match error.classify() {
            serde_json::error::Category::Data => "invalid_schema",
            serde_json::error::Category::Eof => "incomplete_json",
            serde_json::error::Category::Syntax | serde_json::error::Category::Io => "invalid_json",
        };
        Self {
            json_line: Some(error.line()),
            json_column: Some(error.column()),
            ..Self::new(kind)
        }
    }

    /// 响应在输出上限处被截断（`finish_reason=length`）：流本身是正常收尾的，只是
    /// 模型没有写出一个完整的 JSON 对象。它和解析失败同类——这一次请求没有产出可用
    /// 答案——因此按文本块跳过，而不是让整次运行失败。
    pub(crate) fn response_truncated() -> Self {
        Self::new("response_truncated")
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// Coordinates refer to the selected JSON object, when one was found.
    pub fn json_line(&self) -> Option<usize> {
        self.json_line
    }

    pub fn json_column(&self) -> Option<usize> {
        self.json_column
    }

    pub fn expected_segments(&self) -> Option<usize> {
        self.expected_segments
    }

    pub fn actual_segments(&self) -> Option<usize> {
        self.actual_segments
    }
}

impl fmt::Display for ResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            "invalid_schema" => "翻译模型返回的分段 JSON 结构无效",
            "positional_segments" => "翻译模型把分段写成了位置数组，缺少 id 与 text 字段名",
            "incomplete_json" => "翻译模型返回的分段 JSON 不完整",
            "incomplete_reasoning" => "翻译模型没有完整结束思考内容",
            "ambiguous_json" => "翻译模型返回了多个 JSON 结果，无法确认唯一译文",
            "missing_json" => "翻译模型没有返回分段 JSON",
            "response_truncated" => "翻译模型响应在输出上限处被截断",
            "segment_count_mismatch" => "翻译模型返回的片段数量与原文不一致",
            "batch_block_count_mismatch" => "翻译模型返回的文本块数量与这一批不一致",
            "batch_block_ids" => "翻译模型返回的文本块编号对不上这一批",
            "unknown_segment_id" => "翻译模型返回了未知片段编号",
            "duplicate_segment_id" => "翻译模型返回了重复片段编号",
            "missing_segment_id" => "翻译模型遗漏了原文片段",
            "empty_segment_text" => "翻译模型返回了空片段",
            "keep_with_text" => "翻译模型声明片段不需要翻译，却又返回了译文文本",
            "invalid_segment_text" => "翻译模型返回的片段包含 NUL 字符",
            _ => "翻译模型未返回有效的分段 JSON",
        })
    }
}

impl std::error::Error for ResponseError {}
