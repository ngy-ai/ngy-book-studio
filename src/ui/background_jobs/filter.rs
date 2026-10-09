//! Matching jobs against the kind filter and the failure banner.

use super::*;

pub(crate) fn kind_matches(kind: &str, filter: &str) -> bool {
    match filter {
        "" => true,
        "other" => !matches!(
            kind,
            "translation" | "embedding" | "vision" | "visual_render"
        ),
        _ => kind == filter,
    }
}

pub(crate) fn job_matches(
    job: &BackgroundJobSnapshot,
    books: &[BackgroundJobBook],
    kind: &str,
    status: Option<BackgroundJobStatus>,
    query: &str,
) -> bool {
    kind_matches(&job.kind, kind)
        && status.is_none_or(|status| job.status == status)
        && (query.is_empty()
            || job.id.to_lowercase().contains(query)
            || books
                .iter()
                .any(|book| book.id == job.book_id && book.title.to_lowercase().contains(query)))
}

/// Banner above the block list. A failed task used to show nothing at all about
/// why it stopped, so this carries the persisted reason and names the blocks the
/// newest execution left untranslated. `None` when there is nothing to report.
pub(crate) fn failure_banner(
    job: &BackgroundJobSnapshot,
    inspector: &BlockInspector,
    theme: Palette,
) -> Option<gpui_kit::AnyElement> {
    let failed = inspector.progress.failed;
    let failed_task = job.status == BackgroundJobStatus::Failed;
    if !failed_task && failed == 0 {
        return None;
    }
    let mut content = div().v_flex().gap_1();
    if failed_task {
        content = content.child(format!(
            "任务失败：{}",
            job.error.as_deref().unwrap_or("未记录失败原因")
        ));
    } else {
        content = content.child(format!(
            "本次执行有 {failed} 个文本块没有译文，已保留原文；可筛选“失败”查看。"
        ));
    }
    if let Some(summary) = untranslated_summary(inspector) {
        content = content.child(summary);
    }
    if inspector.failures.truncated {
        content = content.child("诊断记录已按保留上限清理，上面的清单可能不完整。");
    }
    let (foreground, background) = if failed_task {
        (rgb(theme.danger), rgb(theme.danger_soft))
    } else {
        (rgb(theme.accent_strong), rgb(theme.accent_soft))
    };
    Some(
        div()
            .p_2()
            .rounded(px(6.))
            .bg(background)
            .text_color(foreground)
            .child(content)
            .into_any_element(),
    )
}

/// Which blocks the newest execution left untranslated. The list is bounded so
/// a book where every block failed cannot turn the banner into a wall of
/// numbers; the "失败" filter shows the rest.
pub(crate) fn untranslated_summary(inspector: &BlockInspector) -> Option<String> {
    const LISTED: usize = 12;
    let ordinals = inspector
        .failures
        .reasons
        .keys()
        .filter(|ordinal| **ordinal < inspector.progress.total)
        .collect::<Vec<_>>();
    if ordinals.is_empty() {
        return None;
    }
    let listed = ordinals
        .iter()
        .take(LISTED)
        .map(|ordinal| format!("#{ordinal}"))
        .collect::<Vec<_>>()
        .join("、");
    Some(if ordinals.len() > LISTED {
        format!(
            "本次执行未翻译的文本块：{listed} … 共 {} 个",
            ordinals.len()
        )
    } else {
        format!("本次执行未翻译的文本块：{listed}")
    })
}

pub(crate) fn tab_id(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Summary => "summary",
        DetailTab::Logs => "logs",
        DetailTab::Blocks => "blocks",
    }
}
