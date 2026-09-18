//! The status line shown in the editor header.

use super::*;

/// Picks the icon, label and colour that describe what the editor is doing.
pub(crate) fn editor_status(
    app: &EditorApp,
    media_modal_open: bool,
    cx: &App,
) -> (IconName, &'static str, u32) {
    let theme = palette(cx);
    if app.closing || app.webview_build_gate.close_requested {
        (IconName::BookOpen, "正在安全关闭编辑器…", theme.accent)
    } else if let Some(operation) = app.active_write.as_ref() {
        match operation.intent {
            EditorWriteIntent::Save => (IconName::BookOpen, "正在后台保存…", theme.accent),
            EditorWriteIntent::Export => (IconName::BookOpen, "正在后台保存并导出…", theme.accent),
            EditorWriteIntent::Close => (IconName::BookOpen, "正在后台保存并关闭…", theme.accent),
        }
    } else if app.export_dialog_open {
        (IconName::ExternalLink, "正在选择导出位置…", theme.accent)
    } else if app.cover_loading {
        (IconName::BookOpen, "正在读取封面图片…", theme.accent)
    } else if app.media_loading {
        (IconName::BookOpen, "正在读取媒体文件…", theme.accent)
    } else if media_modal_open {
        (IconName::Settings2, "正在编辑媒体信息…", theme.accent)
    } else if let Some(pending) = app.pending_snapshot.as_ref() {
        match pending.action {
            PendingEditorAction::Save => (IconName::BookOpen, "正在同步并保存…", theme.accent),
            PendingEditorAction::Export => (IconName::BookOpen, "正在同步并导出…", theme.accent),
            PendingEditorAction::Close => (IconName::BookOpen, "正在保存并关闭…", theme.accent),
            _ => (IconName::BookOpen, "正在同步富文本…", theme.accent),
        }
    } else if app.pending_ready_action.is_some() {
        (IconName::BookOpen, "等待富文本视图就绪…", theme.accent)
    } else if app.webview_build_gate.building {
        (IconName::BookOpen, "正在初始化预览组件…", theme.accent)
    } else if app.tab.uses_webview() && app.editor_webview.is_none() {
        (IconName::TriangleAlert, "预览组件不可用", theme.danger)
    } else if app.tab == EditorTab::RichText
        && !app.active_web_page.as_ref().is_some_and(|page| page.ready)
    {
        (IconName::BookOpen, "富文本视图正在加载…", theme.accent)
    } else {
        (IconName::CircleCheck, "编辑器已就绪", 0x376441)
    }
}

/// The one-line summary under the chapter title, describing the current chapter.
pub(crate) fn editor_summary_text(app: &EditorApp, chapter_title: &str) -> String {
    if app.chapters.get(app.selected).is_some() {
        format!(
            "第 {} / {} 章 · {}",
            app.selected + 1,
            app.chapters.len(),
            if chapter_title.is_empty() {
                "未命名章节"
            } else {
                chapter_title
            }
        )
    } else {
        "暂无章节".to_string()
    }
}

/// The label describing the current editing surface.
pub(crate) fn editor_mode_label(tab: EditorTab) -> &'static str {
    match tab {
        EditorTab::Source => "HTML 源码编辑",
        EditorTab::Preview => "只读预览",
        EditorTab::RichText => "富文本 · 所见即所得",
    }
}

/// The context line shown next to the mode, including the search result count.
pub(crate) fn editor_context_label(
    mode: &str,
    search_query: &str,
    search_result_count: usize,
) -> String {
    if search_query.is_empty() {
        format!("{mode} · 手动保存")
    } else {
        format!("{mode} · 检索 {search_result_count} 个结果 · 手动保存")
    }
}
