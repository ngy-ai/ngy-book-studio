//! The error kinds and how a failure is classified into them.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobLogErrorKind {
    Provider,
    Database,
    Io,
    InvalidData,
    Timeout,
    Translation,
    ContextWindowExceeded,
    StreamProtocol,
    Cancelled,
    InvalidJson,
    IncompleteJson,
    InvalidSchema,
    IncompleteReasoning,
    AmbiguousJson,
    MissingJson,
    SegmentCountMismatch,
    UnknownSegmentId,
    DuplicateSegmentId,
    MissingSegmentId,
    EmptySegmentText,
    InvalidSegmentText,
    /// 模型响应在输出上限处被截断，没有写出完整答案。
    ResponseTruncated,
    Unknown,
}

impl JobLogErrorKind {
    /// Fixed explanation of one category, for the task window. The table is
    /// closed, so a label can never echo provider text, a book body or a URL.
    /// It also describes a failure without depending on the error's `Display`.
    pub fn label(self) -> &'static str {
        use JobLogErrorKind::*;
        match self {
            Provider => "模型服务返回错误",
            Database => "本地数据库错误",
            Io => "本地读写错误",
            InvalidData => "任务数据无效",
            Timeout => "模型请求超时或长时间无数据",
            Translation => "译文内容不符合分段协议",
            ContextWindowExceeded => "超出模型上下文长度",
            StreamProtocol => "流式响应协议错误",
            Cancelled => "请求被取消",
            InvalidJson => "响应不是合法 JSON",
            IncompleteJson => "响应 JSON 不完整",
            InvalidSchema => "响应结构不符合分段协议",
            IncompleteReasoning => "模型思考未结束",
            AmbiguousJson => "响应包含多个候选答案",
            MissingJson => "响应中没有找到答案",
            SegmentCountMismatch => "片段数量与原文不一致",
            UnknownSegmentId => "响应包含未知片段编号",
            DuplicateSegmentId => "响应重复了同一片段编号",
            MissingSegmentId => "响应缺少片段编号",
            EmptySegmentText => "响应包含空译文片段",
            InvalidSegmentText => "译文片段格式无效",
            ResponseTruncated => "模型响应在输出上限处被截断",
            Unknown => "未归类的错误",
        }
    }
}

/// Inspect typed errors through the existing safe classifier; never inspect or
/// persist `Display`, a context message, an HTTP body or a URL.
pub fn classify_error(error: &anyhow::Error) -> JobLogErrorKind {
    use JobLogErrorKind::*;
    match crate::ai::ai_diagnostics::error_kind(error) {
        "database" => Database,
        "io" | "json_io" => Io,
        "http_timeout" | "tool_timeout" => Timeout,
        "http_rejected" | "http_connect" | "http_body" | "http_decode" | "http_transport" => {
            Provider
        }
        "context_window_exceeded" => ContextWindowExceeded,
        "stream_protocol" | "incomplete_tool_arguments" => StreamProtocol,
        "cancelled" => Cancelled,
        "invalid_json" | "json_syntax" => InvalidJson,
        "incomplete_json" | "json_eof" => IncompleteJson,
        // 位置数组是「结构无效」的一个更具体的子类：模型按段答了，只是把
        // `{"id","text"}` 写成了 serde 的位置序列。任务日志沿用同一分类，
        // 逐片段回退的判定在读 `ResponseError::kind()` 时已经能把两者分开。
        // 批信封的形状问题（块数不符、块编号对不上）同样归到「结构不符合分段协议」：
        // 它们都表示模型没有按协议作答，而具体分类在读 `ResponseError::kind()` 时已经
        // 分开了（批路径还会据此拆半重发、退回单块）。
        "invalid_schema"
        | "json_data"
        | "positional_segments"
        | "keep_with_text"
        | "batch_block_count_mismatch"
        | "batch_block_ids" => InvalidSchema,
        "incomplete_reasoning" => IncompleteReasoning,
        "ambiguous_json" => AmbiguousJson,
        "missing_json" => MissingJson,
        "segment_count_mismatch" => SegmentCountMismatch,
        "unknown_segment_id" => UnknownSegmentId,
        "duplicate_segment_id" => DuplicateSegmentId,
        "missing_segment_id" => MissingSegmentId,
        "empty_segment_text" => EmptySegmentText,
        "invalid_segment_text" => InvalidSegmentText,
        "response_truncated" => ResponseTruncated,
        "agent_configuration" | "tool_arguments" | "agent_limit" | "scope_violation" => InvalidData,
        _ => Unknown,
    }
}
