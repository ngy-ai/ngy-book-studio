use super::*;
use super::{
    ai_controller::AiSidebarController,
    ai_sidebar::{
        AiCitationTextFocus, AiSidebarEvent, AiSourceLink, citation_dom_navigation_script,
    },
};
use anyhow::ensure;
use gpui::{DragMoveEvent, EmptyView, Pixels};
use moye_epub_editor::{
    chat::ChatWindowKind,
    document::{BookDocument, DocumentLocator, SourceLocator},
    media::MediaService,
    reader::{
        AuthorizedReaderAsset, ReaderResourceAuthorizations, ResourceResponse,
        load_resource_with_range,
    },
    services::{AppServices, TranslationDisplayMode},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

mod annotations;
use annotations::{AnnotationAction, ReaderAnnotations};

mod translations;
use translations::ReaderTranslations;

#[cfg(target_os = "windows")]
pub(super) mod selection_menu;

const MAX_READER_SELECTION_BYTES: usize = 32 * 1024;
const READER_NAVIGATION_DEFAULT_WIDTH: f32 = 286.;
const READER_NAVIGATION_MIN_WIDTH: f32 = 200.;
const READER_NAVIGATION_MAX_WIDTH: f32 = 480.;
pub(super) const READER_NAVIGATION_COLLAPSED_WIDTH: f32 = 44.;
const READER_CONTENT_MIN_WIDTH: f32 = 320.;
pub(super) const READER_PANE_RESIZE_HANDLE_WIDTH: f32 = 6.;
const READER_CSP: &str = "default-src 'self' data:; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; media-src 'self' data:";
const READER_INITIALIZATION_SCRIPT: &str = r#"
(() => {
  "use strict";
  const MAX_SELECTION_BYTES = 32768;
  const encoder = new TextEncoder();
  let timer = 0;

  const ownDocument = () => {
    const url = new URL(window.location.href);
    return window.top === window && (
      (url.protocol === "epubreader:" && url.hostname === "book") ||
      ((url.protocol === "http:" || url.protocol === "https:") &&
        url.hostname === "epubreader.book")
    );
  };

  const isChapterNode = (node) => node && node.getRootNode() === document &&
    document.body?.contains(node);

  const insideTranslation = (node) => {
    const element = node && (node.nodeType === 1 ? node : node.parentElement);
    return !!(element && element.closest?.("[data-moye-translation]"));
  };

  // Reading-time translations are a display layer. Selections and AI references
  // must only ever contain the immutable original book text.
  const bookText = (range) => {
    const fragment = range.cloneContents();
    if (fragment.querySelectorAll) {
      for (const node of Array.from(fragment.querySelectorAll("[data-moye-translation]"))) {
        node.remove();
      }
    }
    return fragment.textContent || "";
  };

  // A selection made on the translation layer stands for the original passage it
  // was translated from; the translation runtime owns that mapping and returns
  // null when the selection touches no applied translation.
  const translationOriginalRange = (range) => {
    const resolve = window.moyeTranslations?.originalRange;
    return typeof resolve === "function" ? resolve(range) : null;
  };

  const boundedSelection = () => {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return "";
    // Notes live in a closed shadow root outside body. Their text (including
    // an identical quote) is never a chapter selection or an AI reference.
    let range = selection.getRangeAt(0);
    if (range.collapsed || ![selection.anchorNode, selection.focusNode,
        range.startContainer, range.endContainer].every(isChapterNode)) return "";
    if ([range.startContainer, range.endContainer].some(insideTranslation)) {
      const original = translationOriginalRange(range);
      if (!original) return "";
      range = original;
    }
    // Focusing a notes input can leave the previous body Range in Selection.
    const active = document.activeElement;
    if (active && (!isChapterNode(active) ||
        active.matches("input,textarea,select,[contenteditable='true']"))) return "";
    const value = bookText(range)
      .replace(/\s+/gu, " ")
      .trim();
    if (!value) return "";
    if (encoder.encode(value).byteLength <= MAX_SELECTION_BYTES) return value;
    let low = 0;
    let high = value.length;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      if (encoder.encode(value.slice(0, middle)).byteLength <= MAX_SELECTION_BYTES) {
        low = middle;
      } else {
        high = middle - 1;
      }
    }
    return value.slice(0, low).trim();
  };

  const send = () => {
    window.clearTimeout(timer);
    if (!ownDocument()) return;
    try {
      window.ipc?.postMessage(JSON.stringify({
        type: "selection_changed",
        selected_text: boundedSelection(),
      }));
    } catch {
      // A closed host simply drops the transient selection.
    }
  };

  document.addEventListener("selectionchange", () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(send, 80);
  });
  // focusin is composed, unlike a textarea's own selectionchange in a closed
  // shadow root. Clear the old transient highlight before a queued send fires.
  document.addEventListener("focusin", () => {
    const active = document.activeElement;
    if (active && (!isChapterNode(active) ||
        active.matches("input,textarea,select,[contenteditable='true']"))) send();
  }, true);
  window.addEventListener("pagehide", () => window.clearTimeout(timer), { once: true });
})();
"#;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReaderIpcMessage {
    AnnotationAction {
        #[serde(flatten)]
        action: AnnotationAction,
    },
    SelectionChanged {
        selected_text: String,
    },
    CitationNavigationResult {
        request_id: u64,
        found: bool,
        #[serde(default)]
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReaderWebEvent {
    AnnotationNavigationBlocked,
    AnnotationAction {
        url: String,
        action: AnnotationAction,
    },
    PageLoaded(String),
    ExplainSelection {
        url: String,
        selected_text: String,
    },
    SelectionChanged {
        url: String,
        selected_text: Option<String>,
    },
    CitationNavigationResult {
        url: String,
        request_id: u64,
        found: bool,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReflowableCitationTarget {
    pub(super) spine_index: usize,
    pub(super) href: String,
    pub(super) focus: Option<AiCitationTextFocus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingReaderCitationNavigation {
    request_id: u64,
    spine_index: usize,
    focus: AiCitationTextFocus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReaderResizablePane {
    Navigation,
    Ai,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ReaderPaneLayout {
    navigation_width: f32,
    navigation_collapsed: bool,
    ai_width: f32,
    resizing: Option<ReaderResizablePane>,
}

impl ReaderPaneLayout {
    pub(super) fn new(ai_width: Pixels) -> Self {
        Self {
            navigation_width: READER_NAVIGATION_DEFAULT_WIDTH,
            navigation_collapsed: false,
            ai_width: f32::from(ai_width).clamp(AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_MAX_WIDTH),
            resizing: None,
        }
    }

    pub(super) fn navigation_width(&self) -> Pixels {
        px(self.effective_navigation_width())
    }

    pub(super) fn is_navigation_collapsed(&self) -> bool {
        self.navigation_collapsed
    }

    pub(super) fn set_navigation_collapsed(&mut self, collapsed: bool) -> bool {
        if self.navigation_collapsed == collapsed {
            return false;
        }
        self.navigation_collapsed = collapsed;
        if collapsed {
            self.resizing = self
                .resizing
                .filter(|pane| *pane != ReaderResizablePane::Navigation);
        }
        true
    }

    pub(super) fn toggle_navigation_collapsed(&mut self) -> bool {
        self.set_navigation_collapsed(!self.navigation_collapsed)
    }

    /// The width the navigation pane actually occupies. Collapsing keeps the
    /// resized width so expanding again restores what the reader had chosen.
    fn effective_navigation_width(&self) -> f32 {
        if self.navigation_collapsed {
            READER_NAVIGATION_COLLAPSED_WIDTH
        } else {
            self.navigation_width
        }
    }

    pub(super) fn ai_width(&self) -> Pixels {
        px(self.ai_width)
    }

    pub(super) fn begin_resize(&mut self, pane: ReaderResizablePane) -> bool {
        if self.resizing == Some(pane) {
            return false;
        }
        if pane == ReaderResizablePane::Navigation && self.navigation_collapsed {
            return false;
        }
        self.resizing = Some(pane);
        true
    }

    pub(super) fn finish_resize(&mut self) -> bool {
        self.resizing.take().is_some()
    }

    pub(super) fn is_resizing(&self, pane: ReaderResizablePane) -> bool {
        self.resizing == Some(pane)
    }

    pub(super) fn constrain(&mut self, viewport_width: Pixels, ai_collapsed: bool) -> bool {
        let viewport_width = f32::from(viewport_width);
        if !viewport_width.is_finite() {
            return false;
        }
        let old = (self.navigation_width, self.ai_width);
        self.navigation_width = self
            .navigation_width
            .clamp(READER_NAVIGATION_MIN_WIDTH, READER_NAVIGATION_MAX_WIDTH);
        self.ai_width = self
            .ai_width
            .clamp(AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_MAX_WIDTH);
        if !ai_collapsed {
            self.ai_width = self.ai_width.min(self.maximum_ai_width(viewport_width));
        }
        if !self.navigation_collapsed {
            self.navigation_width = self
                .navigation_width
                .min(self.maximum_navigation_width(viewport_width, ai_collapsed));
        }
        old != (self.navigation_width, self.ai_width)
    }

    pub(super) fn resize_from_pointer(
        &mut self,
        pane: ReaderResizablePane,
        pointer_x: Pixels,
        viewport_width: Pixels,
        ai_collapsed: bool,
    ) -> bool {
        if self.resizing != Some(pane)
            || (pane == ReaderResizablePane::Ai && ai_collapsed)
            || (pane == ReaderResizablePane::Navigation && self.navigation_collapsed)
        {
            return false;
        }
        let pointer_x = f32::from(pointer_x);
        let viewport_width = f32::from(viewport_width);
        if !pointer_x.is_finite() || !viewport_width.is_finite() {
            return false;
        }

        let constrained = self.constrain(px(viewport_width), ai_collapsed);
        let old = (self.navigation_width, self.ai_width);
        let half_handle = READER_PANE_RESIZE_HANDLE_WIDTH / 2.;
        match pane {
            ReaderResizablePane::Navigation => {
                self.navigation_width = (pointer_x - half_handle).clamp(
                    READER_NAVIGATION_MIN_WIDTH,
                    self.maximum_navigation_width(viewport_width, ai_collapsed),
                );
            }
            ReaderResizablePane::Ai => {
                self.ai_width = (viewport_width - pointer_x - half_handle)
                    .clamp(AI_SIDEBAR_MIN_WIDTH, self.maximum_ai_width(viewport_width));
            }
        }
        constrained || old != (self.navigation_width, self.ai_width)
    }

    fn maximum_navigation_width(&self, viewport_width: f32, ai_collapsed: bool) -> f32 {
        let ai_width = if ai_collapsed {
            AI_SIDEBAR_COLLAPSED_WIDTH
        } else {
            self.ai_width
        };
        let handle_count = if ai_collapsed { 1. } else { 2. };
        (viewport_width
            - ai_width
            - READER_CONTENT_MIN_WIDTH
            - READER_PANE_RESIZE_HANDLE_WIDTH * handle_count)
            .clamp(READER_NAVIGATION_MIN_WIDTH, READER_NAVIGATION_MAX_WIDTH)
    }

    fn maximum_ai_width(&self, viewport_width: f32) -> f32 {
        (viewport_width
            - self.effective_navigation_width()
            - READER_CONTENT_MIN_WIDTH
            - READER_PANE_RESIZE_HANDLE_WIDTH * 2.)
            .clamp(AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_MAX_WIDTH)
    }
}

#[derive(Clone, Copy, Debug)]
struct ReaderPaneResizeDrag(ReaderResizablePane);

pub(super) fn reflowable_citation_target(
    source: &AiSourceLink,
    document: &BookDocument,
    opened: &OpenedBook,
) -> std::result::Result<ReflowableCitationTarget, String> {
    let target = source.current_navigation_target(document)?;
    let spine = opened
        .spine
        .get(target.unit_index)
        .ok_or_else(|| "引用对应的阅读章节已不存在".to_string())?;
    let href = match target.source.as_ref() {
        Some(SourceLocator::Epub { href }) => {
            if opened.spine_index_for_url(&OpenedBook::url_for_href(href))
                != Some(target.unit_index)
            {
                return Err("引用的 EPUB 地址与当前内容单元不匹配".to_string());
            }
            href.clone()
        }
        _ => spine.href.clone(),
    };
    Ok(ReflowableCitationTarget {
        spine_index: target.unit_index,
        href,
        focus: target.focus,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReadingProgressWrite {
    Unit { spine_index: usize, unit_id: String },
}

fn reading_progress_for_locator(
    spine_index: usize,
    progress_locators: &[DocumentLocator],
) -> Option<ReadingProgressWrite> {
    progress_locators
        .get(spine_index)
        .map(|locator| ReadingProgressWrite::Unit {
            spine_index,
            unit_id: locator.unit_id.clone(),
        })
}

#[derive(Clone, Debug)]
pub(super) struct ReadingProgressWriteEvent {
    pub(super) progress: ReadingProgressWrite,
    pub(super) error: Option<String>,
    pub(super) projection: Option<ReadingProgressProjection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SequencedReadingProgressWrite {
    sequence: u64,
    progress: ReadingProgressWrite,
}

pub(super) type ReadingProgressProjection = (u64, LibraryStore);
pub(super) type ReadingProgressCompletion =
    std::result::Result<Option<ReadingProgressProjection>, String>;

static NEXT_READING_PROGRESS_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn next_reading_progress_sequence() -> Result<u64, String> {
    NEXT_READING_PROGRESS_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| "阅读进度动作序列已耗尽".to_string())
}

/// Serializes progress mutations through the process-level library service.
///
/// The worker drains queued positions to the most recent one between writes.
/// This keeps rapid page changes from producing an unbounded series of SQLite
/// mutations while preserving FIFO ordering around an already-active write.
pub(super) struct ReadingProgressWriter {
    sender: async_channel::Sender<SequencedReadingProgressWrite>,
    completion: async_channel::Receiver<ReadingProgressCompletion>,
}

impl ReadingProgressWriter {
    pub(super) fn start(
        book_id: String,
        book_incarnation: u64,
        services: Arc<AppServices>,
    ) -> (Self, async_channel::Receiver<ReadingProgressWriteEvent>) {
        let (sender, receiver) = async_channel::unbounded();
        let (event_sender, event_receiver) = async_channel::unbounded();
        let (completion_sender, completion_receiver) = async_channel::bounded(1);
        let runtime = services.runtime();
        let _worker = runtime.spawn(run_reading_progress_writer(
            book_id,
            book_incarnation,
            services,
            receiver,
            event_sender,
            completion_sender,
        ));
        (
            Self {
                sender,
                completion: completion_receiver,
            },
            event_receiver,
        )
    }

    pub(super) fn enqueue(&self, progress: ReadingProgressWrite) -> Result<(), String> {
        let sequence = next_reading_progress_sequence()?;
        self.sender
            .try_send(SequencedReadingProgressWrite { sequence, progress })
            .map_err(|_| "阅读进度后台任务已停止".to_string())
    }

    /// Queues the final position and closes this window's input. The returned
    /// receiver resolves only after the runtime worker has drained the queue
    /// and the last SQLite mutation has completed. Window-close code must wait
    /// for it before removing the final GPUI window.
    pub(super) fn finish(
        self,
        progress: ReadingProgressWrite,
    ) -> (
        Result<(), String>,
        async_channel::Receiver<ReadingProgressCompletion>,
    ) {
        let result = self.enqueue(progress);
        self.sender.close();
        (result, self.completion)
    }
}

async fn run_reading_progress_writer(
    book_id: String,
    book_incarnation: u64,
    services: Arc<AppServices>,
    receiver: async_channel::Receiver<SequencedReadingProgressWrite>,
    event_sender: async_channel::Sender<ReadingProgressWriteEvent>,
    completion_sender: async_channel::Sender<ReadingProgressCompletion>,
) {
    let mut final_result = Ok(None);
    while let Ok(mut write) = receiver.recv().await {
        while let Ok(latest) = receiver.try_recv() {
            write = latest;
        }
        let progress = write.progress;

        let operation_book_id = book_id.clone();
        let operation_progress = progress.clone();
        let operation_sequence = write.sequence;
        let task = services.spawn_library_projected(move |library| match operation_progress {
            ReadingProgressWrite::Unit {
                spine_index,
                unit_id,
            } => library
                .update_progress_at_ordered(
                    &operation_book_id,
                    spine_index,
                    &unit_id,
                    book_incarnation,
                    operation_sequence,
                )
                .map(|_| ()),
        });
        let (error, projection) = match task.await {
            Ok(Ok(mutation)) => (None, Some((mutation.generation, mutation.snapshot))),
            Ok(Err(error)) => (Some(format!("{error:#}")), None),
            Err(error) => (Some(format!("阅读进度任务异常停止：{error}")), None),
        };
        if let Some(error) = error.as_deref() {
            tracing::warn!(
                book_id = %book_id,
                ?progress,
                %error,
                "failed to persist reading progress"
            );
        }
        final_result = match &error {
            Some(error) => Err(error.clone()),
            None => Ok(projection.clone()),
        };
        let _ = event_sender.try_send(ReadingProgressWriteEvent {
            progress,
            error,
            projection,
        });
    }
    let _ = completion_sender.send(final_result).await;
}

/// The root view of a reader window. Each book opens in its own window so
/// several books can be read side by side; the library window stays on screen.
pub struct ReaderApp {
    book_id: String,
    book_incarnation: u64,
    book: OpenedBook,
    current_spine: usize,
    progress_locators: Vec<DocumentLocator>,
    selected_toc: Option<usize>,
    webview: Option<Entity<WebView>>,
    library: LibraryStore,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    ai_sidebar: Entity<AiSidebar>,
    ai_controller: AiSidebarController,
    pane_layout: ReaderPaneLayout,
    ai_sidebar_collapsed: bool,
    search_input: Entity<InputState>,
    search_query: String,
    search_results: Vec<SearchHit>,
    selected_text: Option<String>,
    annotations: ReaderAnnotations,
    translations: ReaderTranslations,
    _annotation_ai_subscription: Subscription,
    _annotation_ai_submit_subscription: Subscription,
    _annotation_ai_failure_subscription: Subscription,
    citation_request_id: u64,
    pending_citation_navigation: Option<PendingReaderCitationNavigation>,
    _search_subscription: Subscription,
    _ai_subscription: Subscription,
    _ai_layout_subscription: Subscription,
    webview_build_gate: ReaderWebViewBuildGate,
    pub(super) page_sync_task: Option<Task<()>>,
    progress_writer: Option<ReadingProgressWriter>,
    progress_sync_task: Option<Task<()>>,
    protocol_gate: Option<ReaderProtocolGate>,
    closing_webview: Option<WeakEntity<WebView>>,
    closing: bool,
    /// URL of the page currently shown, so the translation display preference can
    /// be re-applied live when it changes in the AI settings.
    current_reader_url: Option<String>,
    /// Set when the window closes because its book left the library: the final
    /// reading position belongs to a document that no longer exists.
    closing_for_removed_book: bool,
    progress_close_ready: bool,
    removal_scheduled: bool,
    notice: Option<Notice>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReaderWebViewBuildGate {
    pub(super) building: bool,
    pub(super) close_requested: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ReaderProtocolGate {
    open: Arc<Mutex<bool>>,
    annotation_navigation_blocked: Arc<AtomicBool>,
}

impl ReaderProtocolGate {
    fn new() -> Self {
        Self {
            open: Arc::new(Mutex::new(true)),
            annotation_navigation_blocked: Arc::new(AtomicBool::new(false)),
        }
    }

    fn is_open(&self) -> bool {
        self.open.lock().is_ok_and(|open| *open)
    }

    fn close(&self) {
        if let Ok(mut open) = self.open.lock() {
            *open = false;
        }
    }
}

impl ReaderWebViewBuildGate {
    pub(super) fn new(building: bool) -> Self {
        Self {
            building,
            close_requested: false,
        }
    }

    /// Keep the native parent HWND alive until Wry's asynchronous child build
    /// no longer holds the borrowed raw handle.
    pub(super) fn request_close(&mut self) -> bool {
        if self.building {
            self.close_requested = true;
            true
        } else {
            false
        }
    }

    /// Marks the build settled and consumes a close request that was vetoed.
    pub(super) fn finish(&mut self) -> bool {
        self.building = false;
        std::mem::take(&mut self.close_requested)
    }
}

pub(super) async fn build_reader_webview(
    opened: &OpenedBook,
    current_spine: usize,
    initial_href: Option<&str>,
    parent: &ParentWindowHandle,
    book_id: String,
    services: Arc<AppServices>,
) -> Result<(
    gpui_component::wry::WebView,
    async_channel::Receiver<ReaderWebEvent>,
    ReaderProtocolGate,
)> {
    let initial_url = OpenedBook::navigation_url_for_href(
        initial_href.unwrap_or(&opened.spine[current_spine].href),
    );
    let protocol_book = opened.epub.clone();
    let authorization_book = Arc::clone(&protocol_book);
    let authorization_book_id = book_id.clone();
    let authorizations = services
        .spawn_library_read(move |library| {
            let document = library.document(&authorization_book_id)?;
            ReaderResourceAuthorizations::for_document(&authorization_book, &document)
        })
        .await
        .context("EPUB 资源授权任务异常停止")??;
    ensure!(
        authorizations.book_id() == book_id,
        "EPUB 资源授权属于其它图书"
    );
    let protocol_gate = ReaderProtocolGate::new();
    let protocol_callback_gate = protocol_gate.clone();
    let protocol_services = Arc::clone(&services);
    let protocol_runtime = services.runtime();
    let (event_sender, event_receiver) = async_channel::unbounded::<ReaderWebEvent>();
    let ipc_sender = event_sender.clone();
    let page_sender = event_sender.clone();
    let navigation_sender = event_sender.clone();
    let navigation_gate = protocol_gate.clone();

    let raw_webview = gpui_component::wry::WebViewBuilder::new()
        .with_asynchronous_custom_protocol(
            "epubreader".to_string(),
            move |_, request, responder| {
                if !protocol_callback_gate.is_open() {
                    return;
                }
                if !is_reader_document_uri(request.uri()) {
                    let open = protocol_callback_gate.open.lock().unwrap();
                    if *open {
                        responder.respond(reader_owned_response(reader_error_response(
                            403,
                            "拒绝非阅读器来源",
                        )));
                    }
                    return;
                }
                let request_path = request.uri().path().to_string();
                let range_header = request
                    .headers()
                    .get("Range")
                    .map(|value| value.to_str().unwrap_or("invalid").to_string());
                let authorized = authorizations.asset_for_path(&request_path);
                let task = match authorized {
                    Ok(Some(asset)) => {
                        spawn_reader_media(&protocol_services, book_id.clone(), asset, range_header)
                    }
                    Ok(None) | Err(_) => {
                        let fallback_book = Arc::clone(&protocol_book);
                        protocol_runtime.handle().spawn_blocking(move || {
                            load_resource_with_range(
                                &fallback_book,
                                &request_path,
                                range_header.as_deref(),
                            )
                        })
                    }
                };
                let response_gate = protocol_callback_gate.clone();
                protocol_runtime.spawn(async move {
                    let response = match task.await {
                        Ok(Ok(response)) => reader_resource_response(response),
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "cannot serve EPUB reader resource");
                            reader_error_response(404, "阅读器资源不存在")
                        }
                        Err(error) => {
                            tracing::warn!(%error, "EPUB reader resource task stopped");
                            reader_error_response(404, "阅读器资源不存在")
                        }
                    };
                    let open = response_gate.open.lock().unwrap();
                    if *open {
                        responder.respond(reader_owned_response(response));
                    }
                });
            },
        )
        .with_navigation_handler(move |url| {
            if navigation_gate
                .annotation_navigation_blocked
                .load(Ordering::SeqCst)
            {
                let _ = navigation_sender.try_send(ReaderWebEvent::AnnotationNavigationBlocked);
                return false;
            }
            url.starts_with("epubreader://book/")
                || url.starts_with("http://epubreader.book/")
                || url.starts_with("https://epubreader.book/")
        })
        .with_ipc_handler(move |request| {
            let Some(event) = reader_ipc_event(request.uri(), request.body()) else {
                return;
            };
            let _ = ipc_sender.try_send(event);
        })
        .with_initialization_script(READER_INITIALIZATION_SCRIPT)
        .with_initialization_script(include_str!("reader/annotations.js"))
        .with_initialization_script(include_str!("reader/translations.js"))
        .with_new_window_req_handler(|_, _| gpui_component::wry::NewWindowResponse::Deny)
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, gpui_component::wry::PageLoadEvent::Finished) {
                let _ = page_sender.try_send(ReaderWebEvent::PageLoaded(url));
            }
        })
        .with_download_started_handler(|_, _| false)
        .with_background_color((251, 250, 247, 255))
        .with_hotkeys_zoom(false)
        .with_incognito(true)
        .with_url(initial_url)
        .build_as_child_async(parent)
        .await
        .context("无法创建正文视图")?;

    #[cfg(target_os = "windows")]
    selection_menu::install(
        &raw_webview,
        event_sender,
        Arc::new({
            let gate = protocol_gate.clone();
            move || gate.is_open()
        }),
        selection_menu::reader_document_uri,
        |url, selected_text| ReaderWebEvent::ExplainSelection { url, selected_text },
        MAX_READER_SELECTION_BYTES,
    )
    .context("无法添加阅读器 AI 解释菜单")?;

    Ok((raw_webview, event_receiver, protocol_gate))
}

fn spawn_reader_media(
    services: &Arc<AppServices>,
    book_id: String,
    asset: AuthorizedReaderAsset,
    range_header: Option<String>,
) -> tokio::task::JoinHandle<Result<ResourceResponse>> {
    services.spawn_library_read(move |library| {
        let (media_type, byte_len) = library.asset_metadata(&book_id, &asset.asset_id)?;
        ensure!(
            byte_len == asset.byte_len && same_base_media_type(&media_type, &asset.media_type),
            "EPUB 媒体与阅读器授权快照不匹配"
        );
        let response = MediaService::new((*library).clone()).serve(
            &book_id,
            &asset.asset_id,
            range_header.as_deref(),
        )?;
        Ok(response.into())
    })
}

fn same_base_media_type(left: &str, right: &str) -> bool {
    let base = |value: &str| {
        value
            .split(';')
            .next()
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase()
    };
    base(left) == base(right)
}

fn reader_resource_response(
    response: ResourceResponse,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    let mut builder = gpui_component::wry::http::Response::builder()
        .status(response.status)
        .header("Content-Type", response.mime)
        .header("Content-Length", response.content_length.to_string())
        .header("Content-Security-Policy", READER_CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store");
    if let Some(accept_ranges) = response.accept_ranges {
        builder = builder.header("Accept-Ranges", accept_ranges);
    }
    if let Some(content_range) = response.content_range {
        builder = builder.header("Content-Range", content_range);
    }
    builder
        .body(Cow::Owned(response.bytes))
        .expect("valid EPUB resource response")
}

fn reader_error_response(
    status: u16,
    message: impl Into<String>,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    gpui_component::wry::http::Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Security-Policy", "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(Cow::Owned(message.into().into_bytes()))
        .expect("valid EPUB error response")
}

fn reader_owned_response(
    response: gpui_component::wry::http::Response<Cow<'static, [u8]>>,
) -> gpui_component::wry::http::Response<Vec<u8>> {
    let (parts, body) = response.into_parts();
    gpui_component::wry::http::Response::from_parts(parts, body.into_owned())
}

fn reader_ipc_event(uri: &gpui_component::wry::http::Uri, body: &str) -> Option<ReaderWebEvent> {
    if body.len() > 128 * 1024 || !is_reader_document_uri(uri) {
        return None;
    }
    match serde_json::from_str::<ReaderIpcMessage>(body).ok()? {
        ReaderIpcMessage::AnnotationAction { action } => {
            action.valid().then(|| ReaderWebEvent::AnnotationAction {
                url: uri.to_string(),
                action,
            })
        }
        ReaderIpcMessage::SelectionChanged { selected_text } => {
            let selected_text = normalize_reader_selection(&selected_text)?;
            Some(ReaderWebEvent::SelectionChanged {
                url: uri.to_string(),
                selected_text,
            })
        }
        ReaderIpcMessage::CitationNavigationResult {
            request_id,
            found,
            reason,
        } if (1..=(1_u64 << 53) - 1).contains(&request_id) && reason.len() <= 64 => {
            Some(ReaderWebEvent::CitationNavigationResult {
                url: uri.to_string(),
                request_id,
                found,
                reason,
            })
        }
        ReaderIpcMessage::CitationNavigationResult { .. } => None,
    }
}

fn is_reader_document_uri(uri: &gpui_component::wry::http::Uri) -> bool {
    matches!(
        (uri.scheme_str(), uri.host()),
        (Some("epubreader"), Some("book")) | (Some("http" | "https"), Some("epubreader.book"))
    )
}

fn normalize_reader_selection(value: &str) -> Option<Option<String>> {
    if value.len() > MAX_READER_SELECTION_BYTES {
        return None;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    Some((!normalized.is_empty()).then_some(normalized))
}

/// Reading-window switch order: the same three choices the system configuration
/// offers, with the translation-only mode last because it is the default.
const TRANSLATION_DISPLAY_MODES: [TranslationDisplayMode; 3] = [
    TranslationDisplayMode::Bilingual,
    TranslationDisplayMode::OriginalOnly,
    TranslationDisplayMode::TranslationOnly,
];

fn translation_display_mode(index: usize) -> TranslationDisplayMode {
    TRANSLATION_DISPLAY_MODES
        .get(index)
        .copied()
        .unwrap_or_default()
}

fn translation_display_mode_name(mode: TranslationDisplayMode) -> &'static str {
    match mode {
        TranslationDisplayMode::Bilingual => "双语",
        TranslationDisplayMode::OriginalOnly => "原文",
        TranslationDisplayMode::TranslationOnly => "译文",
    }
}

impl Render for ReaderApp {
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
        let current = self.current_spine;
        let total = self.book.spine.len();
        let chapter_label = self
            .selected_toc
            .and_then(|index| self.book.toc.get(index))
            .map(|item| item.label.clone())
            .unwrap_or_else(|| self.book.spine[current].title.clone());
        let progress = if total == 0 {
            0.0
        } else {
            (current + 1) as f32 / total as f32
        };
        let (status_icon, status_text, status_color) =
            if self.closing || self.webview_build_gate.close_requested {
                (IconName::BookOpen, "正在安全关闭阅读器…", ACCENT)
            } else if self.webview_build_gate.building {
                (IconName::BookOpen, "正在创建正文视图…", ACCENT)
            } else if self.webview.is_some() {
                (IconName::CircleCheck, "正文视图已连接", 0x376441)
            } else {
                (IconName::TriangleAlert, "正文视图不可用", DANGER)
            };
        let reader_summary = format!("第 {} / {} 章 · {chapter_label}", current + 1, total);
        let reader_context = if self.search_query.is_empty() {
            format!("章节进度 · {:.0}%", progress * 100.)
        } else {
            format!("全文检索 · {} 个结果", self.search_results.len())
        };
        let status_bar = render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            reader_summary,
            reader_context,
        );

        let previous_view = view.clone();
        let next_view = view.clone();
        let notes_view = view.clone();
        // The reading window owns the display choice for this book: once chosen it
        // wins over the global preference from the system configuration and is
        // stored per book, and "跟随全局" hands it back to that default. Without a
        // target language nothing is ever translated, so the switch stays hidden
        // instead of offering a no-op.
        let provider_settings = self.services.provider_settings().ok();
        let global_display_mode = provider_settings
            .as_ref()
            .map(|settings| settings.translation_display_mode)
            .unwrap_or_default();
        let follow_global = self.translations.overrides_global().then(|| {
            let follow_view = view.clone();
            Button::new("reader-translation-follow-global")
                .ghost()
                .xsmall()
                .label("跟随全局")
                .tooltip(format!(
                    "恢复跟随系统配置（当前为{}）",
                    translation_display_mode_name(global_display_mode)
                ))
                .on_click(move |_, _, cx| {
                    follow_view.update(cx, |this, cx| this.follow_global_translation_display(cx));
                })
        });
        let display_switch = provider_settings
            .as_ref()
            .is_some_and(|settings| settings.default_language.is_some())
            .then(|| {
                let effective = self.translations.effective();
                TabBar::new("reader-translation-display")
                    .segmented()
                    .small()
                    .selected_index(
                        TRANSLATION_DISPLAY_MODES
                            .iter()
                            .position(|mode| *mode == effective)
                            .unwrap_or(TRANSLATION_DISPLAY_MODES.len() - 1),
                    )
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.set_translation_display(translation_display_mode(*index), cx);
                    }))
                    .children(
                        TRANSLATION_DISPLAY_MODES
                            .into_iter()
                            .map(|mode| Tab::new().label(translation_display_mode_name(mode))),
                    )
            });
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
                        div().h_flex().min_w(px(0.)).gap_3().child(
                            div()
                                .v_flex()
                                .min_w(px(0.))
                                .gap_0p5()
                                .child(
                                    div()
                                        .max_w(px(520.))
                                        .truncate()
                                        .font_semibold()
                                        .text_color(rgb(INK))
                                        .child(self.book.title.clone()),
                                )
                                .child(
                                    div()
                                        .max_w(px(520.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(MUTED))
                                        .child(chapter_label),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .when_some(display_switch, |row, switch| row.child(switch))
                            .when_some(follow_global, |row, button| row.child(button))
                            .child(
                                Button::new("reader-book-notes")
                                    .ghost()
                                    .icon(IconName::Menu)
                                    .label("本书笔记")
                                    .on_click(move |_, _, cx| {
                                        notes_view.update(cx, |this, cx| {
                                            if let Err(error) = open_notes_window(
                                                Arc::clone(&this.services),
                                                this.library_view.clone(),
                                                Some((
                                                    this.book_id.clone(),
                                                    this.book.title.clone(),
                                                )),
                                                cx,
                                            ) {
                                                this.set_error(
                                                    format!("无法打开本书笔记：{error:#}"),
                                                    cx,
                                                );
                                            }
                                        });
                                    }),
                            )
                            .child(div().mr_2().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{} / {}",
                                current + 1,
                                total
                            )))
                            .child(
                                Button::new("previous-chapter")
                                    .ghost()
                                    .icon(IconName::ChevronLeft)
                                    .tooltip("上一章")
                                    .disabled(current == 0)
                                    .on_click(move |_, _, cx| {
                                        previous_view
                                            .update(cx, |this, cx| this.change_chapter(-1, cx));
                                    }),
                            )
                            .child(
                                Button::new("next-chapter")
                                    .ghost()
                                    .icon(IconName::ChevronRight)
                                    .tooltip("下一章")
                                    .disabled(current + 1 >= total)
                                    .on_click(move |_, _, cx| {
                                        next_view.update(cx, |this, cx| this.change_chapter(1, cx));
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
            self.book
                .toc
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let selected = self.selected_toc == Some(index);
                    let navigable = item.href.is_some();
                    let padding = px(18. + item.depth.min(4) as f32 * 14.);
                    if navigable {
                        let toc_view = view.clone();
                        Button::new(("toc", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(px(38.))
                            .justify_start()
                            .pl(padding)
                            .pr_3()
                            .py_2()
                            .rounded(px(7.))
                            .when(selected, |this| this.bg(rgb(ACCENT_SOFT)))
                            .on_click(move |_, _, cx| {
                                toc_view.update(cx, |this, cx| this.go_to_toc(index, cx));
                            })
                            .child(
                                div()
                                    .w_full()
                                    .text_left()
                                    .text_sm()
                                    .line_clamp(2)
                                    .text_color(if selected { rgb(ACCENT) } else { rgb(INK) })
                                    .child(item.label.clone()),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .min_h(px(38.))
                            .pl(padding)
                            .pr_3()
                            .py_2()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(MUTED))
                            .child(div().line_clamp(2).child(item.label.clone()))
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>()
        } else {
            if self.search_results.is_empty() {
                vec![
                    div()
                        .p_4()
                        .text_center()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child("当前图书没有匹配章节")
                        .into_any_element(),
                ]
            } else {
                self.search_results
                    .iter()
                    .enumerate()
                    .map(|(index, hit)| {
                        let result_view = view.clone();
                        let spine_index = hit.spine_index;
                        let selected = spine_index == Some(self.current_spine);
                        Button::new(("reader-search-result", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(px(66.))
                            .justify_start()
                            .px_3()
                            .py_2()
                            .rounded(px(7.))
                            .when(selected, |this| this.bg(rgb(ACCENT_SOFT)))
                            .on_click(move |_, _, cx| {
                                if let Some(spine_index) = spine_index {
                                    result_view.update(cx, |this, cx| {
                                        this.navigate_to_spine(spine_index, cx)
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
                                            .text_color(if selected {
                                                rgb(ACCENT)
                                            } else {
                                                rgb(INK)
                                            })
                                            .child(
                                                hit.chapter_title
                                                    .clone()
                                                    .unwrap_or_else(|| "章节".to_string()),
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
                    .collect::<Vec<_>>()
            }
        };

        let navigation_collapse_view = view.clone();
        let toc = div()
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
                    .pl_5()
                    .pr_2()
                    .justify_between()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(INK))
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .gap_2()
                            .child(Icon::new(IconName::Menu).small())
                            .child(div().truncate().child(if self.search_query.is_empty() {
                                "目录".to_string()
                            } else {
                                format!("搜索结果（{}）", self.search_results.len())
                            })),
                    )
                    .child(
                        Button::new("reader-navigation-collapse")
                            .ghost()
                            .xsmall()
                            .icon(IconName::PanelLeftClose)
                            .tooltip("收起目录")
                            .on_click(move |_, _, cx| {
                                navigation_collapse_view
                                    .update(cx, |this, cx| this.toggle_navigation_collapsed(cx));
                            }),
                    ),
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
                    .id("toc-scroll")
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
                .child(div().text_sm().child("正在加载正文…"))
                .into_any_element(),
        };
        let content = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(rgb(0xe9e5de))
            .child(
                div().size_full().p_3().child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(0xfbfaf7))
                        .child(body),
                ),
            );
        let navigation: gpui::AnyElement = if self.pane_layout.is_navigation_collapsed() {
            self.render_collapsed_navigation(cx)
        } else {
            toc.into_any_element()
        };
        let navigation_resize_handle = (!self.pane_layout.is_navigation_collapsed())
            .then(|| self.render_pane_resize_handle(ReaderResizablePane::Navigation, cx));
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
                    .when_some(navigation_resize_handle, |this, handle| this.child(handle))
                    .child(content)
                    .when_some(ai_resize_handle, |this, handle| this.child(handle))
                    .child(self.ai_sidebar.clone()),
            )
            .child(status_bar)
            .into_any_element()
    }
}

impl ReaderApp {
    pub fn new(
        book_id: String,
        book_incarnation: u64,
        book: OpenedBook,
        current_spine: usize,
        progress_locators: Vec<DocumentLocator>,
        annotation_revisions: (u64, Vec<u64>),
        initial_citation: Option<ReflowableCitationTarget>,
        library: LibraryStore,
        services: Arc<AppServices>,
        library_view: Entity<EpubReaderApp>,
        webview_building: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前图书正文…"));
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let selected_toc = book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(current_spine));
        let current_book = AiBookOption::new(book_id.clone(), book.title.clone());
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
        let references = reader_reference_hints(
            &book_id,
            &book.spine,
            &progress_locators,
            current_spine,
            None,
        );
        ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
        let mut ai_controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book_id.clone()),
        )
        .expect("reader AI scope is valid");
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        let _annotation_ai_subscription =
            cx.subscribe(&ai_sidebar, Self::on_annotation_ai_completed);
        let _annotation_ai_submit_subscription =
            cx.subscribe(&ai_sidebar, Self::on_annotation_ai_submitted);
        let _annotation_ai_failure_subscription =
            cx.subscribe(&ai_sidebar, Self::on_annotation_ai_failed);
        let ai_sidebar_collapsed = ai_sidebar.read(cx).is_collapsed();
        let pane_layout = ReaderPaneLayout::new(ai_sidebar.read(cx).expanded_width());
        let _ai_layout_subscription = cx.observe(&ai_sidebar, |this, sidebar, cx| {
            let collapsed = sidebar.read(cx).is_collapsed();
            if this.ai_sidebar_collapsed != collapsed {
                this.ai_sidebar_collapsed = collapsed;
                cx.notify();
            }
        });
        ai_controller.restore(ai_sidebar.clone(), cx);
        let (progress_writer, progress_events) =
            ReadingProgressWriter::start(book_id.clone(), book_incarnation, Arc::clone(&services));
        let pending_citation_navigation = initial_citation.as_ref().and_then(|target| {
            target
                .focus
                .clone()
                .map(|focus| PendingReaderCitationNavigation {
                    request_id: 1,
                    spine_index: target.spine_index,
                    focus,
                })
        });
        let global_display_mode = services
            .provider_settings()
            .map(|settings| settings.translation_display_mode)
            .unwrap_or_default();
        let mut reader = Self {
            book_id,
            book_incarnation,
            book,
            current_spine,
            progress_locators,
            selected_toc,
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
            selected_text: None,
            annotations: ReaderAnnotations::new(annotation_revisions),
            translations: ReaderTranslations::new(global_display_mode),
            _annotation_ai_subscription,
            _annotation_ai_submit_subscription,
            _annotation_ai_failure_subscription,
            citation_request_id: u64::from(pending_citation_navigation.is_some()),
            pending_citation_navigation,
            _search_subscription,
            _ai_subscription,
            _ai_layout_subscription,
            webview_build_gate: ReaderWebViewBuildGate::new(webview_building),
            page_sync_task: None,
            progress_writer: Some(progress_writer),
            progress_sync_task: None,
            protocol_gate: None,
            closing_webview: None,
            closing: false,
            current_reader_url: None,
            closing_for_removed_book: false,
            progress_close_ready: false,
            removal_scheduled: false,
            notice: None,
        };
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
        reader.start_translation_refresh(cx);
        reader.load_translation_display_override(cx);
        reader
    }

    fn toggle_navigation_collapsed(&mut self, cx: &mut Context<Self>) {
        if self.pane_layout.toggle_navigation_collapsed() {
            cx.notify();
        }
    }

    /// Narrow rail shown while the table of contents is collapsed. The reader
    /// keeps the resized width so expanding restores the previous layout.
    fn render_collapsed_navigation(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        div()
            .v_flex()
            .w(px(READER_NAVIGATION_COLLAPSED_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .border_r_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SIDEBAR))
            .py_3()
            .child(
                Button::new("reader-navigation-expand")
                    .ghost()
                    .icon(IconName::PanelLeftOpen)
                    .tooltip("展开目录")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle_navigation_collapsed(cx));
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .text_color(rgb(ACCENT))
                    .child(Icon::new(IconName::Menu).small()),
            )
            .into_any_element()
    }

    fn render_pane_resize_handle(
        &self,
        pane: ReaderResizablePane,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let drag_view = view.clone();
        let handle_id = match pane {
            ReaderResizablePane::Navigation => "reader-navigation-resize-handle",
            ReaderResizablePane::Ai => "reader-ai-resize-handle",
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
                move |this, event: &DragMoveEvent<ReaderPaneResizeDrag>, window, cx| {
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
                ReaderPaneResizeDrag(pane),
                move |_: &ReaderPaneResizeDrag, _, _, cx| {
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

    fn on_ai_sidebar_event(
        &mut self,
        _sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                if self.closing || self.webview_build_gate.close_requested {
                    self.ai_sidebar.update(cx, |sidebar, cx| {
                        sidebar.cancel_for_window_close(cx);
                    });
                    return;
                }
                self.ai_controller
                    .submit(request.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::Cancel { request_id } => self.ai_controller.cancel(*request_id),
            AiSidebarEvent::NewSession => {
                self.ai_controller.new_session(self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::SwitchSession { thread_id } => {
                self.ai_controller
                    .switch_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::DeleteSession { thread_id } => {
                self.ai_controller
                    .delete_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::ScopeChanged { book_ids } => {
                self.ai_controller
                    .reconcile_scope(book_ids.clone(), self.ai_sidebar.clone(), cx)
            }
            AiSidebarEvent::OpenSource(source) => self.open_ai_source(source.clone(), window, cx),
        }
    }

    fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source.book_id != self.book_id {
            self.library_view.update(cx, |library, cx| {
                library.open_book_at_source(source, None, window, cx);
            });
            return;
        }
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let opened = self.book.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            reflowable_citation_target(&lookup, &document, &opened).map_err(anyhow::Error::msg)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|_window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(target)) => this.navigate_to_citation(target, cx),
                    Ok(Err(error)) => this.set_error(format!("引用已失效：{error:#}"), cx),
                    Err(error) => this.set_error(format!("引用定位任务已停止：{error}"), cx),
                });
            });
        })
        .detach();
    }

    fn on_search_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = input.read(cx).value().trim().to_string();
                self.search_query = query.clone();
                self.search_results.clear();
                if self.notice.as_ref().is_some_and(|notice| {
                    notice.error && notice.text.starts_with("搜索当前图书失败：")
                }) {
                    self.notice = None;
                }
                if !query.is_empty() {
                    match self.library.search_book(&self.book_id, &query, 200) {
                        Ok(results) => self.search_results = results,
                        Err(error) => {
                            self.notice = Some(Notice {
                                text: format!("搜索当前图书失败：{error:#}"),
                                error: true,
                            });
                        }
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(spine_index) =
                    self.search_results.first().and_then(|hit| hit.spine_index)
                {
                    self.navigate_to_spine(spine_index, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn attach_webview(
        &mut self,
        webview: Entity<WebView>,
        protocol_gate: ReaderProtocolGate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.webview = Some(webview);
        self.protocol_gate = Some(protocol_gate);
        if self.webview_build_gate.finish() {
            self.resume_deferred_close(window, cx);
            // Final persistence is asynchronous. Keep the page event receiver
            // alive while the child WebView is retained so a failed close can
            // restore a fully functional reader; successful teardown cancels
            // the task in `release_webview_for_close`.
            return self.webview.is_some();
        }
        self.notice = None;
        cx.notify();
        true
    }

    /// Resumes a close that was vetoed while the child WebView was building.
    fn resume_deferred_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing_for_removed_book {
            self.finish_removal_close(window, cx);
        } else {
            self.schedule_window_removal(window, cx);
        }
    }

    pub(super) fn fail_webview_build(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.webview_build_gate.finish() {
            self.resume_deferred_close(window, cx);
            return;
        }
        self.set_error(error, cx);
    }

    pub(super) fn sync_web_event(&mut self, event: ReaderWebEvent, cx: &mut Context<Self>) {
        match event {
            ReaderWebEvent::AnnotationNavigationBlocked => {
                self.annotation_navigation_blocked(cx);
            }
            ReaderWebEvent::AnnotationAction { url, action } => {
                self.handle_annotation_action(&url, action, cx)
            }
            ReaderWebEvent::PageLoaded(url) => self.sync_loaded_page(&url, cx),
            ReaderWebEvent::ExplainSelection { url, selected_text } => {
                self.explain_selection(&url, &selected_text, cx);
            }
            ReaderWebEvent::SelectionChanged { url, selected_text } => {
                self.sync_selection(&url, selected_text, cx)
            }
            ReaderWebEvent::CitationNavigationResult {
                url,
                request_id,
                found,
                reason,
            } => self.finish_citation_navigation(&url, request_id, found, &reason, cx),
        }
        self.refresh_note_navigation_gate();
    }

    fn sync_loaded_page(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(spine_index) = self.book.spine_index_for_url(url) else {
            return;
        };
        self.current_spine = spine_index;
        self.current_reader_url = Some(url.to_string());
        self.selected_toc = self
            .book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(spine_index));
        self.selected_text = None;
        self.sync_ai_reference(cx);
        self.notice = None;
        self.queue_current_progress(cx);
        self.run_pending_citation_navigation(url, cx);
        self.configure_annotations(cx);
        self.configure_translations(url, cx);
        cx.notify();
    }

    fn run_pending_citation_navigation(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if self.book.spine_index_for_url(url) != Some(pending.spine_index) {
            return;
        }
        let Some(webview) = self.webview.as_ref() else {
            self.pending_citation_navigation = None;
            self.set_error("正文视图尚未就绪，无法定位引用".to_owned(), cx);
            return;
        };
        let script = match citation_dom_navigation_script(
            &pending.focus,
            serde_json::json!({
                "type": "citation_navigation_result",
                "request_id": pending.request_id,
            }),
        ) {
            Ok(script) => script,
            Err(error) => {
                self.pending_citation_navigation = None;
                self.set_error(error, cx);
                return;
            }
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.pending_citation_navigation = None;
            self.set_error(format!("无法执行引用定位：{error}"), cx);
        }
    }

    fn finish_citation_navigation(
        &mut self,
        url: &str,
        request_id: u64,
        found: bool,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if pending.request_id != request_id
            || self.book.spine_index_for_url(url) != Some(pending.spine_index)
        {
            return;
        }
        self.pending_citation_navigation = None;
        if found {
            self.notice = None;
        } else {
            let detail = match reason {
                "ambiguous" => "正文中存在多个相同片段",
                "not_found" | "missing_root" | "empty_text" => "正文中找不到该片段",
                _ => "正文无法映射该文字范围",
            };
            self.set_error(
                format!("引用定位失败：{detail}；未跳转到章节中的其它位置。"),
                cx,
            );
        }
    }

    fn sync_selection(&mut self, url: &str, selected_text: Option<String>, cx: &mut Context<Self>) {
        if self.closing
            || self.book.spine_index_for_url(url) != Some(self.current_spine)
            || self.selected_text == selected_text
        {
            return;
        }
        self.selected_text = selected_text;
        self.sync_ai_reference(cx);
        cx.notify();
    }

    fn explain_selection(&mut self, url: &str, selected_text: &str, cx: &mut Context<Self>) {
        if self.closing || self.webview_build_gate.close_requested {
            return;
        }
        if self.book.spine_index_for_url(url) != Some(self.current_spine) {
            self.set_error(
                "所选文本已失效，请在当前章节重新选择后解释。".to_string(),
                cx,
            );
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
            let script = format!(
                "window.moyeAnnotations?.explainSelection({});",
                serde_json::json!(selected_text)
            );
            if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
                self.set_error(format!("无法读取笔记选区：{error}"), cx);
            }
        }
    }

    fn go_to_toc(&mut self, toc_index: usize, cx: &mut Context<Self>) {
        if self.closing || self.annotation_navigation_blocked(cx) {
            return;
        }
        let Some(item) = self.book.toc.get(toc_index) else {
            return;
        };
        let Some(href) = item.href.as_deref() else {
            return;
        };
        let url = OpenedBook::navigation_url_for_href(href);
        let Some(webview) = &self.webview else {
            return;
        };
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.notice = Some(Notice {
                text: format!("无法打开章节：{error}"),
                error: true,
            });
            cx.notify();
            return;
        }
        self.selected_toc = Some(toc_index);
        if let Some(spine_index) = item.spine_index {
            self.current_spine = spine_index;
            self.queue_current_progress(cx);
        }
        self.selected_text = None;
        self.sync_ai_reference(cx);
        cx.notify();
    }

    fn change_chapter(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let next = self.current_spine as isize + offset;
        if next < 0 || next >= self.book.spine.len() as isize {
            return;
        }
        self.navigate_to_spine(next as usize, cx);
    }

    pub(super) fn navigate_to_spine(&mut self, spine_index: usize, cx: &mut Context<Self>) {
        if self.closing || self.annotation_navigation_blocked(cx) {
            return;
        }
        let Some(spine) = self.book.spine.get(spine_index) else {
            return;
        };
        let url = OpenedBook::navigation_url_for_href(&spine.href);
        let Some(webview) = &self.webview else {
            return;
        };
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.notice = Some(Notice {
                text: format!("无法打开章节：{error}"),
                error: true,
            });
            cx.notify();
            return;
        }
        self.current_spine = spine_index;
        self.selected_toc = self
            .book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(spine_index));
        self.selected_text = None;
        self.sync_ai_reference(cx);
        self.notice = None;
        self.queue_current_progress(cx);
        cx.notify();
    }

    pub(super) fn navigate_to_citation(
        &mut self,
        target: ReflowableCitationTarget,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.annotation_navigation_blocked(cx) {
            return;
        }
        let Some(webview) = self.webview.as_ref() else {
            self.set_error("正文视图尚未就绪，无法打开引用".to_owned(), cx);
            return;
        };
        self.pending_citation_navigation = target.focus.map(|focus| {
            self.citation_request_id = if self.citation_request_id >= (1_u64 << 53) - 1 {
                1
            } else {
                self.citation_request_id + 1
            };
            PendingReaderCitationNavigation {
                request_id: self.citation_request_id,
                spine_index: target.spine_index,
                focus,
            }
        });
        let url = OpenedBook::navigation_url_for_href(&target.href);
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.pending_citation_navigation = None;
            self.set_error(format!("无法打开引用章节：{error}"), cx);
            return;
        }
        self.current_spine = target.spine_index;
        self.selected_toc = self
            .book
            .toc
            .iter()
            .position(|item| item.spine_index == Some(target.spine_index));
        self.selected_text = None;
        self.sync_ai_reference(cx);
        self.notice = None;
        self.queue_current_progress(cx);
        cx.notify();
    }

    fn queue_progress(&mut self, progress: ReadingProgressWrite, cx: &mut Context<Self>) {
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

    fn current_progress_write(&self) -> Option<ReadingProgressWrite> {
        reading_progress_for_locator(self.current_spine, &self.progress_locators)
    }

    fn queue_current_progress(&mut self, cx: &mut Context<Self>) {
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

    fn handle_progress_write_event(
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

    fn sync_ai_reference(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice { text, error: true });
        cx.notify();
    }

    fn start_progress_writer(&mut self, cx: &mut Context<Self>) {
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

    fn release_webview_for_close(&mut self, cx: &mut Context<Self>) -> Option<WeakEntity<WebView>> {
        self.pending_citation_navigation = None;
        self.progress_sync_task.take();
        self.page_sync_task.take();
        self.webview.take().map(|webview| {
            let weak = webview.downgrade();
            webview.update(cx, |webview, _| webview.hide());
            weak
        })
    }

    fn finish_progress_writer(&mut self, cx: &mut Context<Self>) {
        let Some(final_progress) = self.current_progress_write() else {
            self.recover_from_progress_close(
                "当前章节缺少稳定阅读位置，无法确认最终进度已保存。".to_string(),
                cx,
            );
            return;
        };
        let Some(writer) = self.progress_writer.take() else {
            self.recover_from_progress_close("阅读进度后台任务不可用。".to_string(), cx);
            return;
        };
        let (queued, completion) = writer.finish(final_progress);
        let queue_error = queued.err();
        cx.spawn(async move |view, cx| {
            let result = match queue_error {
                Some(error) => Err(error),
                None => completion
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("阅读进度后台任务在完成关闭屏障前停止".to_string())),
            };
            let _ = view.update(cx, |this, cx| match result {
                Ok(projection) => {
                    // Apply the final projection in the same GPUI turn before
                    // close teardown drops the event listener.
                    this.apply_progress_projection(projection, cx);
                    this.complete_progress_close(cx);
                }
                Err(error) => this.recover_from_progress_close(error, cx),
            });
        })
        .detach();
    }

    fn complete_progress_close(&mut self, cx: &mut Context<Self>) {
        self.finish_close(cx);
    }

    /// Drops every resource the window owns. Render removes the native window
    /// once `closing` and `progress_close_ready` are both set.
    fn finish_close(&mut self, cx: &mut Context<Self>) {
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
        self.ai_controller.close();
        if let Some(protocol_gate) = self.protocol_gate.take() {
            protocol_gate.close();
        }
        self.closing_webview = self.release_webview_for_close(cx);
        self.progress_close_ready = true;
        cx.notify();
    }

    fn recover_from_progress_close(&mut self, error: String, cx: &mut Context<Self>) {
        cancel_application_exit(cx);
        tracing::warn!(%error, "final reading progress was not persisted");
        self.progress_sync_task.take();
        self.closing = false;
        self.progress_close_ready = false;
        self.removal_scheduled = false;
        self.start_progress_writer(cx);
        self.notice = Some(Notice {
            text: format!(
                "阅读进度保存失败，窗口已保持打开。请检查磁盘或权限后再次关闭以重试：{error}"
            ),
            error: true,
        });
        cx.notify();
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
        if self.annotation_navigation_blocked(cx) {
            cancel_application_exit(cx);
            return false;
        }
        if self.webview_build_gate.request_close() {
            self.notice = Some(Notice {
                text: "正在安全关闭阅读器…".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        self.schedule_window_removal(window, cx);
        false
    }

    /// Closes this reader because its book left the library.
    ///
    /// Unlike a user-initiated close there is nothing left to persist: the
    /// final position is dropped with its writer, and no failed write may
    /// reopen a window whose document no longer exists.
    pub(super) fn close_for_removed_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing || self.closing_for_removed_book {
            return;
        }
        self.closing_for_removed_book = true;
        if self.webview_build_gate.request_close() {
            return;
        }
        self.finish_removal_close(window, cx);
    }

    fn finish_removal_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        // Dropping the writer stops its runtime worker without queueing a
        // final position for a book that has already been removed.
        self.progress_writer.take();
        self.finish_close(cx);
        window.refresh();
    }
}

fn reader_explanation_reference(
    book_id: &str,
    book: &OpenedBook,
    progress_locators: &[DocumentLocator],
    current_spine: usize,
    url: &str,
    selected_text: &str,
    displayed_text: Option<&str>,
) -> Option<AiReferenceHint> {
    let uri = url.parse().ok()?;
    if !is_reader_document_uri(&uri) || book.spine_index_for_url(url) != Some(current_spine) {
        return None;
    }
    let selected_text = normalize_reader_selection(selected_text)??;
    let mut reference = reader_reference_hints(
        book_id,
        &book.spine,
        progress_locators,
        current_spine,
        Some(&selected_text),
    )
    .into_iter()
    .find(|reference| reference.frozen_text.is_some())?;
    // The reader may have selected text of a reading-time translation. The model
    // then explains what the reader saw, while the frozen quote stays the
    // original behind every citation, note anchor and version check. An empty or
    // oversized report is simply dropped: the explanation falls back to the
    // original passage.
    reference.displayed_text = displayed_text
        .and_then(normalize_reader_selection)
        .flatten()
        .filter(|text| text.len() <= MAX_READER_SELECTION_BYTES);
    Some(reference)
}

fn reader_reference_hints(
    book_id: &str,
    spine: &[moye_epub_editor::reader::SpineItem],
    progress_locators: &[DocumentLocator],
    current_spine: usize,
    selected_text: Option<&str>,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..spine.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != current_spine, *index));
    indices
        .into_iter()
        .filter_map(|index| {
            let spine = spine.get(index)?;
            let locator = progress_locators.get(index)?;
            if locator.book_id != book_id || locator.validate().is_err() {
                return None;
            }
            let mut reference = AiReferenceHint::chapter(
                book_id,
                locator.unit_id.clone(),
                index,
                if index == current_spine {
                    if selected_text.is_some() {
                        format!("当前章节高亮 · {}", spine.title)
                    } else {
                        format!("当前章节 · {}", spine.title)
                    }
                } else {
                    spine.title.clone()
                },
            );
            reference.locator = Some(locator.clone());
            if index == current_spine {
                reference.frozen_text = selected_text.map(str::to_string);
            }
            Some(reference)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_window_switch_maps_to_the_three_display_modes() {
        assert_eq!(TRANSLATION_DISPLAY_MODES.len(), 3);
        assert_eq!(
            translation_display_mode(0),
            TranslationDisplayMode::Bilingual
        );
        assert_eq!(
            translation_display_mode(1),
            TranslationDisplayMode::OriginalOnly
        );
        assert_eq!(
            translation_display_mode(2),
            TranslationDisplayMode::TranslationOnly
        );
        // An unknown index keeps the default instead of panicking.
        assert_eq!(
            translation_display_mode(99),
            TranslationDisplayMode::TranslationOnly
        );
        assert_eq!(
            [
                translation_display_mode_name(TranslationDisplayMode::Bilingual),
                translation_display_mode_name(TranslationDisplayMode::OriginalOnly),
                translation_display_mode_name(TranslationDisplayMode::TranslationOnly),
            ],
            ["双语", "原文", "译文"]
        );
    }

    #[test]
    fn reader_pane_drag_preserves_width_at_the_handle_center() {
        let mut layout = ReaderPaneLayout::new(px(360.));
        assert!(layout.begin_resize(ReaderResizablePane::Navigation));
        assert!(!layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(READER_NAVIGATION_DEFAULT_WIDTH + READER_PANE_RESIZE_HANDLE_WIDTH / 2.),
            px(1180.),
            false,
        ));
        assert_eq!(layout.navigation_width(), px(286.));
        assert!(layout.finish_resize());

        assert!(layout.begin_resize(ReaderResizablePane::Ai));
        assert!(!layout.resize_from_pointer(
            ReaderResizablePane::Ai,
            px(1180. - 360. - READER_PANE_RESIZE_HANDLE_WIDTH / 2.),
            px(1180.),
            false,
        ));
        assert_eq!(layout.ai_width(), px(360.));
    }

    #[test]
    fn reader_pane_drag_clamps_each_sidebar_and_keeps_the_center_readable() {
        let mut layout = ReaderPaneLayout::new(px(360.));
        assert!(layout.begin_resize(ReaderResizablePane::Navigation));
        assert!(layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(0.),
            px(1180.),
            false,
        ));
        assert_eq!(layout.navigation_width(), px(READER_NAVIGATION_MIN_WIDTH));
        assert!(layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(1180.),
            px(1180.),
            false,
        ));
        assert_eq!(layout.navigation_width(), px(READER_NAVIGATION_MAX_WIDTH));
        assert!(layout.finish_resize());

        assert!(layout.begin_resize(ReaderResizablePane::Ai));
        assert!(layout.resize_from_pointer(ReaderResizablePane::Ai, px(1180.), px(1180.), false,));
        assert_eq!(layout.ai_width(), px(AI_SIDEBAR_MIN_WIDTH));
        assert!(layout.resize_from_pointer(ReaderResizablePane::Ai, px(0.), px(1180.), false,));
        assert_eq!(layout.ai_width(), px(368.));
        assert_eq!(
            f32::from(layout.navigation_width())
                + f32::from(layout.ai_width())
                + READER_CONTENT_MIN_WIDTH
                + READER_PANE_RESIZE_HANDLE_WIDTH * 2.,
            1180.
        );
    }

    #[test]
    fn collapsing_navigation_narrows_the_pane_and_frees_room_for_content() {
        let mut layout = ReaderPaneLayout::new(px(360.));
        assert!(layout.constrain(px(900.), false));
        assert_eq!(layout.navigation_width(), px(286.));
        assert_eq!(layout.ai_width(), px(282.));

        assert!(layout.toggle_navigation_collapsed());
        assert!(layout.is_navigation_collapsed());
        assert_eq!(
            layout.navigation_width(),
            px(READER_NAVIGATION_COLLAPSED_WIDTH)
        );
        // The freed width goes first to the reading area, and the AI sidebar may
        // now grow past the limit the expanded navigation used to impose.
        assert!(layout.begin_resize(ReaderResizablePane::Ai));
        assert!(layout.resize_from_pointer(ReaderResizablePane::Ai, px(0.), px(900.), false,));
        assert_eq!(layout.ai_width(), px(524.));
        assert!(layout.finish_resize());

        assert!(layout.toggle_navigation_collapsed());
        assert!(!layout.is_navigation_collapsed());
        assert_eq!(layout.navigation_width(), px(286.));
        assert!(layout.constrain(px(900.), false));
        assert_eq!(layout.ai_width(), px(282.));
    }

    #[test]
    fn collapsed_navigation_rail_is_not_resizable_and_cancels_its_drag() {
        let mut layout = ReaderPaneLayout::new(px(360.));
        assert!(layout.begin_resize(ReaderResizablePane::Navigation));
        assert!(layout.is_resizing(ReaderResizablePane::Navigation));

        assert!(layout.toggle_navigation_collapsed());
        assert!(!layout.is_resizing(ReaderResizablePane::Navigation));
        assert!(!layout.finish_resize());
        assert!(!layout.begin_resize(ReaderResizablePane::Navigation));
        assert!(!layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(600.),
            px(900.),
            false,
        ));

        assert!(layout.set_navigation_collapsed(false));
        assert!(!layout.set_navigation_collapsed(false));
        assert_eq!(
            layout.navigation_width(),
            px(READER_NAVIGATION_DEFAULT_WIDTH)
        );
    }

    #[test]
    fn reader_pane_layout_reconstrains_expanded_widths_after_window_shrinks() {
        let mut layout = ReaderPaneLayout::new(px(AI_SIDEBAR_MAX_WIDTH));
        assert!(layout.constrain(px(900.), false));
        assert_eq!(layout.navigation_width(), px(286.));
        assert_eq!(layout.ai_width(), px(282.));
        assert!(!layout.constrain(px(900.), false));
    }

    #[test]
    fn collapsed_ai_sidebar_leaves_room_to_expand_navigation_until_reopened() {
        let mut layout = ReaderPaneLayout::new(px(360.));
        assert!(layout.begin_resize(ReaderResizablePane::Navigation));
        assert!(layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(900.),
            px(900.),
            true,
        ));
        assert_eq!(layout.navigation_width(), px(READER_NAVIGATION_MAX_WIDTH));
        assert_eq!(layout.ai_width(), px(360.));
        assert!(layout.finish_resize());
        assert!(!layout.constrain(px(900.), true));
        assert!(layout.begin_resize(ReaderResizablePane::Ai));
        assert!(!layout.resize_from_pointer(ReaderResizablePane::Ai, px(400.), px(900.), true,));
        assert_eq!(layout.ai_width(), px(360.));
        assert!(layout.finish_resize());

        assert!(layout.constrain(px(900.), false));
        assert_eq!(layout.navigation_width(), px(288.));
        assert_eq!(layout.ai_width(), px(AI_SIDEBAR_MIN_WIDTH));
        assert_eq!(
            f32::from(layout.navigation_width())
                + f32::from(layout.ai_width())
                + READER_CONTENT_MIN_WIDTH
                + READER_PANE_RESIZE_HANDLE_WIDTH * 2.,
            900.
        );
    }

    #[test]
    fn reader_pane_layout_ignores_moves_without_matching_drag_state() {
        let mut layout = ReaderPaneLayout::new(px(360.));
        assert!(!layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(400.),
            px(1180.),
            false,
        ));
        assert!(layout.begin_resize(ReaderResizablePane::Ai));
        assert!(!layout.resize_from_pointer(
            ReaderResizablePane::Navigation,
            px(400.),
            px(1180.),
            false,
        ));
        assert_eq!(layout.navigation_width(), px(286.));
        assert!(layout.finish_resize());
        assert!(!layout.finish_resize());
    }

    #[test]
    fn webview_build_gate_keeps_parent_alive_until_build_settles() {
        let mut gate = ReaderWebViewBuildGate::new(true);
        assert!(gate.request_close());
        assert!(gate.request_close());
        assert!(gate.finish());
        assert!(!gate.finish());
        assert!(!gate.request_close());

        let mut idle_gate = ReaderWebViewBuildGate::new(false);
        assert!(!idle_gate.request_close());
        assert!(!idle_gate.finish());
    }

    #[test]
    fn progress_writer_updates_the_shared_store_and_flushes_the_final_position() {
        let data_dir = tempfile::tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let book = runtime.block_on(async {
            services
                .spawn_library(|library| {
                    let created = library.create_book("测试图书", "作者")?;
                    let mut editor = moye_epub_editor::editing::DocumentEditor::new(
                        library.document(&created.id)?,
                    )?;
                    for index in 1..=3 {
                        editor.add_unit(
                            moye_epub_editor::editing::NewContentUnit::html_chapter(
                                format!("第 {} 章", index + 1),
                                index,
                            ),
                        )?;
                    }
                    library.apply_document(editor.into_document())
                })
                .await
                .unwrap()
                .unwrap()
        });
        let stale_window_snapshot = services.library_snapshot().unwrap();
        let document = stale_window_snapshot.document(&book.id).unwrap();
        let progress = |spine_index: usize| ReadingProgressWrite::Unit {
            spine_index,
            unit_id: document.units[spine_index].id.clone(),
        };
        let incarnation = stale_window_snapshot
            .progress_incarnation(&book.id)
            .unwrap();
        let (writer, events) =
            ReadingProgressWriter::start(book.id.clone(), incarnation, Arc::clone(&services));

        writer.enqueue(progress(1)).unwrap();
        writer.enqueue(progress(2)).unwrap();
        let final_progress = progress(3);
        let (queued, completion) = writer.finish(final_progress.clone());
        queued.unwrap();

        let completed = runtime.block_on(async {
            let final_projection = completion.recv().await.unwrap().unwrap();
            assert!(final_projection.is_some());
            let mut completed = Vec::new();
            while let Ok(event) = events.recv().await {
                completed.push(event);
            }
            completed
        });
        assert!(!completed.is_empty());
        assert!(completed.iter().all(|event| event.error.is_none()));
        assert!(completed.iter().all(|event| event.projection.is_some()));
        assert_eq!(
            completed.last().map(|event| &event.progress),
            Some(&final_progress)
        );
        assert_eq!(stale_window_snapshot.books()[0].last_spine, 0);
        assert_eq!(
            services
                .library_snapshot()
                .unwrap()
                .book_record(&book.id)
                .unwrap()
                .last_spine,
            3
        );
    }

    #[test]
    fn progress_writer_reports_a_final_persistence_failure() {
        let data_dir = tempfile::tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let book = runtime.block_on(async {
            services
                .spawn_library(|library| library.create_book("测试图书", "作者"))
                .await
                .unwrap()
                .unwrap()
        });
        let unit_id = services
            .library_snapshot()
            .unwrap()
            .document(&book.id)
            .unwrap()
            .units[0]
            .id
            .clone();
        let db_path = services
            .library_snapshot()
            .unwrap()
            .database_path()
            .to_path_buf();
        let incarnation = services
            .library_snapshot()
            .unwrap()
            .progress_incarnation(&book.id)
            .unwrap();
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute("DELETE FROM progress WHERE book_id = ?1", [&book.id])
            .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_progress_write
             BEFORE INSERT ON progress
             BEGIN
                 SELECT RAISE(ABORT, 'test progress write failure');
             END;",
        )
        .unwrap();
        drop(conn);
        let (writer, events) =
            ReadingProgressWriter::start(book.id.clone(), incarnation, Arc::clone(&services));

        let (queued, completion) = writer.finish(ReadingProgressWrite::Unit {
            spine_index: 0,
            unit_id: unit_id.clone(),
        });
        queued.unwrap();

        let (completion_result, event) = runtime.block_on(async {
            let completion_result = completion.recv().await.unwrap();
            (completion_result, events.recv().await.unwrap())
        });
        assert!(
            completion_result
                .as_ref()
                .is_err_and(|error| error.contains("无法保存阅读进度")),
            "unexpected completion result: {completion_result:?}"
        );
        assert_eq!(
            event.progress,
            ReadingProgressWrite::Unit {
                spine_index: 0,
                unit_id,
            }
        );
        assert!(
            event
                .error
                .as_deref()
                .is_some_and(|error| error.contains("无法保存阅读进度"))
        );
    }

    #[test]
    fn progress_writer_discards_a_removed_canonical_position() {
        let data_dir = tempfile::tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let book = runtime.block_on(async {
            services
                .spawn_library(|library| library.create_book("测试图书", "作者"))
                .await
                .unwrap()
                .unwrap()
        });
        let incarnation = services
            .library_snapshot()
            .unwrap()
            .progress_incarnation(&book.id)
            .unwrap();
        let (writer, events) =
            ReadingProgressWriter::start(book.id, incarnation, Arc::clone(&services));

        let (queued, completion) = writer.finish(ReadingProgressWrite::Unit {
            spine_index: 0,
            unit_id: "removed-unit".to_string(),
        });
        queued.unwrap();

        let (completion_result, event) = runtime.block_on(async {
            let completion_result = completion.recv().await.unwrap();
            (completion_result, events.recv().await.unwrap())
        });
        assert!(completion_result.unwrap().is_some());
        assert!(event.error.is_none());
    }

    #[test]
    fn epub_progress_is_derived_from_the_stable_opened_unit_locator() {
        let locators = vec![
            DocumentLocator::unit("book-1", "unit-a"),
            DocumentLocator::unit("book-1", "unit-b"),
        ];
        assert_eq!(
            reading_progress_for_locator(1, &locators),
            Some(ReadingProgressWrite::Unit {
                spine_index: 1,
                unit_id: "unit-b".to_string(),
            })
        );
        assert_eq!(reading_progress_for_locator(2, &locators), None);
    }

    #[test]
    fn ai_reference_options_include_current_then_all_chapters() {
        let spine = vec![
            moye_epub_editor::reader::SpineItem {
                href: "one.xhtml".to_string(),
                title: "One".to_string(),
            },
            moye_epub_editor::reader::SpineItem {
                href: "two.xhtml".to_string(),
                title: "Two".to_string(),
            },
            moye_epub_editor::reader::SpineItem {
                href: "three.xhtml".to_string(),
                title: "Three".to_string(),
            },
        ];
        let locators = vec![
            DocumentLocator::unit("book-a", "unit-1"),
            DocumentLocator::unit("book-a", "unit-2"),
            DocumentLocator::unit("book-a", "unit-3"),
        ];

        let references = reader_reference_hints("book-a", &spine, &locators, 1, None);

        assert_eq!(
            references
                .iter()
                .map(|reference| reference.unit_id.as_str())
                .collect::<Vec<_>>(),
            vec!["unit-2", "unit-1", "unit-3"]
        );
        assert!(references[0].label.starts_with("当前章节"));
        assert_eq!(references[0].locator.as_ref(), Some(&locators[1]));
    }

    #[test]
    fn explanation_reference_requires_current_chapter_and_captures_only_selected_text() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let record = library.create_book("解释测试", "").unwrap();
        let book = OpenedBook::open_bytes(library.reader_epub_bytes(&record.id).unwrap()).unwrap();
        let document = library.document(&record.id).unwrap();
        let locators = document
            .units
            .iter()
            .map(|unit| DocumentLocator::unit(&record.id, &unit.id))
            .collect::<Vec<_>>();
        let url = OpenedBook::navigation_url_for_href(&book.spine[0].href);
        let reference = reader_explanation_reference(
            &record.id,
            &book,
            &locators,
            0,
            &url,
            "  所选\n\t文本  ",
            None,
        )
        .unwrap();
        assert_eq!(reference.frozen_text.as_deref(), Some("所选 文本"));
        assert_eq!(reference.locator.as_ref(), Some(&locators[0]));
        assert_eq!(reference.displayed_text, None);
        // A reading-time translation travels with the reference: the question
        // explains what the reader saw while the frozen quote stays the original.
        let translated = reader_explanation_reference(
            &record.id,
            &book,
            &locators,
            0,
            &url,
            "  所选\n\t文本  ",
            Some("  译文\n\t段落  "),
        )
        .unwrap();
        assert_eq!(translated.frozen_text.as_deref(), Some("所选 文本"));
        assert_eq!(translated.displayed_text.as_deref(), Some("译文 段落"));
        // An empty or oversized report must not reject the explanation.
        for displayed in [" \n\t ", &"x".repeat(MAX_READER_SELECTION_BYTES + 1)] {
            assert_eq!(
                reader_explanation_reference(
                    &record.id,
                    &book,
                    &locators,
                    0,
                    &url,
                    "  所选\n\t文本  ",
                    Some(displayed),
                )
                .unwrap()
                .displayed_text,
                None
            );
        }
        for (spine_index, source_url, selection) in [
            (1, url.as_str(), "过期章节"),
            (0, "https://example.com/chapter.xhtml", "其它来源"),
            (0, "http://epubreader.book/missing.xhtml", "缺失章节"),
            (0, url.as_str(), " \n\t "),
        ] {
            assert!(
                reader_explanation_reference(
                    &record.id,
                    &book,
                    &locators,
                    spine_index,
                    source_url,
                    selection,
                    None,
                )
                .is_none()
            );
        }
        assert!(
            reader_explanation_reference(
                "another-book",
                &book,
                &locators,
                0,
                &url,
                "越权文本",
                None
            )
            .is_none()
        );
    }

    #[test]
    fn reader_selection_is_bounded_normalized_and_frozen_on_the_current_chapter() {
        let spine = vec![moye_epub_editor::reader::SpineItem {
            href: "one.xhtml".to_string(),
            title: "One".to_string(),
        }];
        let locators = vec![DocumentLocator::unit("book-a", "unit-1")];
        let references = reader_reference_hints(
            "book-a",
            &spine,
            &locators,
            0,
            normalize_reader_selection("  selected\n\twords  ")
                .unwrap()
                .as_deref(),
        );

        assert_eq!(references[0].frozen_text.as_deref(), Some("selected words"));
        assert!(references[0].label.starts_with("当前章节高亮"));
        assert!(normalize_reader_selection(&"x".repeat(MAX_READER_SELECTION_BYTES + 1)).is_none());
    }

    #[test]
    fn reader_ipc_accepts_only_typed_messages_from_the_private_origin() {
        let own_uri = "epubreader://book/Text/one.xhtml".parse().unwrap();
        let event = reader_ipc_event(
            &own_uri,
            r#"{"type":"selection_changed","selected_text":"  one\n two  "}"#,
        );
        assert_eq!(
            event,
            Some(ReaderWebEvent::SelectionChanged {
                url: "epubreader://book/Text/one.xhtml".to_string(),
                selected_text: Some("one two".to_string()),
            })
        );

        let forged_uri = "https://example.com/Text/one.xhtml".parse().unwrap();
        assert!(
            reader_ipc_event(
                &forged_uri,
                r#"{"type":"selection_changed","selected_text":"one"}"#
            )
            .is_none()
        );
        assert!(
            reader_ipc_event(&own_uri, r#"{"type":"unknown","selected_text":"one"}"#).is_none()
        );
        assert_eq!(
            reader_ipc_event(
                &own_uri,
                r#"{"type":"citation_navigation_result","request_id":7,"found":false,"reason":"ambiguous"}"#,
            ),
            Some(ReaderWebEvent::CitationNavigationResult {
                url: "epubreader://book/Text/one.xhtml".to_string(),
                request_id: 7,
                found: false,
                reason: "ambiguous".to_string(),
            })
        );
    }

    #[test]
    fn trusted_reader_bridge_only_reports_bounded_selection_changes() {
        for marker in [
            "selectionchange",
            "selection_changed",
            "MAX_SELECTION_BYTES = 32768",
            "window.top === window",
            "epubreader.book",
        ] {
            assert!(
                READER_INITIALIZATION_SCRIPT.contains(marker),
                "missing {marker}"
            );
        }
    }

    #[test]
    fn reader_http_response_preserves_range_contract_and_security_headers() {
        let response = reader_resource_response(ResourceResponse {
            status: 206,
            bytes: b"2345".to_vec(),
            mime: "audio/mpeg".to_string(),
            accept_ranges: Some("bytes"),
            content_length: 4,
            content_range: Some("bytes 2-5/10".to_string()),
        });

        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["Content-Type"], "audio/mpeg");
        assert_eq!(response.headers()["Accept-Ranges"], "bytes");
        assert_eq!(response.headers()["Content-Length"], "4");
        assert_eq!(response.headers()["Content-Range"], "bytes 2-5/10");
        assert_eq!(response.headers()["X-Content-Type-Options"], "nosniff");
        assert_eq!(response.headers()["Content-Security-Policy"], READER_CSP);
        assert_eq!(response.body().as_ref(), b"2345");
    }

    #[test]
    fn reader_protocol_gate_revokes_late_responses() {
        let gate = ReaderProtocolGate::new();
        let callback = gate.clone();
        assert!(callback.is_open());
        gate.close();
        assert!(!callback.is_open());
    }
}
