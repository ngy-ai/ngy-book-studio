//! The translation block inspector.

use super::*;

#[test]
pub(crate) fn translation_block_inspector_pages_filters_and_marks_the_in_flight_blocks() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    // 并发 3 的一次运行：游标只说「第一块没完成的」，处理中的三块由窗口集合给出。
    // 窗口来自服务层投影（运行中的翻译任务不下发原始游标），所以这里只给投影，
    // `cursor_json` 保持为空 —— 现场 2026-09-20：界面回去读那份永远缺失的游标，
    // 于是「已处理」一直在涨而「处理中」恒为 0。
    job.cursor_json = None;
    job.progress.processing = vec![17, 18, 19];
    let inspector = BlockInspector::new(&job, blocks.total, None);
    let progress = &inspector.progress;
    assert_eq!(
        (progress.done, progress.processing.clone(), progress.pending),
        (17, vec![17, 18, 19], 58)
    );
    assert_eq!(inspector.state(16), BlockState::Done);
    assert_eq!(inspector.state(17), BlockState::Processing);
    assert_eq!(inspector.state(19), BlockState::Processing);
    assert_eq!(inspector.state(20), BlockState::Pending);

    let first = block_page_indices(&blocks, &inspector, None, 0);
    assert_eq!(first, (0..BLOCKS_PER_PAGE).collect::<Vec<_>>());
    let second = block_page_indices(&blocks, &inspector, None, 1);
    assert_eq!(second.first(), Some(&50));
    assert_eq!(second.last(), Some(&77));

    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Done), 0),
        (0..17).collect::<Vec<_>>()
    );
    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Processing), 0),
        vec![17, 18, 19]
    );
    let pending = block_page_indices(&blocks, &inspector, Some(BlockState::Pending), 0);
    assert_eq!(pending.first(), Some(&20));
    assert_eq!(pending.len(), BLOCKS_PER_PAGE);

    // A task whose row says running but whose cursor carries no window (an older
    // format, or a process that died between two rounds) claims nothing is in
    // flight instead of inventing one block at the cursor.
    job.progress.processing = Vec::new();
    let unwindowed = BlockInspector::new(&job, blocks.total, None);
    assert!(unwindowed.progress.processing.is_empty());
    assert_eq!(unwindowed.progress.pending, 61);
    assert_eq!(
        block_page_indices(&blocks, &unwindowed, Some(BlockState::Processing), 0),
        Vec::<usize>::new()
    );

    // A finished task leaves no block in flight, and the page controls cannot
    // scroll past the last block.
    job.progress.processing = vec![17, 18, 19];
    job.progress.completed = blocks.total;
    let finished = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(
        (
            finished.progress.done,
            finished.progress.processing.clone(),
            finished.progress.pending
        ),
        (78, Vec::new(), 0)
    );
    assert!(block_page_indices(&blocks, &finished, Some(BlockState::Processing), 0).is_empty());
    // Past the last page there is nothing left to show.
    assert!(block_page_indices(&blocks, &finished, None, 2).is_empty());

    // A stale snapshot that overran the current block list stays consistent.
    job.progress.completed = 200;
    let stale = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(
        (
            stale.progress.done,
            stale.progress.processing.clone(),
            stale.progress.pending
        ),
        (78, Vec::new(), 0)
    );
}

/// 「立即翻译本章」把推进点借给一章：计数必须按借出点算「已处理」，不能拿停在
/// 那一章第一块的游标当边界。
///
/// 现场 2026-10-18：整本 7974 个文本块早已翻完，点「立即翻译本章」后任务窗口显示
/// 「已处理 168 · 处理中 43 · 待处理 7763」—— 游标被借到了这一章的第一块，于是整本
/// 剩下的块都被当成没翻过；实际只翻译了这一章，任务随即结束。
#[test]
pub(crate) fn a_chapter_jump_counts_the_blocks_outside_its_range_by_the_borrowed_position() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    job.cursor_json = None;
    // 整本翻完之后跳到 #40 这一章：借出点是 78，收窄到 40..=42，41 正在发请求。
    job.progress.completed = 40;
    job.progress.scope = vec![40, 41, 42];
    job.progress.resume = Some(78);
    job.progress.processing = vec![41];

    let inspector = BlockInspector::new(&job, blocks.total, None);
    let progress = &inspector.progress;
    assert_eq!(
        (
            progress.done,
            progress.processing.clone(),
            progress.failed,
            progress.pending
        ),
        (75, vec![41], 0, 2),
        "范围之外的块按借出点算已处理，只有这一章没翻完的两块在等"
    );
    assert_eq!(
        progress.done + progress.processing.len() + progress.failed + progress.pending,
        progress.total
    );
    assert_eq!(inspector.state(39), BlockState::Done);
    assert_eq!(inspector.state(40), BlockState::Pending);
    assert_eq!(inspector.state(41), BlockState::Processing);
    assert_eq!(inspector.state(43), BlockState::Done);
    // 说明行与复制出来的诊断都要讲清楚这次只有一章在跑，以及跑完回到哪一块。
    let note = scope_note(&job, progress).expect("收窄的运行必须给出说明");
    assert!(
        note.contains("3 个文本块") && note.contains("#78"),
        "{note}"
    );
    assert!(
        translation_blocks_copy(&job, &blocks, &inspector).contains("#78"),
        "复制文本必须带上收窄说明"
    );
    assert!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Pending), 0) == vec![40, 42],
        "待处理筛选只应列出这一章没翻完的块"
    );

    // 向前跳（借出点 17、跳到 #60）：游标直接跳到这一章，借出点与那一章之间的块
    // 并没有被处理过，不能因为游标越过了它们就记成已处理。
    job.progress.completed = 60;
    job.progress.scope = vec![60, 61, 62];
    job.progress.resume = Some(17);
    job.progress.processing = Vec::new();
    let forward = BlockInspector::new(&job, blocks.total, None);
    assert_eq!((forward.progress.done, forward.progress.pending), (17, 61));
    assert_eq!(forward.state(16), BlockState::Done);
    assert_eq!(forward.state(30), BlockState::Pending);
    assert_eq!(forward.state(59), BlockState::Pending);
    assert_eq!(forward.state(60), BlockState::Pending);

    // 收窄结束、推进点还回来之后又回到普通口径。
    job.progress.completed = 78;
    job.progress.scope = Vec::new();
    job.progress.resume = None;
    let returned = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(returned.progress.done, 78);
    assert_eq!(returned.progress.pending, 0);
    assert!(scope_note(&job, &returned.progress).is_none());
}

/// The durable cursor alone can only say how far a failed task got: the blocks
/// it skipped are named by the per-block diagnostics, and those are what the
/// window has to show.
#[test]
pub(crate) fn translation_block_inspector_names_the_blocks_that_failed() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Failed;
    job.progress.completed = 60;
    let logs = log_snapshot(vec![
        log_entry(JobLogEvent::RunStarted, None, None),
        log_entry(JobLogEvent::ItemSaved, Some(1), None),
        // 「模型把原文原样抄回来」不算失败块：它只说明库里那一行不算译文、walk 会重发
        // 这一块，紧接着那次请求要么翻好它、要么按下面两条跳过事件进列表。把它算成失败
        // 会让刚翻好的块一直挂在「失败」上（`docs/agent/code-map-ui.md`）。
        log_entry(JobLogEvent::SourceEchoed, Some(5), None),
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(58),
            Some(JobLogErrorKind::InvalidSchema),
        ),
        // 请求级失败（端点/模型不可用）与协议跳过一样是「这一块没有译文」：它必须同
        // 样出现在失败列表里，否则用户看不到哪一块该重试。
        log_entry(
            JobLogEvent::ProviderSkipped,
            Some(59),
            Some(JobLogErrorKind::Provider),
        ),
        log_entry(
            JobLogEvent::ProtocolRejected,
            Some(61),
            Some(JobLogErrorKind::SegmentCountMismatch),
        ),
        log_entry(
            JobLogEvent::StepFailed,
            Some(61),
            Some(JobLogErrorKind::SegmentCountMismatch),
        ),
        // The headline failure carries the run position, not a block.
        log_entry(
            JobLogEvent::RunFailed,
            Some(60),
            Some(JobLogErrorKind::Timeout),
        ),
    ]);
    let inspector = BlockInspector::new(&job, blocks.total, Some(&logs));

    assert_eq!(inspector.progress.failed, 3);
    assert_eq!(inspector.state(58), BlockState::Failed);
    assert_eq!(inspector.state(59), BlockState::Failed);
    assert_eq!(inspector.state(61), BlockState::Failed);
    assert_eq!(inspector.reason(58), Some("响应结构不符合分段协议"));
    assert_eq!(inspector.reason(59), Some("模型服务返回错误"));
    assert_eq!(inspector.reason(61), Some("片段数量与原文不一致"));
    // A rejected attempt that was retried into a valid answer is not a failure.
    assert_eq!(inspector.reason(60), None);
    assert_eq!(inspector.state(60), BlockState::Pending);
    assert_eq!(inspector.reason(1), None);
    assert_eq!(inspector.state(1), BlockState::Done);
    // 「原样抄回来」那一块同样不是失败块：它没有译文这件事由紧接着那次重发的结果决定。
    assert_eq!(inspector.reason(5), None);
    assert_eq!(inspector.state(5), BlockState::Done);

    // The failed blocks leave the state the cursor gave them, so the header
    // still adds up to the number of blocks.
    let progress = &inspector.progress;
    assert_eq!(progress.cursor, 60);
    assert_eq!(progress.done, 58);
    assert_eq!(
        progress.done + progress.processing.len() + progress.failed + progress.pending,
        progress.total
    );

    // They can be filtered and jumped to on their own.
    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Failed), 0),
        vec![58, 59, 61]
    );
    assert_eq!(
        anchor_page(&blocks, &inspector, Some(BlockState::Failed)),
        0
    );
    assert_eq!(
        anchor_page(&blocks, &inspector, Some(BlockState::Pending)),
        0
    );
    assert_eq!(
        untranslated_summary(&inspector).as_deref(),
        Some("本次执行未翻译的文本块：#58、#59、#61")
    );
    // A failed task must explain itself in the inspector, not only in the log.
    assert!(failure_banner(&job, &inspector, Palette::LIGHT).is_some());
}

#[test]
pub(crate) fn translation_block_inspector_reports_every_untranslated_block_in_the_banner() {
    let blocks = block_list(200);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Failed;
    job.progress.completed = 0;
    let mut entries = vec![log_entry(JobLogEvent::RunStarted, None, None)];
    entries.extend((0..30).map(|ordinal| {
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(ordinal),
            Some(JobLogErrorKind::InvalidSchema),
        )
    }));
    let logs = log_snapshot(entries);
    let inspector = BlockInspector::new(&job, blocks.total, Some(&logs));
    assert_eq!(inspector.progress.failed, 30);
    let summary = untranslated_summary(&inspector).expect("失败块必须能被列出来");
    assert!(summary.contains("#0、#1"), "{summary}");
    assert!(summary.contains("共 30 个"), "{summary}");
    assert!(
        !summary.contains("#12"),
        "清单必须有上限，否则横幅会变成数字墙：{summary}"
    );
}

/// A retry must not inherit the failures of the attempt before it, and a
/// controlled stop is not a block failure.
#[test]
pub(crate) fn translation_block_inspector_scopes_failures_to_the_newest_execution() {
    let blocks = block_list(20);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    job.progress.completed = 12;
    let logs = log_snapshot(vec![
        log_entry(JobLogEvent::RunStarted, None, None),
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(3),
            Some(JobLogErrorKind::InvalidSchema),
        ),
        log_entry(JobLogEvent::RunFailed, Some(3), None),
        // A new execution translated what the previous one skipped.
        log_entry(JobLogEvent::RunStarted, None, None),
        log_entry(JobLogEvent::ItemSaved, Some(3), None),
        log_entry(
            JobLogEvent::StepFailed,
            Some(11),
            Some(JobLogErrorKind::Cancelled),
        ),
    ]);
    let inspector = BlockInspector::new(&job, blocks.total, Some(&logs));
    assert_eq!(inspector.progress.failed, 0);
    assert_eq!(inspector.state(3), BlockState::Done);
    assert!(failure_banner(&job, &inspector, Palette::LIGHT).is_none());

    // Without any log at all the inspector still renders from the cursor: 已处理与待处理
    // 由游标决定，处理中由快照投影的窗口集合决定（单块在飞时就是游标那一块）。
    job.progress.processing = vec![12];
    let bare = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(bare.progress.failed, 0);
    assert_eq!(bare.state(11), BlockState::Done);
    assert_eq!(bare.state(12), BlockState::Processing);
    assert_eq!(bare.state(13), BlockState::Pending);
}
