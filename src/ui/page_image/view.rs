//! Paging through the document, zooming and the text panel.

use super::*;

impl PageImageApp {
    pub(super) fn set_current(&mut self, index: usize, cx: &mut Context<Self>) {
        let index = index.min(self.pages.len().saturating_sub(1));
        self.notice = None;
        if self.current == index {
            cx.notify();
            return;
        }
        self.current = index;
        self.sync_ai_references(cx);
        self.queue_progress(cx);
        cx.notify();
    }

    pub(super) fn previous(&mut self, cx: &mut Context<Self>) {
        self.set_current(self.current.saturating_sub(1), cx);
    }

    pub(super) fn next(&mut self, cx: &mut Context<Self>) {
        self.set_current(self.current.saturating_add(1), cx);
    }

    pub(super) fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let next = if self.zoom_milli == ZOOM_FIT {
            ZOOM_MIN_MILLI * 4
        } else {
            self.zoom_milli.saturating_add(ZOOM_STEP_MILLI)
        };
        self.set_zoom_milli(next.min(ZOOM_MAX_MILLI), cx);
    }

    pub(super) fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let next = if self.zoom_milli == ZOOM_FIT {
            ZOOM_MIN_MILLI * 4
        } else {
            self.zoom_milli.saturating_sub(ZOOM_STEP_MILLI)
        };
        self.set_zoom_milli(
            if next < ZOOM_MIN_MILLI {
                ZOOM_FIT
            } else {
                next
            },
            cx,
        );
    }

    pub(super) fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom_milli(ZOOM_FIT, cx);
    }

    /// Ctrl + wheel, the same scale the zoom buttons drive. The page size
    /// belongs to the book, so every change below is also remembered.
    pub(super) fn zoom_from_wheel(&mut self, zoom_in: bool, cx: &mut Context<Self>) {
        self.set_zoom_milli(zoom_after_wheel(self.zoom_milli, zoom_in), cx);
    }

    pub(super) fn set_zoom_milli(&mut self, zoom_milli: u32, cx: &mut Context<Self>) {
        if self.zoom_milli == zoom_milli {
            return;
        }
        self.zoom_milli = zoom_milli;
        self.schedule_zoom_save(cx);
        cx.notify();
    }

    /// Reads this book's remembered page size once per window. Until it arrives
    /// the window is fitted to the window, so an unreadable row keeps that
    /// default rather than blocking the pages.
    pub(super) fn load_zoom(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let stored = services.reader_zoom(book_id, ReaderZoomSurface::Page).await;
            let _ = view.update(cx, |this, cx| {
                let stored = match stored {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(%error, "cannot read the book's page size");
                        return;
                    }
                };
                if this.closing {
                    return;
                }
                let Some(stored) = stored else {
                    return;
                };
                if this.zoom_milli != ZOOM_FIT {
                    // The reader already changed it in this window; the older
                    // row must not pull the page back.
                    return;
                }
                this.set_zoom_milli(
                    if stored == ZOOM_FIT {
                        ZOOM_FIT
                    } else {
                        stored.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI)
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    pub(super) fn schedule_zoom_save(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        let zoom_milli = self.zoom_milli;
        // Replacing the pending task drops the previous one, so the row always
        // ends up with the size the reader stopped at.
        self.zoom_save_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(ZOOM_SAVE_DEBOUNCE_MS))
                .await;
            let result = services
                .set_reader_zoom(book_id, ReaderZoomSurface::Page, zoom_milli)
                .await;
            let _ = view.update(cx, |this, cx| {
                this.zoom_save_task = None;
                if let Err(error) = result {
                    this.notice = Some(Notice {
                        text: format!("无法保存本书的页面大小：{error:#}"),
                        error: true,
                    });
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn toggle_text_panel(&mut self, cx: &mut Context<Self>) {
        self.text_panel_open = !self.text_panel_open;
        cx.notify();
    }

    pub(super) fn sync_ai_references(&mut self, cx: &mut Context<Self>) {
        let references = page_reference_hints(&self.book_id, &self.pages, self.current);
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }

    pub(super) fn handle_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return true;
        }
        self.closing = true;
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
        self.ai_controller.close();
        true
    }

    /// Cancels this reader because its book left the library. The caller
    /// removes the native window; nothing here is left to save.
    pub(super) fn close_for_removed_book(&mut self, cx: &mut Context<Self>) {
        self.handle_window_close(cx);
    }
}
