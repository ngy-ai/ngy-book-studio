//! Opening the annotation a card points at.

use super::*;

impl NotesWindow {
    pub(super) fn open_annotation(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.opening_note.is_some() {
            return;
        }
        let Some(note) = self
            .listing
            .notes
            .iter()
            .find(|note| note.annotation.id == id)
        else {
            return;
        };
        let book_id = note.annotation.book_id.clone();
        let unit_id = note.annotation.content_unit_id.clone();
        let id = id.to_owned();
        self.opening_note = Some(id.clone());
        self.error = None;
        let task = self.services.spawn_library_read(move |library| {
            let current = library
                .list_annotations(&book_id, Some(&unit_id))?
                .into_iter()
                .find(|note| note.id == id)
                .context("这条笔记已删除，请刷新列表")?;
            anyhow::ensure!(!current.stale, "笔记位置已失效，请刷新列表查看当前状态");
            Ok(current)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.opening_note = None;
                    match outcome {
                        Ok(Ok(annotation)) => {
                            let error_view = cx.entity().downgrade();
                            this.library_view.update(cx, |library, cx| {
                                library.open_book_at_annotation(
                                    annotation,
                                    move |message, cx| {
                                        let _ = error_view.update(cx, |this, cx| {
                                            this.error = Some(message);
                                            cx.notify();
                                        });
                                    },
                                    window,
                                    cx,
                                );
                            });
                        }
                        Ok(Err(error)) => {
                            this.error = Some(format!("无法打开笔记章节：{error:#}"));
                        }
                        Err(error) => {
                            this.error = Some(format!("笔记定位任务已停止：{error}"));
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }
}
