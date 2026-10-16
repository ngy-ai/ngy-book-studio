//! 把多个文本块打包进一次请求：信封、切批预算与逐块解析。
//!
//! 批只是**传输信封**：每个块在请求里带自己的 `source` 与 `segments`，回复里每一项仍按
//! 片段给出译文，落库形状（[`StoredTranslation`]）、缓存键、重试单位与任务游标全部保持
//! 按块。因此 `EXECUTION_IDENTITY_PROTOCOL` 不升版：已经存下的译文照旧命中，把一次运行
//! 从单块改成多块不会让整本书重翻一遍。
//!
//! 三条边界：
//!
//! - **同一个内容单元**（同一章）的块才能进同一批：`book`/`chapter` 语境字段是每批一份，
//!   混进另一章的块会让这两个字段撒谎（规则由调用方保证，见 [`batch_request_input`]）。
//! - **信封坏就退回单块**：批这一层不重试、不纠正。信封不合格（不是批形状、块数不符、
//!   块编号对不上）由调用方拆半重发，最后退回单块请求 —— 那条路径带着它自己的严格尝试、
//!   协议纠正与逐片段回退。宽容解码只做一份，不在这层重抄。
//! - **单块的形状问题只影响那一块**：某一块的片段数量/编号不合格时，只有它走单块重试，
//!   同一批里已经答对的块照常落库。
//!
//! 打包同时受三个上限约束：一次请求要花掉的输出 token（输出上限的
//! [`TRANSLATION_BATCH_OUTPUT_SHARE_PERCENT`]）、批内块数、批请求体字节数。**输出预算是
//! 真正的天花板** —— 512k 的输入窗口在这里没有意义，译文与原文同阶，而 `max_output_tokens`
//! 是每请求的预算。

use super::*;

/// 批里的一个文本块。它在请求里的编号就是它在批里的位置（`0..N`），与任务游标里的
/// `ordinal` 无关：同一个块单独发与放进批里发，请求体里它自己那一份负载逐字节相同。
#[derive(Clone, Copy, Debug)]
pub struct TranslationBatchItem<'a> {
    pub source: &'a TranslationSource,
    pub context: TranslationContext<'a>,
}

/// 批内块数的硬上限。「自动」档在远程端点上用它，用户也可以把端点的「每请求文本块数」
/// 调到这个值。再往上收益很小（输出预算先撑不住），而一旦模型答错，一次要退回的块更多。
pub const MAX_TRANSLATION_BATCH_BLOCKS: usize = 8;

/// 端点设置里「每请求文本块数」能填的最大值。
pub const MAX_TRANSLATION_BATCH_BLOCKS_SETTING: u32 = 64;

/// 一次请求的输出预算里留给译文的百分比。剩下的留给 JSON 外壳、纠正重试与估算误差 ——
/// 顶到输出上限的代价是整批被截断，等于白花一次请求。
pub const TRANSLATION_BATCH_OUTPUT_SHARE_PERCENT: u32 = 60;

/// 批请求体的字节上限。单块的 64 KiB 上限不变，批按份数放宽，但仍要有天花板：8 个块
/// 各自接近上限时请求体能到几百 KiB。
pub const MAX_TRANSLATION_BATCH_REQUEST_BYTES: usize = 256 * 1024;

/// 批里每一项除块负载之外的外壳开销（`{"id":0,` 与分隔逗号）。
const BATCH_ITEM_OVERHEAD_BYTES: usize = 32;

/// 打包预算：一次请求里装得下多少。
#[derive(Clone, Copy, Debug)]
pub struct TranslationBatchBudget {
    /// 这一批能花在译文上的输出 token。
    pub output_tokens: usize,
    /// 批内块数上限。
    pub max_blocks: usize,
    /// 批请求体字节上限。
    pub max_bytes: usize,
}

impl TranslationBatchBudget {
    /// 按一次请求的生成参数与端点策略算出预算。
    pub fn new(max_output_tokens: u32, max_blocks: usize) -> Self {
        Self {
            output_tokens: max_output_tokens as usize
                * TRANSLATION_BATCH_OUTPUT_SHARE_PERCENT as usize
                / 100,
            max_blocks: max_blocks.clamp(1, MAX_TRANSLATION_BATCH_BLOCKS),
            max_bytes: MAX_TRANSLATION_BATCH_REQUEST_BYTES,
        }
    }
}

/// 候选块进批时要看的两项估算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranslationBatchEstimate {
    pub output_tokens: usize,
    pub request_bytes: usize,
}

/// 一个块的估算。
///
/// 输出 token 用**源文字符数**当上界：译文与原文同阶（中文目标约 1 token/字，拉丁目标
/// 约 0.3 token/字符），取字符数既不低估，也不必在这里猜分词器。低估的代价是整批被输出
/// 上限截断 —— 那一次请求白花，所以宁可保守。
pub fn estimate_batch_block(
    source: &TranslationSource,
    context: &TranslationContext<'_>,
) -> TranslationBatchEstimate {
    TranslationBatchEstimate {
        output_tokens: source.text.chars().count(),
        request_bytes: source.request_input(context).len() + BATCH_ITEM_OVERHEAD_BYTES,
    }
}

/// 打包器：一边规划块、一边决定它还能不能塞进当前这一批。
///
/// 增量式的形状是调用方的需要：规划一个块会读缓存、记诊断，不能为了「先看一整段再切批」
/// 把范围外的块也规划一遍。空批永远收得下第一块 —— 打包是为了少发请求，不是「装不下就
/// 不翻」。
#[derive(Clone, Copy, Debug)]
pub struct TranslationBatchPacker {
    budget: TranslationBatchBudget,
    output_tokens: usize,
    request_bytes: usize,
    blocks: usize,
}

impl TranslationBatchPacker {
    pub fn new(budget: TranslationBatchBudget) -> Self {
        Self {
            budget,
            output_tokens: 0,
            request_bytes: 0,
            blocks: 0,
        }
    }

    /// 这一块还塞得进当前这一批吗。空批永远为真。
    pub fn accepts(&self, estimate: &TranslationBatchEstimate) -> bool {
        if self.blocks == 0 {
            return true;
        }
        self.blocks < self.budget.max_blocks
            && self.output_tokens + estimate.output_tokens <= self.budget.output_tokens
            && self.request_bytes + estimate.request_bytes <= self.budget.max_bytes
    }

    pub fn push(&mut self, estimate: &TranslationBatchEstimate) {
        self.blocks += 1;
        self.output_tokens += estimate.output_tokens;
        self.request_bytes += estimate.request_bytes;
    }

    pub fn len(&self) -> usize {
        self.blocks
    }

    pub fn is_empty(&self) -> bool {
        self.blocks == 0
    }
}

/// 这个端点该用多大的批。`configured` 就是端点设置里的「每请求文本块数」：`0` = 自动，
/// `1` = 关闭打包，`> 1` = 上限。
///
/// 自动档按回环端点判定，与「每分钟请求数上限」的自动档同一套语义（本机端点不限速、
/// 远程端点按服务商额度），用户不必为「本机跑小模型」再学一个开关。本机默认不打包还有
/// 一条实测理由：弱模型更容易把多个块或片段合并成一条，而批会让一处坏形状影响更多块。
pub fn batch_block_limit(configured: u32, endpoint_is_loopback: bool) -> usize {
    match configured {
        0 if endpoint_is_loopback => 1,
        0 => MAX_TRANSLATION_BATCH_BLOCKS,
        configured => (configured as usize).min(MAX_TRANSLATION_BATCH_BLOCKS_SETTING as usize),
    }
}

/// 批回复的信封。与单块形状一样严格（`deny_unknown_fields`）：外壳不对就整批退回单块
/// 路径，不在批这一层猜模型意图。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchResponseBlock {
    id: usize,
    translations: Vec<ResponseSegment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchResponse {
    blocks: Vec<BatchResponseBlock>,
}

/// 一次批回复的解析结果。
///
/// 外层 `Err`：**信封**不合格 —— 不是批形状、块数不符、块编号重复或越界。调用方据此拆半
/// 重发，最后退回单块请求。
///
/// 外层 `Ok`：每一项对应输入里的一个块，顺序与输入相同。其中 `Err` 的那一项是**这一块
/// 自己**的片段形状问题（片段数量、编号、`keep`、空白、NUL），同一批里其它块不受影响。
pub fn parse_batch_response(
    items: &[TranslationBatchItem<'_>],
    response: &str,
    identity: &str,
) -> Result<Vec<std::result::Result<StoredTranslation, ResponseError>>, ResponseError> {
    // 不变量：批路径上的执行身份来自配置。空身份是调用方用错了（不是模型的问题），
    // 仍按信封错误返回，让整批退回单块路径 —— 那里会明确报出「翻译执行身份不能为空」。
    if identity.trim().is_empty() {
        return Err(ResponseError::new("invalid_schema"));
    }
    let decoded = decode_batch_response(response)?;
    if decoded.blocks.len() != items.len() {
        return Err(ResponseError {
            expected_segments: Some(items.len()),
            actual_segments: Some(decoded.blocks.len()),
            ..ResponseError::new("batch_block_count_mismatch")
        });
    }
    // 编号就是位置：重复、越界或缺失都在这里被堵住，缺一个也不会去猜是哪一块。
    let mut slots: Vec<Option<BatchResponseBlock>> = (0..items.len()).map(|_| None).collect();
    for block in decoded.blocks {
        let Some(slot) = slots.get_mut(block.id) else {
            return Err(ResponseError::new("batch_block_ids"));
        };
        if slot.is_some() {
            return Err(ResponseError::new("batch_block_ids"));
        }
        *slot = Some(block);
    }
    Ok(items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            // 块数与编号都在上面查过，这里必定有值；缺了也只报编号对不上，不 panic。
            let Some(block) = slots[index].take() else {
                return Err(ResponseError::new("batch_block_ids"));
            };
            restore_segments(item.source, block.translations, identity).map_err(response_error_of)
        })
        .collect())
}

/// 把单块回复里的 `ResponseError` 取出来；不是它的只可能是调用方用错了不变量
/// （空执行身份、没有可翻译片段），而批里的每一项都来自「已规划成要发请求」的块。
/// 那种情况下归到结构无效，让这一块退回单块路径重试一次，不把整批判死。
fn response_error_of(error: anyhow::Error) -> ResponseError {
    match error.downcast_ref::<ResponseError>() {
        Some(detail) => detail.clone(),
        None => ResponseError::new("invalid_schema"),
    }
}

/// 找到批回复里的那一个 JSON 容器。
///
/// 只做单块路径里那几件无论如何都必要的事：剥掉显式闭合的思考块、去掉首尾空白、容忍
/// 模型套在 JSON 外面的说明与围栏。**位置数组、丢掉外层键、丢掉 `translations` 外壳
/// 这些宽容解码只在单块路径上有**：信封不合格的整批会退回单块请求，那里每块都有自己的
/// 严格尝试、纠正与逐片段回退，不必在这一层再抄一份。
fn decode_batch_response(response: &str) -> std::result::Result<BatchResponse, ResponseError> {
    let mut body = response.trim_matches(is_matching_whitespace);
    while let Some(thinking) = body.strip_prefix("<think>") {
        let end = thinking
            .find("</think>")
            .ok_or_else(|| ResponseError::new("incomplete_reasoning"))?;
        body = thinking[end + "</think>".len()..].trim_matches(is_matching_whitespace);
    }
    if body.starts_with('{')
        && let Ok(decoded) = serde_json::from_str::<BatchResponse>(body)
    {
        return Ok(decoded);
    }
    let candidate = single_json_container(body)?;
    serde_json::from_str::<BatchResponse>(candidate).map_err(|error| ResponseError::json(&error))
}
