use super::reader::{
    READER_PANE_RESIZE_HANDLE_WIDTH, ReaderPaneLayout, ReaderResizablePane,
    ReadingProgressProjection, ReadingProgressWrite, ReadingProgressWriteEvent,
    ReadingProgressWriter,
};
use super::*;
use gpui::{DragMoveEvent, EmptyView};
use moye_epub_editor::document::DocumentLocator;

const MAX_PDF_IPC_BYTES: usize = 80 * 1024;
const MAX_PDF_SELECTION_BYTES: usize = 32 * 1024;
const PDF_PROTOCOL_CSP: &str = "default-src 'none'; script-src 'self'; worker-src 'self' blob:; \
    style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; font-src 'self' data:; \
    connect-src 'self'; media-src 'none'; object-src 'none'; frame-src 'none'; \
    child-src 'none'; base-uri 'none'; form-action 'none'";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PdfReaderPage {
    /// Canonical unit identity when this rendered page can be associated
    /// without guessing. Ephemeral Office PDFs deliberately leave this empty
    /// when pagination does not line up with the normalized document model.
    pub unit_id: Option<String>,
    pub unit_index: Option<usize>,
    pub title: String,
    pub page_number: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PdfTextSelection {
    request_id: u64,
    page_number: u32,
    text: String,
}

pub(super) struct PdfReaderInit {
    pub book_id: String,
    pub book_incarnation: u64,
    pub title: String,
    pub pages: Vec<PdfReaderPage>,
    pub initial_page: u32,
    pub library: LibraryStore,
    pub services: Arc<AppServices>,
    pub library_view: Entity<EpubReaderApp>,
    pub webview_building: bool,
    pub persist_progress: bool,
    pub preview_label: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum PdfIpcMessage {
    #[serde(rename = "moye-pdf-viewer-ready")]
    ViewerReady,
    #[serde(rename = "moye-pdf-ready")]
    Ready {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "pageCount")]
        page_count: u32,
    },
    #[serde(rename = "moye-pdf-page-changed")]
    PageChanged {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "pageNumber")]
        page_number: u32,
        #[serde(rename = "pageCount")]
        page_count: u32,
    },
    #[serde(rename = "moye-pdf-selection-changed")]
    SelectionChanged {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "pageNumber")]
        page_number: u32,
        #[serde(rename = "selectedText")]
        selected_text: String,
    },
    #[serde(rename = "moye-pdf-error")]
    Error {
        #[serde(rename = "requestId")]
        request_id: u64,
        message: String,
    },
}

fn normalize_pdf_selection(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut separated = false;
    for character in text.chars() {
        if character.is_whitespace() || character.is_control() {
            separated = !normalized.is_empty();
        } else {
            if separated {
                normalized.push(' ');
                separated = false;
            }
            normalized.push(character);
        }
    }
    normalized
}

fn validated_pdf_selection(
    expected_request_id: u64,
    current_page: u32,
    request_id: u64,
    page_number: u32,
    selected_text: &str,
) -> Option<Option<PdfTextSelection>> {
    if request_id != expected_request_id
        || page_number != current_page
        || selected_text.len() > MAX_PDF_SELECTION_BYTES
    {
        return None;
    }
    let text = normalize_pdf_selection(selected_text);
    if text.len() > MAX_PDF_SELECTION_BYTES {
        return None;
    }
    Some((!text.is_empty()).then_some(PdfTextSelection {
        request_id,
        page_number,
        text,
    }))
}

fn is_pdf_navigation_url(url: &str) -> bool {
    url.starts_with("moyepdf://viewer/")
        || url.starts_with("http://moyepdf.viewer/")
        || url.starts_with("https://moyepdf.viewer/")
}

fn pdf_viewer_url(initial_page: u32) -> String {
    format!("moyepdf://viewer/viewer.html?startPage={initial_page}")
}

fn pdf_progress_write(
    page_number: u32,
    page: Option<&PdfReaderPage>,
) -> Result<ReadingProgressWrite, String> {
    let page = page
        .filter(|page| page.page_number == page_number)
        .ok_or_else(|| format!("第 {page_number} 页没有对应的规范内容单元"))?;
    let unit_id = page
        .unit_id
        .as_deref()
        .filter(|unit_id| !unit_id.trim().is_empty())
        .ok_or_else(|| format!("第 {page_number} 页缺少稳定内容单元定位"))?;
    let spine_index = page
        .unit_index
        .ok_or_else(|| format!("第 {page_number} 页缺少规范内容单元序号"))?;
    Ok(ReadingProgressWrite::Unit {
        spine_index,
        unit_id: unit_id.to_string(),
    })
}

fn pdf_resource(
    pdf_bytes: &[u8],
    request_path: &str,
) -> Option<(&'static str, Cow<'static, [u8]>)> {
    let path = request_path.strip_prefix('/').unwrap_or(request_path);
    if path == "document.pdf" {
        return Some(("application/pdf", Cow::Owned(pdf_bytes.to_vec())));
    }
    // PDF.js ships an optional QuickJS sandbox for document actions. This
    // reader renders static pages only, so do not expose that executable at
    // all even though XFA and annotations are also disabled in the viewer.
    if matches!(path, "wasm/quickjs-eval.js" | "wasm/quickjs-eval.wasm") {
        return None;
    }
    bundled_pdfjs_asset(path).map(|(mime, bytes)| (mime, Cow::Borrowed(bytes)))
}

pub(super) async fn build_pdf_reader_webview(
    pdf_bytes: Arc<Vec<u8>>,
    initial_page: u32,
    parent: &ParentWindowHandle,
) -> Result<(
    gpui_component::wry::WebView,
    async_channel::Receiver<PdfIpcMessage>,
)> {
    let (sender, receiver) = async_channel::unbounded();
    let protocol_pdf = pdf_bytes;
    let raw_webview = gpui_component::wry::WebViewBuilder::new()
        .with_ipc_handler(move |request| {
            if request.body().len() > MAX_PDF_IPC_BYTES
                || !is_pdf_navigation_url(&request.uri().to_string())
            {
                return;
            }
            let Ok(message) = serde_json::from_str::<PdfIpcMessage>(request.body()) else {
                return;
            };
            let _ = sender.try_send(message);
        })
        .with_custom_protocol("moyepdf".to_string(), move |_, request| {
            let Some((mime, body)) = pdf_resource(protocol_pdf.as_slice(), request.uri().path())
            else {
                return gpui_component::wry::http::Response::builder()
                    .status(404)
                    .header("Content-Type", "text/plain; charset=utf-8")
                    .header("Content-Security-Policy", "default-src 'none'")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Cow::Owned(b"PDF resource not found".to_vec()))
                    .expect("valid PDF error response");
            };
            let content_length = body.len();
            gpui_component::wry::http::Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("Content-Length", content_length.to_string())
                .header("Content-Security-Policy", PDF_PROTOCOL_CSP)
                .header(
                    "Permissions-Policy",
                    "camera=(), microphone=(), geolocation=()",
                )
                .header("Referrer-Policy", "no-referrer")
                .header("X-Content-Type-Options", "nosniff")
                .header("Cache-Control", "no-store")
                .body(body)
                .expect("valid local PDF response")
        })
        .with_navigation_handler(|url| is_pdf_navigation_url(&url))
        .with_new_window_req_handler(|_, _| gpui_component::wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_background_color((36, 36, 36, 255))
        .with_hotkeys_zoom(false)
        .with_incognito(true)
        .with_url(pdf_viewer_url(initial_page))
        .build_as_child_async(parent)
        .await
        .context("无法创建 PDF 视图")?;

    Ok((raw_webview, receiver))
}

/// A native reader window that renders the immutable imported PDF with the
/// checked-in PDF.js build. Search and AI use the canonical page units, while
/// the WebView never receives an object-store path or a network URL.
pub struct PdfReaderApp {
    book_id: String,
    book_incarnation: u64,
    title: String,
    pages: Vec<PdfReaderPage>,
    current_page: u32,
    actual_page_count: u32,
    page_request_id: u64,
    pdf_selection: Option<PdfTextSelection>,
    pdf_ready: bool,
    webview: Option<Entity<WebView>>,
    library: LibraryStore,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    ai_sidebar: Entity<AiSidebar>,
    ai_controller: Option<AiSidebarController>,
    pane_layout: ReaderPaneLayout,
    ai_sidebar_collapsed: bool,
    search_input: Entity<InputState>,
    search_query: String,
    search_results: Vec<SearchHit>,
    _search_subscription: Subscription,
    _ai_subscription: Subscription,
    _ai_layout_subscription: Subscription,
    webview_build_gate: ReaderWebViewBuildGate,
    pub(super) ipc_sync_task: Option<Task<()>>,
    progress_writer: Option<ReadingProgressWriter>,
    progress_sync_task: Option<Task<()>>,
    closing_webview: Option<WeakEntity<WebView>>,
    closing: bool,
    progress_close_ready: bool,
    removal_scheduled: bool,
    notice: Option<Notice>,
    persist_progress_enabled: bool,
    preview_label: String,
}

impl PdfReaderApp {
    pub(super) fn new(init: PdfReaderInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let PdfReaderInit {
            book_id,
            book_incarnation,
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
        let ai_sidebar_collapsed = ai_sidebar.read(cx).is_collapsed();
        let pane_layout = ReaderPaneLayout::new(ai_sidebar.read(cx).expanded_width());
        let _ai_layout_subscription = cx.observe(&ai_sidebar, |this, sidebar, cx| {
            let collapsed = sidebar.read(cx).is_collapsed();
            if this.ai_sidebar_collapsed != collapsed {
                this.ai_sidebar_collapsed = collapsed;
                cx.notify();
            }
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
            _search_subscription,
            _ai_subscription,
            _ai_layout_subscription,
            webview_build_gate: ReaderWebViewBuildGate::new(webview_building),
            ipc_sync_task: None,
            progress_writer,
            progress_sync_task: None,
            closing_webview: None,
            closing: false,
            progress_close_ready: !persist_progress,
            removal_scheduled: false,
            notice: None,
            persist_progress_enabled: persist_progress,
            preview_label,
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
        reader
    }

    fn on_ai_sidebar_event(
        &mut self,
        sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.submit(request.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_answer(
                            request.request_id,
                            "AI 会话尚未初始化，请检查设置后重试。".to_string(),
                            cx,
                        );
                    });
                }
            }
            AiSidebarEvent::Cancel { request_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.cancel(*request_id);
                }
            }
            AiSidebarEvent::NewSession => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.new_session(sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::SwitchSession { thread_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.switch_session(thread_id.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::DeleteSession { thread_id } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.delete_session(thread_id.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::ScopeChanged { book_ids } => {
                if let Some(controller) = self.ai_controller.as_mut() {
                    controller.reconcile_scope(book_ids.clone(), sidebar.clone(), cx);
                } else {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.fail_session_operation("AI 会话尚未初始化，请稍后重试。", cx);
                    });
                }
            }
            AiSidebarEvent::OpenSource(source) => {
                self.open_ai_source(source.clone(), window, cx);
            }
        }
    }

    fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            current_canonical_source_unit_index(&lookup, &document).map_err(anyhow::Error::msg)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) => {
                        if source.book_id == this.book_id {
                            let page_number =
                                pdf_page_for_source(&this.pages, &source).or_else(|| {
                                    (!pdf_source_requires_exact_page(&source))
                                        .then(|| this.page_for_unit_index(Some(index)))
                                        .flatten()
                                });
                            if let Some(page_number) = page_number {
                                this.request_page(page_number, cx);
                            } else {
                                this.notice = Some(Notice {
                                    text: "引用对应的 PDF 页面已失效，未跳转到其它页面。"
                                        .to_string(),
                                    error: true,
                                });
                                cx.notify();
                            }
                        } else {
                            this.library_view.update(cx, |library, cx| {
                                library.open_book_at_source(source, Some(index), window, cx);
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        this.notice = Some(Notice {
                            text: format!("引用已失效：{error:#}"),
                            error: true,
                        });
                        cx.notify();
                    }
                    Err(error) => {
                        this.notice = Some(Notice {
                            text: format!("引用定位任务已停止：{error}"),
                            error: true,
                        });
                        cx.notify();
                    }
                });
            });
        })
        .detach();
    }

    fn on_search_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.search_query = input.read(cx).value().trim().to_string();
                self.search_results.clear();
                if !self.search_query.is_empty() {
                    match self
                        .library
                        .search_book(&self.book_id, &self.search_query, 200)
                    {
                        Ok(results) => self.search_results = results,
                        Err(error) => {
                            self.notice = Some(Notice {
                                text: format!("搜索当前 PDF 失败：{error:#}"),
                                error: true,
                            });
                        }
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(index) = self.search_results.first().and_then(|hit| hit.spine_index) {
                    self.open_canonical_unit(index, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn attach_webview(
        &mut self,
        webview: Entity<WebView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.webview = Some(webview);
        if self.webview_build_gate.finish() {
            self.schedule_window_removal(window, cx);
            // A persistent PDF reader keeps its WebView until the final write
            // completes. Install the IPC receiver now so failure recovery does
            // not reopen a view that can no longer report page changes.
            return self.webview.is_some();
        }
        self.notice = None;
        cx.notify();
        true
    }

    pub(super) fn fail_webview_build(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.webview_build_gate.finish() {
            self.schedule_window_removal(window, cx);
            return;
        }
        self.set_error(error, cx);
    }

    pub(super) fn handle_ipc(&mut self, message: PdfIpcMessage, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        match message {
            PdfIpcMessage::ViewerReady => {
                self.notice = Some(Notice {
                    text: "正在解析本地 PDF…".to_string(),
                    error: false,
                });
            }
            PdfIpcMessage::Ready {
                request_id,
                page_count,
            } if request_id == self.page_request_id && page_count > 0 => {
                self.actual_page_count = page_count;
                self.current_page = self.current_page.min(page_count).max(1);
                self.pdf_ready = true;
                self.notice = None;
            }
            PdfIpcMessage::PageChanged {
                request_id,
                page_number,
                page_count,
            } if request_id == self.page_request_id
                && page_count > 0
                && (1..=page_count).contains(&page_number) =>
            {
                self.actual_page_count = page_count;
                self.current_page = page_number;
                self.pdf_selection = None;
                self.pdf_ready = true;
                self.sync_ai_reference(cx);
                self.notice = None;
                self.persist_progress(cx);
            }
            PdfIpcMessage::SelectionChanged {
                request_id,
                page_number,
                selected_text,
            } => {
                let Some(selection) = validated_pdf_selection(
                    self.page_request_id,
                    self.current_page,
                    request_id,
                    page_number,
                    &selected_text,
                ) else {
                    return;
                };
                if self.pdf_selection != selection {
                    self.pdf_selection = selection;
                    self.sync_ai_reference(cx);
                }
            }
            PdfIpcMessage::Error {
                request_id,
                message,
            } if request_id == self.page_request_id => {
                self.notice = Some(Notice {
                    text: format!("PDF 预览失败：{message}"),
                    error: true,
                });
            }
            _ => return,
        }
        cx.notify();
    }

    fn request_page(&mut self, page_number: u32, cx: &mut Context<Self>) {
        if self.closing || !self.pdf_ready {
            return;
        }
        let page_number = page_number.clamp(1, self.actual_page_count.max(1));
        let Some(webview) = self.webview.clone() else {
            return;
        };
        if self.pdf_selection.take().is_some() {
            self.sync_ai_reference(cx);
        }
        self.page_request_id = self.page_request_id.wrapping_add(1).max(1);
        let payload = serde_json::json!({
            "type": "moye-pdf-go-to",
            "requestId": self.page_request_id,
            "pageNumber": page_number,
        });
        let script = format!("window.postMessage({payload}, window.location.origin);");
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.notice = Some(Notice {
                text: format!("无法打开第 {page_number} 页：{error}"),
                error: true,
            });
            cx.notify();
        }
    }

    fn current_page_info(&self) -> Option<&PdfReaderPage> {
        self.pages
            .iter()
            .find(|page| page.page_number == self.current_page)
    }

    fn page_for_unit_index(&self, unit_index: Option<usize>) -> Option<u32> {
        let unit_index = unit_index?;
        self.pages
            .iter()
            .find(|page| page.unit_index == Some(unit_index))
            .map(|page| page.page_number)
    }

    fn open_canonical_unit(
        &mut self,
        unit_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page_number) = self.page_for_unit_index(Some(unit_index)) {
            self.request_page(page_number, cx);
            return;
        }
        let book_id = self.book_id.clone();
        self.library_view.update(cx, |library, cx| {
            library.open_book_at(book_id, Some(unit_index), window, cx);
        });
    }

    fn sync_ai_reference(&mut self, cx: &mut Context<Self>) {
        let references = pdf_reference_hints(
            &self.book_id,
            &self.pages,
            self.current_page,
            self.page_request_id,
            self.pdf_selection.as_ref(),
        );
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }

    fn persist_progress(&mut self, cx: &mut Context<Self>) {
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

    fn handle_progress_write_event(
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

    fn apply_progress_projection(
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

    fn start_progress_writer(&mut self, cx: &mut Context<Self>) {
        if !self.persist_progress_enabled {
            return;
        }
        let (writer, progress_events) = ReadingProgressWriter::start(
            self.book_id.clone(),
            self.book_incarnation,
            Arc::clone(&self.services),
        );
        self.progress_writer = Some(writer);
        self.progress_sync_task = Some(cx.spawn(async move |view, cx| {
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

    fn finish_progress_writer(&mut self, cx: &mut Context<Self>) {
        if !self.persist_progress_enabled {
            self.complete_progress_close(cx);
            return;
        }
        let final_progress = match pdf_progress_write(self.current_page, self.current_page_info()) {
            Ok(progress) => progress,
            Err(error) => {
                self.recover_from_progress_close(
                    format!("无法解析当前 PDF 的稳定阅读位置：{error}"),
                    cx,
                );
                return;
            }
        };
        if let Some(writer) = self.progress_writer.take() {
            let (queued, completion) = writer.finish(final_progress);
            let queue_error = queued.err();
            cx.spawn(async move |view, cx| {
                let result = match queue_error {
                    Some(error) => Err(error),
                    None => completion.recv().await.unwrap_or_else(|_| {
                        Err("PDF 阅读进度后台任务在完成关闭屏障前停止".to_string())
                    }),
                };
                let _ = view.update(cx, |this, cx| match result {
                    Ok(projection) => {
                        // Do not rely on the final best-effort event: close
                        // teardown drops its listener after this callback.
                        this.apply_progress_projection(projection, cx);
                        this.complete_progress_close(cx);
                    }
                    Err(error) => this.recover_from_progress_close(error, cx),
                });
            })
            .detach();
        } else {
            self.recover_from_progress_close("PDF 阅读进度后台任务不可用。".to_string(), cx);
        }
    }

    fn complete_progress_close(&mut self, cx: &mut Context<Self>) {
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
        if let Some(controller) = self.ai_controller.as_mut() {
            controller.close();
        }
        self.progress_sync_task.take();
        self.closing_webview = self.release_webview(cx);
        self.progress_close_ready = true;
        cx.notify();
    }

    fn recover_from_progress_close(&mut self, error: String, cx: &mut Context<Self>) {
        tracing::warn!(%error, "final PDF reading progress was not persisted");
        self.progress_writer.take();
        self.progress_sync_task.take();
        self.closing = false;
        self.progress_close_ready = false;
        self.removal_scheduled = false;
        self.start_progress_writer(cx);
        self.notice = Some(Notice {
            text: format!(
                "PDF 阅读进度保存失败，窗口已保持打开。请检查磁盘或权限后再次关闭以重试：{error}"
            ),
            error: true,
        });
        cx.notify();
    }

    pub(super) fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice { text, error: true });
        cx.notify();
    }

    fn release_webview(&mut self, cx: &mut Context<Self>) -> Option<WeakEntity<WebView>> {
        self.ipc_sync_task.take();
        self.webview.take().map(|webview| {
            let weak = webview.downgrade();
            webview.update(cx, |webview, _| webview.hide());
            weak
        })
    }

    fn schedule_window_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.progress_close_ready = false;
        self.finish_progress_writer(cx);
        cx.notify();
        window.refresh();
    }

    pub(super) fn handle_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.closing {
            return false;
        }
        if self.webview_build_gate.request_close() {
            self.notice = Some(Notice {
                text: "正在安全关闭 PDF 阅读器…".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        self.schedule_window_removal(window, cx);
        false
    }

    fn render_pane_resize_handle(
        &self,
        pane: ReaderResizablePane,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let drag_view = view.clone();
        let handle_id = match pane {
            ReaderResizablePane::Navigation => "pdf-navigation-resize-handle",
            ReaderResizablePane::Ai => "pdf-ai-resize-handle",
        };
        let active = self.pane_layout.is_resizing(pane);

        div()
            .id(handle_id)
            .h_full()
            .w(px(READER_PANE_RESIZE_HANDLE_WIDTH))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h_full()
                    .w(px(if active { 2. } else { 1. }))
                    .bg(rgb(if active { ACCENT } else { BORDER })),
            )
            .hover(|this| this.bg(rgb(ACCENT_SOFT)))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<PdfPaneResizeDrag>, window, cx| {
                    if event.drag(cx).0 != pane {
                        return;
                    }
                    if this.pane_layout.resize_from_pointer(
                        pane,
                        event.event.position.x,
                        window.viewport_size().width,
                        this.ai_sidebar_collapsed,
                    ) {
                        if pane == ReaderResizablePane::Ai {
                            let width = this.pane_layout.ai_width();
                            this.ai_sidebar
                                .update(cx, |sidebar, cx| sidebar.set_expanded_width(width, cx));
                        }
                        cx.notify();
                    }
                },
            ))
            .on_drag(
                PdfPaneResizeDrag(pane),
                move |_: &PdfPaneResizeDrag, _, _, cx| {
                    cx.stop_propagation();
                    drag_view.update(cx, |this, cx| {
                        if this.pane_layout.begin_resize(pane) {
                            cx.notify();
                        }
                    });
                    cx.new(|_| EmptyView)
                },
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.pane_layout.finish_resize() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.pane_layout.finish_resize() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug)]
struct PdfPaneResizeDrag(ReaderResizablePane);

fn pdf_page_for_source(pages: &[PdfReaderPage], source: &AiSourceLink) -> Option<u32> {
    if source.stale {
        return None;
    }
    if let Some(locator) = source.validated_locator() {
        match locator.source.as_ref() {
            Some(SourceLocator::PdfPage { page }) => {
                return pages
                    .iter()
                    .any(|candidate| {
                        candidate.page_number == *page
                            && candidate.unit_id.as_deref() == Some(source.unit_id.as_str())
                    })
                    .then_some(*page);
            }
            Some(_) => return None,
            None => {}
        }
    }
    if source.locator.is_some() && source.validated_locator().is_none() {
        return None;
    }
    pages
        .iter()
        .find(|page| page.unit_id.as_deref() == Some(source.unit_id.as_str()))
        .map(|page| page.page_number)
}

fn pdf_source_requires_exact_page(source: &AiSourceLink) -> bool {
    source.locator.as_ref().is_some_and(|_| {
        source
            .validated_locator()
            .is_none_or(|locator| locator.source.is_some())
    })
}

impl Render for PdfReaderApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.closing && self.progress_close_ready && !self.removal_scheduled {
            self.removal_scheduled = true;
            remove_window_after_current_frame(window, cx, self.closing_webview.take());
        }
        if self
            .pane_layout
            .constrain(window.viewport_size().width, self.ai_sidebar_collapsed)
        {
            let width = self.pane_layout.ai_width();
            self.ai_sidebar
                .update(cx, |sidebar, cx| sidebar.set_expanded_width(width, cx));
        }
        let view = cx.entity().clone();
        let previous_view = view.clone();
        let next_view = view.clone();
        let current_page = self.current_page;
        let total = self.actual_page_count.max(1);
        let progress = current_page as f32 / total as f32;
        let page_title = self
            .current_page_info()
            .map(|page| page.title.clone())
            .unwrap_or_else(|| format!("第 {current_page} 页"));
        let (status_icon, status_text, status_color) =
            if self.closing || self.webview_build_gate.close_requested {
                (IconName::BookOpen, "正在安全关闭 PDF 阅读器…", ACCENT)
            } else if self.webview_build_gate.building || !self.pdf_ready {
                (IconName::BookOpen, "正在加载本地 PDF…", ACCENT)
            } else if self.webview.is_some() {
                (IconName::CircleCheck, "PDF.js 视图已连接", 0x376441)
            } else {
                (IconName::TriangleAlert, "PDF 视图不可用", DANGER)
            };
        let status_bar = render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            format!("第 {current_page} / {total} 页 · {page_title}"),
            if self.search_query.is_empty() {
                if self.persist_progress_enabled {
                    format!("页面进度 · {:.0}%", progress * 100.)
                } else {
                    "临时预览 · 不修改阅读进度".to_string()
                }
            } else {
                format!("全文检索 · {} 个结果", self.search_results.len())
            },
        );

        let toolbar = div()
            .v_flex()
            .bg(rgb(SURFACE))
            .border_b_1()
            .border_color(rgb(BORDER))
            .when_some(self.notice.as_ref(), |toolbar, notice| {
                let (background, foreground, icon) = if notice.error {
                    (rgb(0xf7e1df), rgb(0x9f302c), IconName::TriangleAlert)
                } else {
                    (rgb(0xe3efe5), rgb(0x376441), IconName::CircleCheck)
                };
                toolbar.child(
                    div()
                        .h_flex()
                        .min_h(px(36.))
                        .px_5()
                        .gap_2()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .bg(background)
                        .text_xs()
                        .text_color(foreground)
                        .child(Icon::new(icon).small())
                        .child(notice.text.clone()),
                )
            })
            .child(
                div()
                    .h_flex()
                    .h(px(66.))
                    .px_5()
                    .justify_between()
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .gap_0p5()
                            .child(
                                div()
                                    .max_w(px(560.))
                                    .truncate()
                                    .font_semibold()
                                    .text_color(rgb(INK))
                                    .child(self.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(format!("{} · {page_title}", self.preview_label)),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .mr_2()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(format!("{current_page} / {total}")),
                            )
                            .child(
                                Button::new("previous-pdf-page")
                                    .ghost()
                                    .icon(IconName::ChevronLeft)
                                    .tooltip("上一页")
                                    .disabled(!self.pdf_ready || current_page <= 1)
                                    .on_click(move |_, _, cx| {
                                        previous_view.update(cx, |this, cx| {
                                            this.request_page(current_page.saturating_sub(1), cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("next-pdf-page")
                                    .ghost()
                                    .icon(IconName::ChevronRight)
                                    .tooltip("下一页")
                                    .disabled(!self.pdf_ready || current_page >= total)
                                    .on_click(move |_, _, cx| {
                                        next_view.update(cx, |this, cx| {
                                            this.request_page(current_page.saturating_add(1), cx)
                                        });
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(3.))
                    .w_full()
                    .bg(rgb(BORDER))
                    .child(div().h_full().w(gpui::relative(progress)).bg(rgb(ACCENT))),
            );

        let navigation_items = if self.search_query.is_empty() {
            vec![
                div()
                    .v_flex()
                    .gap_2()
                    .p_4()
                    .rounded(px(8.))
                    .bg(rgb(ACCENT_SOFT))
                    .text_sm()
                    .text_color(rgb(INK))
                    .child(div().font_semibold().child(format!("第 {current_page} 页")))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(page_title))
                    .into_any_element(),
            ]
        } else if self.search_results.is_empty() {
            vec![
                div()
                    .p_4()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("当前 PDF 没有匹配页面")
                    .into_any_element(),
            ]
        } else {
            self.search_results
                .iter()
                .enumerate()
                .map(|(index, hit)| {
                    let result_view = view.clone();
                    let unit_index = hit.spine_index;
                    let page_number = self.page_for_unit_index(unit_index);
                    let selected = page_number == Some(current_page);
                    Button::new(("pdf-search-result", index))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(66.))
                        .justify_start()
                        .px_3()
                        .py_2()
                        .rounded(px(7.))
                        .when(selected, |this| this.bg(rgb(ACCENT_SOFT)))
                        .disabled(unit_index.is_none())
                        .on_click(move |_, window, cx| {
                            if let Some(unit_index) = unit_index {
                                result_view.update(cx, |this, cx| {
                                    this.open_canonical_unit(unit_index, window, cx)
                                });
                            }
                        })
                        .child(
                            div()
                                .v_flex()
                                .w_full()
                                .min_w(px(0.))
                                .gap_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_left()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(if selected { rgb(ACCENT) } else { rgb(INK) })
                                        .child(
                                            hit.chapter_title
                                                .clone()
                                                .or_else(|| {
                                                    page_number.map(|page| format!("第 {page} 页"))
                                                })
                                                .unwrap_or_else(|| "PDF".to_string()),
                                        ),
                                )
                                .child(
                                    div()
                                        .line_clamp(2)
                                        .text_left()
                                        .text_xs()
                                        .text_color(rgb(MUTED))
                                        .child(hit.snippet.clone()),
                                ),
                        )
                        .into_any_element()
                })
                .collect()
        };

        let navigation = div()
            .v_flex()
            .w(self.pane_layout.navigation_width())
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SIDEBAR))
            .child(
                div()
                    .h_flex()
                    .h(px(54.))
                    .px_5()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(INK))
                    .child(Icon::new(IconName::Menu).small())
                    .child(if self.search_query.is_empty() {
                        "页面".to_string()
                    } else {
                        format!("搜索结果（{}）", self.search_results.len())
                    }),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        Input::new(&self.search_input)
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .p_3()
                    .overflow_y_scrollbar()
                    .child(div().v_flex().gap_1().children(navigation_items)),
            );

        let body = match &self.webview {
            Some(webview) => webview.clone().into_any_element(),
            None => div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(rgb(MUTED))
                .child(Icon::new(IconName::BookOpen).large())
                .child(div().text_sm().child("正在加载 PDF.js…"))
                .into_any_element(),
        };
        let content = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(rgb(0x242424))
            .child(body);
        let navigation_resize_handle =
            self.render_pane_resize_handle(ReaderResizablePane::Navigation, cx);
        let ai_resize_handle = (!self.ai_sidebar_collapsed)
            .then(|| self.render_pane_resize_handle(ReaderResizablePane::Ai, cx));

        div()
            .v_flex()
            .size_full()
            .bg(rgb(PAPER))
            .child(toolbar)
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .flex_1()
                    .min_h(px(0.))
                    .child(navigation)
                    .child(navigation_resize_handle)
                    .child(content)
                    .when_some(ai_resize_handle, |this, handle| this.child(handle))
                    .child(self.ai_sidebar.clone()),
            )
            .child(status_bar)
            .into_any_element()
    }
}

fn pdf_reference_hints(
    book_id: &str,
    pages: &[PdfReaderPage],
    current_page: u32,
    current_request_id: u64,
    selection: Option<&PdfTextSelection>,
) -> Vec<AiReferenceHint> {
    let mut pages = pages
        .iter()
        .filter(|page| page.unit_id.is_some())
        .collect::<Vec<_>>();
    pages.sort_by_key(|page| (page.page_number != current_page, page.page_number));
    pages
        .into_iter()
        .filter_map(|page| {
            let unit_id = page.unit_id.as_ref()?;
            let selected_text = selection
                .filter(|selection| {
                    selection.request_id == current_request_id
                        && selection.page_number == page.page_number
                })
                .map(|selection| selection.text.clone());
            Some(AiReferenceHint {
                book_id: book_id.to_string(),
                unit_id: unit_id.clone(),
                unit_index: page.unit_index,
                locator: Some(
                    DocumentLocator::unit(book_id, unit_id)
                        .with_source(SourceLocator::pdf_page(page.page_number)),
                ),
                label: if selected_text.is_some() {
                    format!("当前页高亮 · 第 {} 页 · {}", page.page_number, page.title)
                } else if page.page_number == current_page {
                    format!("当前页 · 第 {} 页 · {}", page.page_number, page.title)
                } else {
                    format!("第 {} 页 · {}", page.page_number, page.title)
                },
                frozen_text: selected_text,
                revision: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_only_exposes_the_document_and_checked_in_assets() {
        let pdf = b"%PDF-test";
        let (mime, bytes) = pdf_resource(pdf, "/document.pdf").expect("document route");
        assert_eq!(mime, "application/pdf");
        assert_eq!(bytes.as_ref(), pdf);
        assert!(pdf_resource(pdf, "/viewer.html").is_some());
        assert!(pdf_resource(pdf, "/../document.pdf").is_none());
        assert!(pdf_resource(pdf, "/%2e%2e/pdf.mjs").is_none());
        assert!(pdf_resource(pdf, "/https://example.com/file").is_none());
        assert!(pdf_resource(pdf, "/wasm/quickjs-eval.js").is_none());
        assert!(pdf_resource(pdf, "/wasm/quickjs-eval.wasm").is_none());
    }

    #[test]
    fn protocol_navigation_stays_on_the_private_origin() {
        assert!(is_pdf_navigation_url("moyepdf://viewer/viewer.html"));
        assert!(is_pdf_navigation_url("http://moyepdf.viewer/pdf.mjs"));
        assert!(!is_pdf_navigation_url("https://example.com/document.pdf"));
        assert!(!is_pdf_navigation_url("moyepdf://attacker/viewer.html"));
    }

    #[test]
    fn ipc_page_change_is_typed() {
        let message = serde_json::from_str::<PdfIpcMessage>(
            r#"{"type":"moye-pdf-page-changed","requestId":7,"pageNumber":3,"pageCount":9,"pdfjsVersion":"5.7.284"}"#,
        )
        .expect("typed page message");
        assert!(matches!(
            message,
            PdfIpcMessage::PageChanged {
                request_id: 7,
                page_number: 3,
                page_count: 9
            }
        ));
    }

    #[test]
    fn pdf_progress_requires_the_exact_canonical_unit_locator() {
        let canonical = PdfReaderPage {
            unit_id: Some("unit-3".to_string()),
            unit_index: Some(7),
            title: "Three".to_string(),
            page_number: 3,
        };
        assert_eq!(
            pdf_progress_write(3, Some(&canonical)),
            Ok(ReadingProgressWrite::Unit {
                spine_index: 7,
                unit_id: "unit-3".to_string(),
            })
        );
        assert!(pdf_progress_write(2, Some(&canonical)).is_err());
        assert!(pdf_progress_write(3, None).is_err());

        let missing_unit = PdfReaderPage {
            unit_id: None,
            ..canonical.clone()
        };
        assert!(pdf_progress_write(3, Some(&missing_unit)).is_err());

        let missing_index = PdfReaderPage {
            unit_index: None,
            ..canonical
        };
        assert!(pdf_progress_write(3, Some(&missing_index)).is_err());
    }

    #[test]
    fn ipc_selection_change_is_typed_and_bounded() {
        let message = serde_json::from_str::<PdfIpcMessage>(
            r#"{"type":"moye-pdf-selection-changed","requestId":7,"pageNumber":3,"selectedText":"  first\n\tsecond  ","pdfjsVersion":"5.7.284"}"#,
        )
        .expect("typed selection message");
        let PdfIpcMessage::SelectionChanged {
            request_id,
            page_number,
            selected_text,
        } = message
        else {
            panic!("expected selection message");
        };
        let selection = validated_pdf_selection(7, 3, request_id, page_number, &selected_text)
            .expect("current request and page")
            .expect("non-empty selection");
        assert_eq!(selection.text, "first second");

        assert!(validated_pdf_selection(8, 3, request_id, page_number, &selected_text).is_none());
        assert!(validated_pdf_selection(7, 4, request_id, page_number, &selected_text).is_none());
        assert!(
            validated_pdf_selection(7, 3, 7, 3, &"x".repeat(MAX_PDF_SELECTION_BYTES + 1)).is_none()
        );
        assert_eq!(validated_pdf_selection(7, 3, 7, 3, " \n\t "), Some(None));
    }

    #[test]
    fn ai_reference_options_include_current_then_all_canonical_pages() {
        let pages = vec![
            PdfReaderPage {
                unit_id: Some("unit-1".to_string()),
                unit_index: Some(0),
                title: "One".to_string(),
                page_number: 1,
            },
            PdfReaderPage {
                unit_id: None,
                unit_index: None,
                title: "Ephemeral".to_string(),
                page_number: 2,
            },
            PdfReaderPage {
                unit_id: Some("unit-3".to_string()),
                unit_index: Some(2),
                title: "Three".to_string(),
                page_number: 3,
            },
        ];

        let references = pdf_reference_hints("book-a", &pages, 3, 0, None);

        assert_eq!(
            references
                .iter()
                .map(|reference| reference.unit_id.as_str())
                .collect::<Vec<_>>(),
            vec!["unit-3", "unit-1"]
        );
        assert!(references[0].label.starts_with("当前页"));
        assert_eq!(
            references[0].locator,
            Some(DocumentLocator::unit("book-a", "unit-3").with_source(SourceLocator::pdf_page(3)))
        );

        let source = AiSourceLink {
            citation_id: "source-3".to_string(),
            book_id: "book-a".to_string(),
            unit_id: "unit-3".to_string(),
            unit_index: None,
            document_revision: moye_epub_editor::document::Revision::new(1),
            unit_revision: moye_epub_editor::document::Revision::new(1),
            locator: references[0].locator.clone(),
            label: "Page 3".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
        };
        assert_eq!(pdf_page_for_source(&pages, &source), Some(3));

        let stale = AiSourceLink {
            locator: Some(
                DocumentLocator::unit("book-a", "unit-3").with_source(SourceLocator::pdf_page(99)),
            ),
            ..source.clone()
        };
        assert_eq!(
            pdf_page_for_source(&pages, &stale),
            None,
            "a stale exact PDF locator must not degrade to the unit's first page"
        );

        let wrong_source_type = AiSourceLink {
            locator: Some(
                DocumentLocator::unit("book-a", "unit-3").with_source(SourceLocator::slide(3)),
            ),
            ..source
        };
        assert_eq!(pdf_page_for_source(&pages, &wrong_source_type), None);
        assert!(
            pdf_source_requires_exact_page(&wrong_source_type),
            "a supplied non-PDF source locator must be rejected instead of using the unit fallback"
        );
    }

    #[test]
    fn current_pdf_selection_freezes_only_the_current_page_reference() {
        let pages = vec![
            PdfReaderPage {
                unit_id: Some("unit-1".to_string()),
                unit_index: Some(0),
                title: "One".to_string(),
                page_number: 1,
            },
            PdfReaderPage {
                unit_id: Some("unit-2".to_string()),
                unit_index: Some(1),
                title: "Two".to_string(),
                page_number: 2,
            },
        ];
        let selection = PdfTextSelection {
            request_id: 11,
            page_number: 2,
            text: "exact highlighted text".to_string(),
        };

        let references = pdf_reference_hints("book-a", &pages, 2, 11, Some(&selection));

        assert_eq!(references[0].unit_id, "unit-2");
        assert!(references[0].label.starts_with("当前页高亮"));
        assert_eq!(
            references[0].frozen_text.as_deref(),
            Some("exact highlighted text")
        );
        assert_eq!(references[1].frozen_text, None);

        let stale_references = pdf_reference_hints("book-a", &pages, 2, 12, Some(&selection));
        assert_eq!(stale_references[0].frozen_text, None);
        assert!(stale_references[0].label.starts_with("当前页 ·"));
    }
}
