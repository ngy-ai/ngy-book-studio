//! Exporting a share package, and the user-visible summaries of a share import.
//!
//! Both directions run on a background executor: a package can hold a whole
//! book plus every note and译文 of it, and neither reading nor writing one may
//! block the library window.

use super::*;

/// What a finished library import produced; the routing in `import.rs` turns
/// either shape into the same completion notice.
pub(crate) enum LibraryImportResult {
    Book(ImportOutcome),
    Share(Box<ShareImportOutcome>),
}

impl EpubReaderApp {
    /// Writes one book, its notes, its conversations and its译文 into a package.
    pub(super) fn export_share_package(
        &mut self,
        book_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        let selection = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出《{}》的分享包", book.title))
            .set_filename(suggested_share_filename(&book.title))
            .add_filter("墨页分享包", [ngy_book_studio::share::SHARE_EXTENSION])
            .save_single_file();
        let title = book.title;
        let library = self.library.clone();
        // As with the other native dialogs, `show` must run outside the entity
        // update that created it so the nested Windows message loop can draw.
        cx.spawn_in(window, async move |view, cx| {
            let target = match selection.show() {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.set_error(format!("无法打开保存文件选择器：{error}"), cx);
                    });
                    return;
                }
            };
            let target_display = target.display().to_string();
            let _ = view.update_in(cx, |this, _window, cx| {
                this.notice = Some(Notice {
                    text: format!("正在打包《{title}》…"),
                    error: false,
                });
                cx.notify();
            });
            let task =
                cx.background_spawn(async move { library.export_share_package(&book_id, &target) });
            let outcome = task.await;
            let _ = view.update_in(cx, |this, _window, cx| {
                match outcome {
                    Ok(summary) => {
                        this.notice = Some(Notice {
                            text: share_export_message(&title, &target_display, &summary),
                            error: false,
                        });
                    }
                    Err(error) => {
                        this.set_error(format!("导出《{title}》分享包失败：{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// Builds the notice for a finished export, naming what actually travelled.
pub(crate) fn share_export_message(
    title: &str,
    target: &str,
    summary: &ShareExportSummary,
) -> String {
    let mut parts = vec![format!("《{title}》分享包已导出到「{target}」")];
    let shared = shared_items(summary.notes, summary.chat_messages, summary.translations);
    match shared {
        Some(shared) => parts.push(format!("包含{shared}")),
        None => parts.push("其中没有笔记、AI 问答或译文".to_string()),
    }
    parts.join("，")
}

/// Builds the notice for a finished import, including what could not be
/// re-anchored: silently dropping shared notes would look like data loss.
pub(crate) fn share_import_message(outcome: &ShareImportOutcome) -> String {
    let title = &outcome.record.title;
    let mut parts = vec![if outcome.added {
        format!("《{title}》已加入图书库")
    } else {
        format!("《{title}》已经在图书库中，已补充分享内容")
    }];
    match shared_items(
        outcome.notes_imported,
        outcome.chat_messages,
        outcome.translations_imported,
    ) {
        Some(shared) => parts.push(format!("导入{shared}")),
        None => parts.push("这个分享包没有笔记、AI 问答或译文".to_string()),
    }
    let skipped = outcome.notes_skipped + outcome.translations_skipped;
    if skipped > 0 {
        parts.push(format!("另有 {skipped} 条与本书正文不再匹配，已跳过"));
    }
    parts.join("，")
}

fn shared_items(notes: usize, messages: usize, translations: usize) -> Option<String> {
    let mut items = Vec::new();
    if notes > 0 {
        items.push(format!("{notes} 条笔记"));
    }
    if messages > 0 {
        items.push(format!("{messages} 段 AI 问答"));
    }
    if translations > 0 {
        items.push(format!("{translations} 条译文"));
    }
    (!items.is_empty()).then(|| items.join("、"))
}

fn suggested_share_filename(title: &str) -> String {
    let epub = suggested_epub_filename(title);
    let stem = epub.strip_suffix(".epub").unwrap_or("book");
    format!("{stem}.{}", ngy_book_studio::share::SHARE_EXTENSION)
}
