//! Construction and the persisted zoom level.

use super::*;

impl PdfReaderApp {
    pub(crate) fn new(init: PdfReaderInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let PdfReaderInit {
            book_id,
            book_incarnation,
            document_revision,
            title,
            pages,
            initial_page,
            library,
            services,
            library_view,
            webview_building,
            persist_progress,
            preview_label,
        } = init;
        let current_page = initial_page.max(1);
        let actual_page_count = pages.iter().map(|page| page.page_number).max().unwrap_or(1);
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前 PDF 正文…"));
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let current_book = AiBookOption::new(book_id.clone(), title.clone());
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
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        let _annotation_ai_subscription =
            cx.subscribe(&ai_sidebar, Self::on_pdf_annotation_ai_completed);
        let _annotation_ai_submit_subscription =
            cx.subscribe(&ai_sidebar, Self::on_pdf_annotation_ai_submitted);
        let _annotation_ai_failure_subscription =
            cx.subscribe(&ai_sidebar, Self::on_pdf_annotation_ai_failed);
        let ai_sidebar_collapsed = ai_sidebar.read(cx).is_collapsed();
        let pane_layout = ReaderPaneLayout::new(ai_sidebar.read(cx).expanded_width());
        let _ai_layout_subscription = cx.observe(&ai_sidebar, |this, sidebar, cx| {
            let collapsed = sidebar.read(cx).is_collapsed();
            if this.ai_sidebar_collapsed != collapsed {
                this.ai_sidebar_collapsed = collapsed;
                cx.notify();
            }
        });
        // 皮肤是应用级偏好：设置窗口保存后全局状态变一次，已经打开的 viewer 也要
        // 跟着换外壳颜色，否则它会留在旧配色的页面上。
        let _theme_subscription =
            cx.observe_global::<ngy_book_studio::theme::AppTheme>(|this, cx| {
                this.push_pdf_appearance(cx);
            });
        let mut ai_controller = match AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book_id.clone()),
        ) {
            Ok(controller) => Some(controller),
            Err(error) => {
                tracing::warn!(%error, "failed to initialize PDF AI conversation");
                None
            }
        };
        if let Some(controller) = ai_controller.as_mut() {
            controller.restore(ai_sidebar.clone(), cx);
        }
        let (progress_writer, progress_events) = if persist_progress {
            let (writer, events) = ReadingProgressWriter::start(
                book_id.clone(),
                book_incarnation,
                Arc::clone(&services),
            );
            (Some(writer), Some(events))
        } else {
            (None, None)
        };
        // A failed read is not a reader failure: page spacing falls back to the
        // documented default (the ordinary gap).
        let pdf_compact_reading = services
            .provider_settings()
            .map(|settings| settings.pdf_compact_reading)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read the PDF reading preference");
                false
            });
        let mut reader = Self {
            book_id,
            book_incarnation,
            title,
            pages,
            current_page: current_page.min(actual_page_count),
            actual_page_count,
            page_request_id: 0,
            pdf_selection: None,
            pdf_ready: false,
            webview: None,
            library,
            services,
            library_view,
            ai_sidebar,
            ai_controller,
            pane_layout,
            ai_sidebar_collapsed,
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            page_list_scroll: gpui_kit::UniformListScrollHandle::new(),
            page_list_synced_page: None,
            annotations: PdfAnnotations::new(document_revision),
            read_aloud: ReadAloudHost::default(),
            _annotation_ai_subscription,
            _annotation_ai_submit_subscription,
            _annotation_ai_failure_subscription,
            _search_subscription,
            _ai_subscription,
            _ai_layout_subscription,
            _theme_subscription,
            webview_build_gate: ReaderWebViewBuildGate::new(webview_building),
            ipc_sync_task: None,
            progress_writer,
            progress_sync_task: None,
            closing_webview: None,
            closing: false,
            closing_for_removed_book: false,
            progress_close_ready: !persist_progress,
            removal_scheduled: false,
            notice: None,
            persist_progress_enabled: persist_progress,
            preview_label,
            pdf_compact_reading,
            pdf_zoom_milli: PDF_ZOOM_DEFAULT_MILLI,
            pdf_zoom_save_task: None,
        };
        if let Some(progress_events) = progress_events {
            reader.progress_sync_task = Some(cx.spawn(async move |view, cx| {
                while let Ok(event) = progress_events.recv().await {
                    if view
                        .update(cx, |this, cx| this.handle_progress_write_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        reader.sync_ai_reference(cx);
        reader.load_pdf_zoom(cx);
        reader
    }

    /// Reads this book's remembered page scale once per window.
    ///
    /// The viewer is already building by then, so the stored scale reaches it
    /// either on `ngy-pdf-viewer-ready` or here, whichever comes last; a viewer
    /// that already laid its pages out re-lays them out at the new scale while
    /// keeping the page the reader is looking at. An unreadable row keeps the
    /// default instead of failing the window.
    pub(crate) fn load_pdf_zoom(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let stored = services.reader_zoom(book_id, ReaderZoomSurface::Pdf).await;
            let _ = view.update(cx, |this, cx| {
                let stored = match stored {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(%error, "cannot read the book's PDF page scale");
                        return;
                    }
                };
                if this.closing {
                    return;
                }
                let Some(stored) = stored else {
                    return;
                };
                this.set_pdf_zoom_milli(stored, cx);
            });
        })
        .detach();
    }

    /// Applies a page scale to this window's viewer. The viewer keeps the page
    /// being read in place, so this is safe for an open document, and a viewer
    /// that is still building keeps the scale for its first layout.
    pub(crate) fn set_pdf_zoom_milli(&mut self, zoom_milli: u32, cx: &mut Context<Self>) {
        let zoom_milli = pdf_zoom_milli(zoom_milli);
        if self.pdf_zoom_milli == zoom_milli {
            return;
        }
        self.pdf_zoom_milli = zoom_milli;
        self.push_pdf_zoom(cx);
    }

    /// Sends this window's page scale to the viewer document. Safe to call more
    /// than once: the viewer only re-lays out when the scale really changes, and
    /// a viewer that is still building simply receives it when it reports ready.
    pub(crate) fn push_pdf_zoom(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        let script = pdf_zoom_script(self.pdf_zoom_milli);
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            // Presentation only: a missed update must not turn into reader error.
            tracing::warn!(%error, "cannot apply the PDF page scale");
        }
    }

    pub(crate) fn schedule_pdf_zoom_save(&mut self, cx: &mut Context<Self>) {
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        let zoom_milli = self.pdf_zoom_milli;
        // Replacing the pending task drops the previous one, so the row always
        // ends up with the scale the reader stopped at.
        self.pdf_zoom_save_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(PDF_ZOOM_SAVE_DEBOUNCE).await;
            let result = services
                .set_reader_zoom(book_id, ReaderZoomSurface::Pdf, zoom_milli)
                .await;
            let _ = view.update(cx, |this, cx| {
                this.pdf_zoom_save_task = None;
                if let Err(error) = result {
                    this.notice = Some(Notice {
                        text: format!("无法保存本书的 PDF 页面大小：{error:#}"),
                        error: true,
                    });
                    cx.notify();
                }
            });
        }));
    }
}
