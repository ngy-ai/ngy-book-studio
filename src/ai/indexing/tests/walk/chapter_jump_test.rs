use super::*;

/// 「任务间隔」按请求计，不按任务计：每个并发槽位在自己的模型请求收尾后先休眠这段
/// 间隔，然后才去接下一块。
///
/// 现场（2026-09-20）：间隔原本只作用于两个任务之间，而一整本书就是一个任务 ——
/// 本机模型（Ollama）因此可以毫不停顿地连续请求，把 GPU 吃到满载、桌面黑屏，而这个
/// 设置对整本书的翻译形同虚设。
#[test]
pub(crate) fn the_configured_interval_pauses_after_each_model_request() {
    let interval = Duration::from_millis(200);
    let provider = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&provider, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, interval);
    let mut stats = TranslationRunStats::default();

    fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        ))
        .expect("带间隔的串行翻译同样要能翻完");

    let starts = provider.request_starts();
    assert!(
        starts.len() >= 2,
        "间隔用例需要至少两次模型请求，实际 {}",
        starts.len()
    );
    let tolerance = Duration::from_millis(20);
    for pair in starts.windows(2) {
        let gap = pair[1].saturating_duration_since(pair[0]);
        assert!(
            gap + tolerance >= interval,
            "两次模型请求之间必须休眠任务间隔：实测 {gap:?} < {interval:?}"
        );
    }
}
