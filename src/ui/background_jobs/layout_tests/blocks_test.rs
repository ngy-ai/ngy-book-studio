//! The translation block inspector.

use super::*;

#[test]
pub(crate) fn translation_block_inspector_pages_filters_and_marks_the_in_flight_blocks() {
    let blocks = block_list(78);
    let mut job = snapshots().remove(0);
    job.status = BackgroundJobStatus::Running;
    // 并发 3 的一次运行：游标只说「第一块没完成的」，处理中的三块由窗口集合给出。
    job.cursor_json =
        Some(serde_json::json!({ "next_ordinal": 17, "inflight": [17, 18, 19] }).to_string());
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
    job.cursor_json = Some(serde_json::json!({ "next_ordinal": 17 }).to_string());
    let unwindowed = BlockInspector::new(&job, blocks.total, None);
    assert!(unwindowed.progress.processing.is_empty());
    assert_eq!(unwindowed.progress.pending, 61);
    assert_eq!(
        block_page_indices(&blocks, &unwindowed, Some(BlockState::Processing), 0),
        Vec::<usize>::new()
    );

    // A finished task leaves no block in flight, and the page controls cannot
    // scroll past the last block.
    job.cursor_json =
        Some(serde_json::json!({ "next_ordinal": 17, "inflight": [17, 18, 19] }).to_string());
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
        log_entry(
            JobLogEvent::ProtocolSkipped,
            Some(58),
            Some(JobLogErrorKind::InvalidSchema),
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

    assert_eq!(inspector.progress.failed, 2);
    assert_eq!(inspector.state(58), BlockState::Failed);
    assert_eq!(inspector.state(61), BlockState::Failed);
    assert_eq!(inspector.reason(58), Some("响应结构不符合分段协议"));
    assert_eq!(inspector.reason(61), Some("片段数量与原文不一致"));
    // A rejected attempt that was retried into a valid answer is not a failure.
    assert_eq!(inspector.reason(60), None);
    assert_eq!(inspector.state(60), BlockState::Pending);
    assert_eq!(inspector.reason(1), None);
    assert_eq!(inspector.state(1), BlockState::Done);

    // The failed blocks leave the state the cursor gave them, so the header
    // still adds up to the number of blocks.
    let progress = &inspector.progress;
    assert_eq!(progress.cursor, 60);
    assert_eq!(progress.done, 59);
    assert_eq!(
        progress.done + progress.processing.len() + progress.failed + progress.pending,
        progress.total
    );

    // They can be filtered and jumped to on their own.
    assert_eq!(
        block_page_indices(&blocks, &inspector, Some(BlockState::Failed), 0),
        vec![58, 61]
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
        Some("本次执行未翻译的文本块：#58、#61")
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
    // 由游标决定，处理中由游标里的窗口集合决定（单块在飞时就是游标那一块）。
    job.cursor_json = Some(serde_json::json!({ "next_ordinal": 12, "inflight": [12] }).to_string());
    let bare = BlockInspector::new(&job, blocks.total, None);
    assert_eq!(bare.progress.failed, 0);
    assert_eq!(bare.state(11), BlockState::Done);
    assert_eq!(bare.state(12), BlockState::Processing);
    assert_eq!(bare.state(13), BlockState::Pending);
}
