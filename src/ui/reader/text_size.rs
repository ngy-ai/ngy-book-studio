//! Reading appearance and the debounced text-size save.

use super::*;

impl ReaderApp {
    /// Re-applies this book's text size and the current skin to the chapter on
    /// screen.
    ///
    /// Both live on the same root element, so one script carries both: applying
    /// only the size would leave a dark skin's chapter on the light stylesheet,
    /// and applying only the skin would put the size back on its baseline.
    pub(crate) fn apply_reading_appearance(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let script = reader_appearance_script(self.text_size, current_appearance(cx));
        self.run_reader_script(
            script,
            // Presentation only: a missed update must not become a reader error.
            |_, error, _| tracing::warn!(%error, "cannot apply the reading appearance"),
            cx,
        );
    }

    /// Reads this book's remembered text size once per window. Until it arrives
    /// the window shows the default, so an unreadable row keeps that default
    /// rather than blocking the chapter.
    pub(crate) fn load_text_size(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let stored = services.reader_zoom(book_id, ReaderZoomSurface::Text).await;
            let _ = view.update(cx, |this, cx| {
                let stored = match stored {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(%error, "cannot read the book's reading text size");
                        return;
                    }
                };
                let Some(stored) = stored else {
                    return;
                };
                if this.closing {
                    return;
                }
                let stored = u8::try_from(stored)
                    .map(reader_font_size)
                    .unwrap_or(READER_FONT_SIZE_DEFAULT);
                if this.text_size == stored {
                    return;
                }
                this.text_size = stored;
                this.apply_reading_appearance(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn schedule_text_size_save(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        let text_size = self.text_size;
        // Replacing the pending task drops the previous one, so the row always
        // ends up with the size the reader stopped at.
        self.text_size_save_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(TEXT_SIZE_SAVE_DEBOUNCE)
                .await;
            let result = services
                .set_reader_zoom(book_id, ReaderZoomSurface::Text, u32::from(text_size))
                .await;
            let _ = view.update(cx, |this, cx| {
                this.text_size_save_task = None;
                if let Err(error) = result {
                    this.set_error(format!("无法保存本书的阅读文字大小：{error:#}"), cx);
                }
            });
        }));
    }
}
