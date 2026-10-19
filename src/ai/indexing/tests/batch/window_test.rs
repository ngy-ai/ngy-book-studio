use super::*;

/// 一批带走整窗的块，而且每一块都落回它自己的块号。
///
/// 这是打包的意义所在：`任务并发` 只决定同时打开几个**请求**，一批里的块共用一发请求，
/// 但落库、游标与缓存仍按块 —— 所以逐块的译文必须各自正确，游标也必须停在书末。
///
/// 块列表按 EPUB 阅读章节解析，所以三个段落加上章标题一共四块。
#[test]
pub(crate) fn a_batch_request_carries_the_whole_window_and_every_block_keeps_its_own_row() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "三个段落 + 章标题");

    let (outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4],
        "整窗的块必须装进同一发请求，而不是分成两发"
    );
    assert_eq!(
        provider.sent_sources()[0],
        blocks
            .iter()
            .map(|block| block.source.text.clone())
            .collect::<Vec<_>>(),
        "批里的块顺序与冻结输入一致"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4);
    assert_eq!(
        (stats.batched_requests, stats.batched_blocks),
        (1, 4),
        "收尾汇总要能看出省了几发请求"
    );
    let shapes = provider
        .batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert!(shapes[0].2, "整窗的块走的是批信封，不是四发单块请求");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("整本翻完必须按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, blocks.len());
    assert!(finished.inflight.is_empty());
}

/// 一发批请求里的每一块都先进持久游标：任务窗口的「处理中」就是这一批的块号集合。
///
/// 窗口是游标的一部分，必须在请求发出前写好（`walk.rs` 的窗口段），否则任务窗口会显示
/// 「处理中」为空、而模型其实正在翻这一批。
#[test]
pub(crate) fn a_batch_publishes_every_block_of_its_window_as_inflight() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.attach_probe(fixture.db_path.clone(), job.id.clone());

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);
    assert_eq!(stats.saved, 4);

    let windows = provider.windows();
    assert_eq!(windows.len(), 1, "四块装一发：只有一个请求窗口");
    let (next_ordinal, inflight) = &windows[0];
    assert_eq!(*next_ordinal, blocks[0].ordinal);
    assert_eq!(
        inflight,
        &blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
        "这一发带走几块，游标里就有几块在飞 —— 「处理中」由此而来"
    );
}

/// 两个并发槽位、每发最多两块：谁都不许多带一块，也不许多开一发 —— 于是「处理中」的
/// 上界就是「任务并发 × 每请求文本块数」。
///
/// 请求形状与持久窗口各查一半：`batch_sizes` 说明两发各带满两块；探针在请求**开始时**读
/// 持久游标，所以读数就是那一发发出时「处理中」的实际块集合 —— 第一发可能在第二发还没攒
/// 完时就被轮询（攒批要拿门闩，等锁期间窗口照常被驱动，见 `walk.rs` 的
/// `lock_transitions`），因此读数只承诺两件事：永远是块列表的一段前缀（发布与提交都按
/// 序号），而且填到过 4 块 ——「任务并发 × 每请求文本块数」的上界既没有被越过，也不是靠
/// 少发请求凑出来的。
#[test]
pub(crate) fn a_batch_window_never_exceeds_concurrency_times_the_block_limit() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 2);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.attach_probe(fixture.db_path.clone(), job.id.clone());
    let ordinals = blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>();

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 2);

    assert_eq!(stats.saved, 4);
    assert_eq!(
        provider.batch_sizes(),
        vec![2, 2],
        "两发各带满「每请求文本块数」= 2 块：没有哪一发越界，也没有多发一发"
    );
    let windows = provider.windows();
    assert_eq!(windows.len(), 2, "两个并发槽位：两发请求各读一次窗口");
    for (next_ordinal, inflight) in &windows {
        assert_eq!(
            *next_ordinal, ordinals[0],
            "两个请求都在第一个块提交之前发出：窗口从头开始"
        );
        assert!(
            inflight.len() <= 4,
            "「处理中」最多是「任务并发 × 每请求文本块数」= 4 块，读到 {inflight:?}"
        );
        assert!(
            ordinals.starts_with(inflight),
            "窗口是块列表的一段前缀：发布与提交都按块序号，读到 {inflight:?}"
        );
    }
    assert_eq!(
        windows.iter().map(|(_, inflight)| inflight.len()).max(),
        Some(4),
        "第二发在四块全部进窗口之后才被轮询：窗口确实填到了上界"
    );
}
