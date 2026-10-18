//! Opening the requests of one batch and pacing them.

use super::*;

/// 一发请求的逐块结论：块号与那一块自己的结果一一对应。
///
/// 「每请求文本块数」大于 1 时一发请求带走多块，所以结果不再是一块对一发；块级失败按
/// `Err` 原样带出来，由 [`absorb_translation_step`] 归位或跳过。
pub(crate) type TranslationStepResult = Result<BlockStep<Vec<(usize, Result<BlockWalk>)>>>;

/// 一轮里打开的一个请求：它覆盖的块号，以及每一块的逐块结果（与块号一一对应）。
///
/// 一个请求现在可能带走多个块（端点级的「每请求文本块数」），所以结果不再是一块对一发：
/// 块号单独带出来，walk 据此把每一块放回 `resolved`，提交仍是按块序号的。
pub(crate) type TranslationRequestFuture<'a> = BoxFuture<'a, (Vec<usize>, TranslationStepResult)>;

/// 把攒好的一批发出去。
///
/// 「每请求文本块数」为 1 时这里就是老的「一块一请求」路径：`translate_block_batch`
/// 把手上的那**一**块交给单块路径，严格尝试、一次协议纠正、
/// `TRANSLATION_BLOCK_RETRIES` 轮重发与逐片段回退都在那边，行为与加批之前一致。
pub(crate) fn spawn_translation_request<'a>(
    in_flight: &mut FuturesUnordered<TranslationRequestFuture<'a>>,
    inner: &Arc<IndexingInner>,
    job: &'a db::index_jobs::IndexJob,
    services: &TranslationServices,
    target_language: &'a str,
    source_language: Option<&'a str>,
    blocks: Vec<TranslationBatchBlock<'a>>,
) {
    let ordinals = blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>();
    let inner = Arc::clone(inner);
    let services = services.clone();
    in_flight.push(
        async move {
            let walk = translate_block_batch(
                &inner,
                job,
                &services,
                target_language,
                source_language,
                &blocks,
            )
            .await;
            // 这一发收尾后才轮到本槽位休息，所以休眠永远发生在请求之后，不会挤占
            // 模型的响应时间。批里的块共用这一次休息：间隔按请求计，不按块计。
            pause_after_translation_request(&inner, &walk).await;
            (ordinals, walk)
        }
        .boxed(),
    );
}

/// 一发模型请求收尾后的块级节流，也就是「任务间隔」真正生效的地方。
///
/// 间隔原本只作用于两个任务之间（`worker_loop`），而一整本书就是一个任务：书内几十上百
/// 次请求之间毫无停顿，本机模型（Ollama 等）会连续把 GPU 吃到满载 —— 现场 2026-09-20，
/// 用户把间隔调成 5000 毫秒也不起作用，持续满载时显示器黑屏。现在按请求计：每个并发
/// 槽位发出一发就休眠一次，窗口仍按「任务并发」填，整体请求频率因此被限制在
/// 「每个槽位一发 / 间隔」。缓存命中与「不必处理」的块不发请求，也就不会休眠。
///
/// 停止请求（暂停、取消、换版本）不休眠：这类结果要求马上把控制权交回 `walk`，
/// 让它清空窗口并按终态发布，多睡一个间隔只会拖慢用户的暂停操作。
pub(crate) async fn pause_after_translation_request<T>(
    inner: &Arc<IndexingInner>,
    walk: &Result<BlockStep<T>>,
) {
    if matches!(walk, Ok(BlockStep::Stopped(_))) {
        return;
    }
    let interval = inner.scheduling().interval;
    if !interval.is_zero() {
        tokio::time::sleep(interval).await;
    }
}
