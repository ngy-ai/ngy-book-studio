//! Construction, progress writes and loading the page texts.

use super::*;

impl PageImageApp {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        book_id: String,
        book_title: String,
        kind: VisualPageSourceKind,
        initial_page: u32,
        book_incarnation: Option<u64>,
        pages: Vec<PageImage>,
        library: LibraryStore,
        services: Arc<AppServices>,
        library_view: Entity<EpubReaderApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = (initial_page.max(1) as usize - 1).min(pages.len().saturating_sub(1));
        let current_book = AiBookOption::new(book_id.clone(), book_title.clone());
        let available_books = library
            .books()
            .iter()
            .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
            .collect();
        let ai_sidebar = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(current_book, available_books),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(page_reference_hints(&book_id, &pages, current), cx);
        });
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        let mut ai_controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book_id.clone()),
        )
        .expect("page image reader AI scope is valid");
        ai_controller.restore(ai_sidebar.clone(), cx);
        let page_texts = vec![None; pages.len()];
        let mut app = Self {
            book_id,
            book_title,
            kind,
            pages,
            current,
            zoom_milli: ZOOM_FIT,
            zoom_save_task: None,
            page_texts,
            text_panel_open: false,
            read_aloud: ReadAloudHost::default(),
            read_aloud_paragraph: None,
            progress_writer: None,
            progress_sync_task: None,
            services,
            library_view,
            ai_sidebar,
            ai_controller,
            _ai_subscription,
            notice: None,
            closing: false,
        };
        if let Some(incarnation) = book_incarnation {
            let (writer, events) = ReadingProgressWriter::start(
                app.book_id.clone(),
                incarnation,
                Arc::clone(&app.services),
            );
            app.progress_writer = Some(writer);
            app.progress_sync_task = Some(cx.spawn(async move |view, cx| {
                while let Ok(event) = events.recv().await {
                    if view
                        .update(cx, |this, cx| this.handle_progress_write_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
            // The initial page is the position the reader was opened at; a
            // citation jump has already been resolved into it.
            app.queue_progress(cx);
        }
        app.load_page_texts(window, cx);
        app.load_zoom(cx);
        app
    }

    /// Persists the current page through the shared ordered progress writer.
    /// A missing writer or a failed write only surfaces a notice.
    pub(super) fn queue_progress(&mut self, cx: &mut Context<Self>) {
        let Some(writer) = self.progress_writer.as_ref() else {
            return;
        };
        let Some(page) = self.pages.get(self.current) else {
            return;
        };
        let progress = ReadingProgressWrite::Unit {
            spine_index: self.current,
            unit_id: page.locator.unit_id.clone(),
        };
        if let Err(error) = writer.enqueue(progress) {
            tracing::warn!(%error, "failed to queue page image reading progress");
            if !self.closing {
                self.notice = Some(Notice {
                    text: format!("阅读进度保存失败：{error}"),
                    error: true,
                });
                cx.notify();
            }
        }
    }

    pub(super) fn handle_progress_write_event(
        &mut self,
        event: ReadingProgressWriteEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some((generation, snapshot)) = event.projection {
            self.library_view.update(cx, |library, cx| {
                library.refresh_after_projected_mutation(generation, snapshot, cx);
            });
        }
        let Some(error) = event.error else {
            return;
        };
        if self.closing {
            return;
        }
        self.notice = Some(Notice {
            text: format!("阅读进度保存失败：{error}"),
            error: true,
        });
        cx.notify();
    }

    /// Loads the canonical page text used by the DjVu text panel. Runs on the
    /// application I/O runtime; a failure only disables the panel.
    pub(super) fn load_page_texts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind != VisualPageSourceKind::Djvu {
            return;
        }
        let services = Arc::clone(&self.services);
        let book_id = self.book_id.clone();
        let unit_ids = self
            .pages
            .iter()
            .map(|page| page.locator.unit_id.clone())
            .collect::<Vec<_>>();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&book_id)?;
            let mut texts = document
                .units
                .iter()
                .map(|unit| (unit.id.clone(), unit.plain_text()))
                .collect::<HashMap<_, _>>();
            Ok(unit_ids
                .into_iter()
                .map(|unit_id| texts.remove(&unit_id))
                .collect::<Vec<_>>())
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(texts)) => {
                        this.text_panel_open = texts.iter().any(|text| {
                            text.as_deref().is_some_and(|text| !text.trim().is_empty())
                        });
                        this.page_texts = texts;
                    }
                    Ok(Err(error)) => {
                        this.notice = Some(Notice {
                            text: format!("无法读取 DjVu 页面文字层：{error:#}"),
                            error: true,
                        });
                    }
                    Err(error) => {
                        this.notice = Some(Notice {
                            text: format!("页面文字读取任务已停止：{error}"),
                            error: true,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
