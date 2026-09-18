//! Persisting progress and applying the resulting projections.

use super::*;

impl PdfReaderApp {
    pub(crate) fn persist_progress(&mut self, cx: &mut Context<Self>) {
        if !self.persist_progress_enabled {
            return;
        }
        let progress = match pdf_progress_write(self.current_page, self.current_page_info()) {
            Ok(progress) => progress,
            Err(error) => {
                tracing::warn!(%error, "cannot resolve canonical PDF reading progress");
                if !self.closing {
                    self.notice = Some(Notice {
                        text: format!("PDF 阅读进度保存失败：{error}"),
                        error: true,
                    });
                    cx.notify();
                }
                return;
            }
        };
        let Some(writer) = self.progress_writer.as_ref() else {
            return;
        };
        if let Err(error) = writer.enqueue(progress) {
            tracing::warn!(%error, "failed to queue PDF reading progress");
            if !self.closing {
                self.notice = Some(Notice {
                    text: format!("PDF 阅读进度保存失败：{error}"),
                    error: true,
                });
                cx.notify();
            }
        }
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
        let Ok(current) = pdf_progress_write(self.current_page, self.current_page_info()) else {
            return;
        };
        if self.closing || event.progress != current {
            return;
        }
        self.notice = Some(Notice {
            text: format!("PDF 阅读进度保存失败：{error}"),
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
}
