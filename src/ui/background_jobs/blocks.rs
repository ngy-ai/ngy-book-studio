//! Block paging, chapter labels and the copy/detail rows.

use super::*;

/// Indices of the blocks on one page of the inspector. The scan stops as soon
/// as the page is filled, so a large book never materializes every match.
pub(crate) fn block_page_indices(
    blocks: &TranslationBlockList,
    inspector: &BlockInspector,
    filter: Option<BlockState>,
    page: usize,
) -> Vec<usize> {
    let start = page.saturating_mul(BLOCKS_PER_PAGE);
    let end = start.saturating_add(BLOCKS_PER_PAGE);
    let mut matched = 0usize;
    let mut indices = Vec::new();
    for index in 0..blocks.total {
        if filter.is_some_and(|filter| filter != inspector.state(index)) {
            continue;
        }
        if matched >= start {
            indices.push(index);
        }
        matched += 1;
        if matched >= end {
            break;
        }
    }
    indices
}

/// Page holding the block at the durable cursor inside one filtered view. The
/// inspector pages the filtered list, so "跳到当前进度" has to map the cursor's
/// ordinal into that view instead of assuming the unfiltered ordering.
pub(crate) fn anchor_page(
    blocks: &TranslationBlockList,
    inspector: &BlockInspector,
    filter: Option<BlockState>,
) -> usize {
    let Some(last) = blocks.total.checked_sub(1) else {
        return 0;
    };
    let target = inspector.progress.cursor.min(last);
    let position = (0..target)
        .filter(|index| filter.is_none_or(|state| state == inspector.state(*index)))
        .count();
    position / BLOCKS_PER_PAGE
}

pub(crate) fn block_chapter_label(block: &TranslationBlockInfo) -> String {
    if block.unit_title.trim().is_empty() {
        format!("第 {} 章", block.unit_ordinal + 1)
    } else {
        block.unit_title.clone()
    }
}

/// 收窄运行的说明：表头为什么不是整本书的进度。
///
/// 「立即翻译本章」把推进点借给这一章，按块重试把范围收窄成一组块；两种情况下
/// 游标都不再是「已处理前缀」的边界，计数必须说明自己只覆盖哪一段。
pub(crate) fn scope_note(
    job: &BackgroundJobSnapshot,
    progress: &TranslationProgress,
) -> Option<String> {
    if progress.scope.is_empty() {
        return None;
    }
    let blocks = progress.scope.len();
    Some(match job.progress.resume {
        Some(resume) => format!(
            "本次运行只处理「立即翻译本章」收窄的 {blocks} 个文本块，跑完回到 #{resume} 继续整本；\
             「已处理」按借出推进点之前的进度计算。"
        ),
        None => format!(
            "本次运行只处理收窄的 {blocks} 个文本块（重试失败块或阅读端重翻），\
             范围之外的块不归这次运行。"
        ),
    })
}

pub(crate) fn translation_blocks_copy(
    job: &BackgroundJobSnapshot,
    blocks: &TranslationBlockList,
    inspector: &BlockInspector,
) -> String {
    let progress = &inspector.progress;
    let mut lines = vec![
        format!("任务 ID: {}", job.id),
        format!("图书 ID: {}", job.book_id),
        format!(
            "目标语言: {}",
            translation_language_label(&blocks.target_language)
                .unwrap_or(blocks.target_language.as_str())
        ),
        format!(
            "文本块: 共 {} · 已处理 {} · 处理中 {} · 失败 {} · 待处理 {}",
            progress.total,
            progress.done,
            progress.processing.len(),
            progress.failed,
            progress.pending
        ),
        "序号与任务游标一致，从 0 开始。".to_string(),
    ];
    if let Some(note) = scope_note(job, progress) {
        lines.push(note);
    }
    if progress.failed > 0 {
        let ordinals = inspector
            .failures
            .reasons
            .iter()
            .map(|(ordinal, kind)| match kind {
                Some(kind) => format!("#{ordinal} {}", kind.label()),
                None => format!("#{ordinal} 未记录分类"),
            })
            .collect::<Vec<_>>()
            .join("、");
        lines.push(format!("本次执行未翻译的文本块: {ordinals}"));
        if inspector.failures.truncated {
            lines.push("诊断记录已按保留上限清理，上面的列表可能不完整。".into());
        }
    }
    if let Some(error) = &job.error {
        lines.push(format!("失败原因: {error}"));
    }
    lines.join("\n")
}

pub(crate) fn detail_rows(job: &BackgroundJobSnapshot) -> Vec<(&'static str, String)> {
    vec![
        ("任务 ID", job.id.clone()),
        ("图书 ID", job.book_id.clone()),
        ("来源 ID", job.source_id.clone().unwrap_or_else(not_set)),
        ("任务类型", job_kind_label(&job.kind).into()),
        (
            "状态 / 尝试",
            format!(
                "{} / {} 次",
                status_database_label(job.status),
                job.attempts
            ),
        ),
        ("处理进度", progress_label(job)),
        ("创建时间 UTC", format_timestamp(job.created_at)),
        ("更新时间 UTC", format_timestamp(job.updated_at)),
        (
            "开始时间 UTC",
            job.started_at.map(format_timestamp).unwrap_or_else(not_set),
        ),
        (
            "结束时间 UTC",
            job.finished_at
                .map(format_timestamp)
                .unwrap_or_else(not_set),
        ),
        (
            "控制请求",
            if job.cancel_requested {
                "正在取消"
            } else if job.pause_requested {
                "正在暂停"
            } else {
                "无"
            }
            .into(),
        ),
    ]
}

pub(crate) fn diagnostic_log_copy(
    job: &BackgroundJobSnapshot,
    logs: &BackgroundJobLogSnapshot,
) -> String {
    let summary = detail_rows(job)
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = logs
        .entries
        .iter()
        .map(|entry| entry.format_line())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{summary}\n\n最近保留的日志：{} 条；较早记录{}清理；时间正序，ordinal 从 0 开始。\n{lines}",
        logs.entries.len(),
        if logs.truncated {
            "已"
        } else {
            "可能按全局上限"
        }
    )
}
