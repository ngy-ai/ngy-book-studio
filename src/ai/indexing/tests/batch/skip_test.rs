use super::*;

/// 一发批请求自己失败（传输层）：这一批的每一块都按请求级失败跳过，运行照常走完。
///
/// 与「坏批拆半」分开：连接类失败拆半只会对同一个坏端点多发几发请求，所以整批一起认输，
/// 连续失败由既有的冷却兜住。
#[test]
pub(crate) fn a_failed_batch_request_skips_its_blocks_without_splitting() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.fail_requests.store(1, Ordering::SeqCst);

    let (outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4],
        "整批认输：不再拆半，也不补发"
    );
    assert!(stored_translations(&fixture).is_empty(), "一块都没有译文");
    assert_eq!(stats.untranslated, 4, "这一批的四块都算跳过");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("个别块的请求失败不该让整次运行失败");
    };
    assert_eq!(finished.next_ordinal, blocks.len(), "游标照常走到书末");
    // 每一块都在任务日志里留下自己的失败读数（块级明细靠它们）。
    let skipped = recorded_events(&fixture, &job.id)
        .into_iter()
        .filter(|entry| entry.event == JobLogEvent::ProviderSkipped)
        .count();
    assert_eq!(skipped, 4);
}

/// 一批只装同一个内容单元（同一章）的块：跨过章边界就必须另起一发。
///
/// `book`/`chapter` 是每批一份的语境字段，混进另一章的块会让这两个字段撒谎 —— 所以这条
/// 用例断言第二发请求的 `chapter` 就是第二章自己的标题。
#[test]
pub(crate) fn a_batch_never_mixes_two_chapters() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    fixture.rewrite_first_unit("<p>alpha</p><p>beta</p>");
    fixture.add_second_unit("第二章");
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "第一章的章标题与两段正文 + 第二章自己的块");
    assert_ne!(blocks[0].unit_id, blocks[3].unit_id);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![3, 1],
        "章边界把一批拆成两发，即使「每请求文本块数」还装得下"
    );
    let batches = provider
        .batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert_eq!(
        batches[1].0, "第二章",
        "第二发的语境字段必须是第二章自己的标题"
    );
    assert_ne!(batches[0].0, "第二章", "第一发不许带上第二章的标题");
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4);
}
