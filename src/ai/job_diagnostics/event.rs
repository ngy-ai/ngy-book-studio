//! The events, transitions and outcomes a job log records.

use super::*;

/// The event is the complete message vocabulary. Do not replace this with a
/// free-form string API: providers can echo private input in any error field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobLogEvent {
    Queued,
    Recovered,
    RunStarted,
    SourceLoaded,
    ItemStarted,
    ModelRequested,
    ModelCompleted,
    ProtocolRejected,
    ProtocolCorrection,
    ProtocolSkipped,
    /// 模型请求本身失败（传输、超时、流协议、上下文超限）：这一块按跳过处理并保留
    /// 原文。与 `ProtocolSkipped` 分开，是因为两者要说的话不一样 —— 一个是「模型答得
    /// 不对」，一个是「根本没答上来」，用户据此判断该换模型还是该查端点。
    ProviderSkipped,
    /// 多片段文本块在两次严格尝试后改用逐片段请求重新翻译。
    SegmentFallback,
    /// 整个文本块在协议纠正后仍不合格，按冻结输入重发一轮（最后一轮才带逐片段回退）。
    BlockRetry,
    ItemSaved,
    PublicationStarted,
    RunSucceeded,
    RunFailed,
    StepFailed,
    PauseRequested,
    ResumeRequested,
    RetryRequested,
    CancelRequested,
    RetranslateRequested,
    Paused,
    Cancelled,
    Superseded,
    Skipped,
    WaitingForPages,
    CacheHit,
    NoTranslatableSegments,
    PreviewOnlySkipped,
}

impl JobLogEvent {
    pub(crate) fn presentation(self) -> (JobLogLevel, &'static str, &'static str) {
        use JobLogLevel::{Error, Info, Warning};
        match self {
            Self::Queued => (Info, "queue", "任务已加入执行队列"),
            Self::Recovered => (Info, "queue", "已从持久化队列恢复任务调度"),
            Self::RunStarted => (Info, "run", "任务开始执行"),
            Self::SourceLoaded => (Info, "source", "已加载并校验任务来源"),
            Self::ItemStarted => (Info, "item", "开始处理当前单元"),
            Self::ModelRequested => (Info, "model", "已发起模型请求"),
            Self::ModelCompleted => (Info, "model", "模型响应接收完成"),
            Self::ProtocolRejected => (Warning, "protocol", "模型响应未通过协议校验"),
            Self::ProtocolCorrection => (Info, "protocol", "使用冻结输入发起一次协议纠正"),
            Self::ProtocolSkipped => (
                Warning,
                "protocol",
                "模型响应在自动纠正后仍不符合分段协议，已跳过当前文本块并保留原文",
            ),
            Self::ProviderSkipped => (
                Warning,
                "model",
                "模型请求失败（端点或模型不可用），已跳过当前文本块并保留原文",
            ),
            Self::SegmentFallback => (
                Info,
                "protocol",
                "多片段响应仍不完整，改用逐片段请求重新翻译当前文本块",
            ),
            Self::BlockRetry => (
                Info,
                "protocol",
                "整个文本块的响应仍不合格，已按冻结输入重发该块",
            ),
            Self::ItemSaved => (Info, "persist", "当前单元结果已持久化"),
            Self::PublicationStarted => (Info, "publish", "开始发布完整任务结果"),
            Self::RunSucceeded => (Info, "run", "任务执行完成"),
            Self::RunFailed => (Error, "run", "任务执行失败"),
            Self::StepFailed => (Error, "item", "当前处理步骤失败"),
            Self::PauseRequested => (Info, "control", "已接受暂停请求"),
            Self::ResumeRequested => (Info, "control", "已接受继续请求"),
            Self::RetryRequested => (Info, "control", "已接受重试请求"),
            Self::CancelRequested => (Info, "control", "已接受取消请求"),
            Self::RetranslateRequested => (Info, "control", "已清除旧译文并重排翻译任务"),
            Self::Paused => (Info, "run", "任务已暂停，保留执行进度"),
            Self::Cancelled => (Info, "run", "任务已取消"),
            Self::Superseded => (Warning, "identity", "任务身份已变化，停止本次执行"),
            Self::Skipped => (Info, "item", "当前单元无需处理"),
            Self::WaitingForPages => (Warning, "dependency", "等待视觉页面生成后重试"),
            Self::CacheHit => (Info, "cache", "已复用通过身份校验的缓存结果"),
            Self::NoTranslatableSegments => (Info, "item", "当前单元没有需要翻译的文字片段"),
            Self::PreviewOnlySkipped => (Info, "item", "当前单元仅用于预览，跳过本次处理"),
        }
    }
}
