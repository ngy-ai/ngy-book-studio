//! Construction, scroll reset and refreshing the listing.

use super::*;

impl NotesWindow {
    pub(super) fn new(
        services: Arc<AppServices>,
        library_view: Entity<EpubReaderApp>,
        book: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索书名、章节、引文或想法…"));
        let subscription = cx.subscribe_in(&search, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.listing.query = this.search.read(cx).value().to_string();
                this.listing.apply_filter();
                this.reset_scroll();
                cx.notify();
            }
        });
        Self {
            services,
            library_view,
            book,
            listing: NoteListing::default(),
            search,
            _search_subscription: subscription,
            scroll: ScrollHandle::default(),
            generation: 0,
            loading: false,
            loaded: false,
            error: None,
            copied_note: None,
            opening_note: None,
        }
    }

    pub(super) fn reset_scroll(&mut self) {
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.copied_note = None;
    }

    pub(super) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        let book_id = self.book.as_ref().map(|(id, _)| id.clone());
        let task = self.services.spawn_library_read(move |library| {
            let notes = library.annotation_overview(book_id.as_deref())?;
            Ok(PreparedNotes::new(notes))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(Ok(notes)) => {
                        this.listing.replace_prepared(notes);
                        this.loaded = true;
                        this.reset_scroll();
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("无法读取笔记：{error:#}。请重试刷新。"));
                    }
                    Err(error) => {
                        this.error = Some(format!("笔记读取任务已停止：{error}。请重试刷新。"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
