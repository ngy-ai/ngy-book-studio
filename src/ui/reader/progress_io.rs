//! Queuing progress writes and applying the resulting projections.

use super::*;

impl ReaderApp {
    pub(crate) fn queue_progress(
        &mut self,
        progress: ReadingProgressWrite,
        cx: &mut Context<Self>,
    ) {
        let Some(writer) = self.progress_writer.as_ref() else {
            return;
        };
        if let Err(error) = writer.enqueue(progress) {
            tracing::warn!(%error, "failed to queue reading progress");
            if !self.closing {
                self.notice = Some(Notice {
                    text: format!("阅读进度保存失败：{error}"),
                    error: true,
                });
                cx.notify();
            }
        }
    }

    pub(crate) fn current_progress_write(&self) -> Option<ReadingProgressWrite> {
        reading_progress_for_locator(
            self.current_spine,
            &self.progress_locators,
            self.current_anchor.clone(),
        )
    }

    pub(crate) fn queue_current_progress(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = self.current_progress_write() else {
            self.notice = Some(Notice {
                text: "当前章节缺少稳定阅读位置，已停止保存进度。".to_string(),
                error: true,
            });
            cx.notify();
            return;
        };
        self.queue_progress(progress, cx);
    }

    pub(crate) fn handle_progress_write_event(
        &mut self,
        event: ReadingProgressWriteEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_progress_projection(event.projection, cx);
        let Some(error) = event.error else {
            return;
        };
        if self.closing || self.current_progress_write().as_ref() != Some(&event.progress) {
            return;
        }
        self.notice = Some(Notice {
            text: format!("阅读进度保存失败：{error}"),
            error: true,
        });
        cx.notify();
    }

    pub(crate) fn apply_progress_projection(
        &mut self,
        projection: Option<ReadingProgressProjection>,
        cx: &mut Context<Self>,
    ) {
        let Some((generation, snapshot)) = projection else {
            return;
        };
        self.library.merge_cached_projection(snapshot.clone());
        self.library_view.update(cx, |library, cx| {
            library.refresh_after_projected_mutation(generation, snapshot, cx);
        });
    }

    pub(crate) fn sync_ai_reference(&mut self, cx: &mut Context<Self>) {
        let references = reader_reference_hints(
            &self.book_id,
            &self.book.spine,
            &self.progress_locators,
            self.current_spine,
            self.selected_text.as_deref(),
        );
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }

    pub(crate) fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice { text, error: true });
        cx.notify();
    }
}
