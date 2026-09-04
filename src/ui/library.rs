use super::*;
use anyhow::ensure;
use std::collections::HashMap;

use moye_epub_editor::{
    agent::PassageRecord,
    document::{BookDocument, BookFormat, BookSource as CanonicalBookSource, DocumentLocator},
    office_com::OfficeCancellation,
    preview::VisualJobState,
    search::{SearchMode, SearchRequest},
    services::OfficeEnhancedPage,
};

const LIBRARY_SEARCH_LIMIT: usize = 200;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
const OFFICE_PREVIEW_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
const OFFICE_PREVIEW_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
struct OfficePreviewRun {
    request_id: u64,
    book_id: String,
    title: String,
    cancellation: OfficeCancellation,
    enabling: bool,
}

struct OfficePreviewLaunch {
    book_id: String,
    title: String,
    pages: Vec<OfficeEnhancedPage>,
}

fn is_office_format_name(format: &str) -> bool {
    matches!(format, "doc" | "docx" | "pptx" | "xlsx")
}

fn is_current_office_preview(
    active: Option<&OfficePreviewRun>,
    request_id: u64,
    book_id: &str,
) -> bool {
    active.is_some_and(|run| run.request_id == request_id && run.book_id == book_id)
}

async fn wait_for_office_enhanced_pages(
    services: Arc<AppServices>,
    book_id: String,
    cancellation: OfficeCancellation,
) -> Result<Vec<OfficeEnhancedPage>> {
    let job_id = services
        .set_office_enhancement_enabled(book_id.clone(), true)
        .await?;
    let coordinator = services.visual_jobs().context("视觉任务协调器不可用")?;
    let started = std::time::Instant::now();
    loop {
        ensure!(!cancellation.is_cancelled(), "Office 增强预览已取消");
        let record = coordinator
            .status(&job_id)
            .await?
            .with_context(|| format!("Office 增强视觉任务不存在：{job_id}"))?;
        match record.state {
            VisualJobState::Succeeded => break,
            VisualJobState::Failed => {
                anyhow::bail!(
                    "Office 增强视觉任务失败：{}",
                    record.error.as_deref().unwrap_or("未知错误")
                )
            }
            VisualJobState::Cancelled => anyhow::bail!("Office 增强预览已取消"),
            VisualJobState::Queued | VisualJobState::Running | VisualJobState::Paused => {}
        }
        ensure!(
            started.elapsed() < OFFICE_PREVIEW_WAIT_TIMEOUT,
            "等待 Office 增强视觉任务完成超时"
        );
        tokio::time::sleep(OFFICE_PREVIEW_POLL_INTERVAL).await;
    }
    ensure!(!cancellation.is_cancelled(), "Office 增强预览已取消");
    services.load_office_enhanced_pages(book_id).await
}

/// Which slice of the library is on screen.
#[derive(Clone, Default, PartialEq, Eq)]
enum GroupFilter {
    #[default]
    All,
    Ungrouped,
    Group(String),
}

impl GroupFilter {
    fn group_id(&self) -> Option<&str> {
        match self {
            Self::Group(id) => Some(id.as_str()),
            _ => None,
        }
    }
}

fn should_apply_library_projection(incoming_generation: u64, applied_generation: u64) -> bool {
    incoming_generation >= applied_generation
}

#[derive(Debug, Default)]
struct LibraryMutationLifecycle {
    pending: usize,
    close_requested: bool,
    close_scheduled: bool,
}

impl LibraryMutationLifecycle {
    fn begin(&mut self) -> bool {
        if self.close_requested {
            return false;
        }
        self.pending = self
            .pending
            .checked_add(1)
            .expect("pending library mutation count overflowed");
        true
    }

    fn finish(&mut self) -> bool {
        self.pending = self
            .pending
            .checked_sub(1)
            .expect("library mutation completed without a matching begin");
        self.take_close_ready()
    }

    fn request_close(&mut self) -> bool {
        self.close_requested = true;
        self.take_close_ready()
    }

    fn close_requested(&self) -> bool {
        self.close_requested
    }

    fn take_close_ready(&mut self) -> bool {
        if self.close_requested && self.pending == 0 && !self.close_scheduled {
            self.close_scheduled = true;
            true
        } else {
            false
        }
    }
}

fn library_ai_scope(library: &LibraryStore, filter: &GroupFilter) -> AiSidebarScope {
    let label = match filter {
        GroupFilter::All => "全部图书".to_string(),
        GroupFilter::Ungrouped => "未分组".to_string(),
        GroupFilter::Group(id) => {
            let path = library
                .group_path(id)
                .into_iter()
                .map(|group| group.name)
                .collect::<Vec<_>>()
                .join(" / ");
            if path.is_empty() {
                "当前分组".to_string()
            } else {
                path
            }
        }
    };
    let subtree = match filter {
        GroupFilter::Group(id) => Some(
            library
                .group_subtree_ids(id)
                .into_iter()
                .collect::<HashSet<_>>(),
        ),
        _ => None,
    };
    let books = library
        .books()
        .iter()
        .filter(|book| match filter {
            GroupFilter::All => true,
            GroupFilter::Ungrouped => book.group_id.is_none(),
            GroupFilter::Group(_) => book
                .group_id
                .as_ref()
                .is_some_and(|group_id| subtree.as_ref().is_some_and(|ids| ids.contains(group_id))),
        })
        .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
        .collect();
    AiSidebarScope::library(label, books)
}

#[derive(Clone)]
enum NameInputAction {
    CreateBook,
    CreateGroup(Option<String>),
    RenameGroup(String),
}

/// Modal dialogs for library and group actions. The pinned `gpui-component`
/// revision predates its `dialog` module, so these are rendered as an in-app
/// overlay.
#[derive(Clone)]
enum GroupModal {
    NameInput {
        title: String,
        confirm_label: String,
        action: NameInputAction,
        input: Entity<InputState>,
        pending_request: Option<u64>,
        error: Option<String>,
    },
    DeleteConfirm {
        group_id: String,
        name: String,
        description: String,
    },
    DeleteBook {
        book_id: String,
        title: String,
        description: String,
    },
    OfficeTrust {
        book_id: String,
        title: String,
        description: String,
    },
}

pub struct EpubReaderApp {
    library: LibraryStore,
    services: Arc<AppServices>,
    library_window: gpui::AnyWindowHandle,
    library_mutations: LibraryMutationLifecycle,
    ai_sidebar: Entity<AiSidebar>,
    ai_controller: AiSidebarController,
    search_input: Entity<InputState>,
    search_query: String,
    search_mode: SearchMode,
    search_results: Vec<SearchHit>,
    search_generation: u64,
    search_results_generation: Option<u64>,
    search_loading: bool,
    search_error: Option<String>,
    _search_subscription: Subscription,
    _ai_subscription: Subscription,
    notice: Option<Notice>,
    is_importing: bool,
    library_projection_generation: u64,
    library_request_generation: u64,
    library_notice_request: Option<u64>,
    group_filter_generation: u64,
    office_preview_generation: u64,
    office_preview: Option<OfficePreviewRun>,
    office_enabled_books: HashSet<String>,
    office_settings_generation: u64,
    _office_state_task: Task<()>,
    group_filter: GroupFilter,
    collapsed_groups: HashSet<String>,
    group_modal: Option<GroupModal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryExportKind {
    Epub,
    Pdf,
    Original,
}

enum OpenReaderPayload {
    Pdf {
        record: BookRecord,
        book_incarnation: u64,
        bytes: Arc<Vec<u8>>,
        pages: Vec<PdfReaderPage>,
        initial_page: u32,
    },
    Reflowable {
        record: BookRecord,
        book_incarnation: u64,
        opened: OpenedBook,
        initial_spine: usize,
        initial_citation: Option<reader::ReflowableCitationTarget>,
        progress_locators: Vec<DocumentLocator>,
    },
}

fn reflowable_progress_locators(
    record: &BookRecord,
    document: &BookDocument,
    opened: &OpenedBook,
) -> Result<Vec<DocumentLocator>> {
    ensure!(document.id == record.id, "阅读文档与图书记录不匹配");
    ensure!(
        document.units.len() == opened.spine.len(),
        "阅读投影与统一文档内容单元数量不一致"
    );
    Ok(document
        .units
        .iter()
        .map(|unit| DocumentLocator::unit(&record.id, &unit.id))
        .collect())
}

struct PdfReaderWindowRequest {
    record: BookRecord,
    book_incarnation: u64,
    bytes: Arc<Vec<u8>>,
    pages: Vec<PdfReaderPage>,
    initial_page: u32,
    library: LibraryStore,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    persist_progress: bool,
    preview_label: String,
}

impl LibraryExportKind {
    fn format(self) -> ExportFormat {
        match self {
            Self::Epub => ExportFormat::Epub,
            Self::Pdf => ExportFormat::Pdf,
            Self::Original => ExportFormat::Original,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Epub => "EPUB",
            Self::Pdf => "PDF",
            Self::Original => "原件",
        }
    }
}

fn export_extension(kind: LibraryExportKind, original_extension: Option<&str>) -> &str {
    match kind {
        LibraryExportKind::Epub => "epub",
        LibraryExportKind::Pdf => "pdf",
        LibraryExportKind::Original => original_extension.unwrap_or("bin"),
    }
}

fn suggested_library_export_filename(
    book: &BookRecord,
    kind: LibraryExportKind,
    original_extension: Option<&str>,
) -> String {
    let epub = suggested_epub_filename(&book.title);
    let stem = epub.strip_suffix(".epub").unwrap_or("book");
    format!("{stem}.{}", export_extension(kind, original_extension))
}

fn canonical_original_extension(document: &BookDocument) -> Result<&'static str> {
    let CanonicalBookSource::Imported { format, .. } = &document.source else {
        anyhow::bail!("这本图书是应用内新建的，没有可导出的原文件；请选择 EPUB 或 PDF")
    };
    Ok(match format {
        BookFormat::Epub => "epub",
        BookFormat::Pdf => "pdf",
        BookFormat::Doc => "doc",
        BookFormat::Docx => "docx",
        BookFormat::Pptx => "pptx",
        BookFormat::Xlsx => "xlsx",
        BookFormat::Mobi => "mobi",
        BookFormat::Azw => "azw",
        BookFormat::Azw3 => "azw3",
    })
}

fn search_mode_index(mode: SearchMode) -> usize {
    match mode {
        SearchMode::Keyword => 0,
        SearchMode::Semantic => 1,
        SearchMode::Hybrid => 2,
    }
}

fn search_mode_from_index(index: usize) -> SearchMode {
    match index {
        0 => SearchMode::Keyword,
        1 => SearchMode::Semantic,
        _ => SearchMode::Hybrid,
    }
}

fn search_mode_label(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Keyword => "关键词",
        SearchMode::Semantic => "语义",
        SearchMode::Hybrid => "混合",
    }
}

fn is_current_search_completion(completed_generation: u64, current_generation: u64) -> bool {
    completed_generation == current_generation
}

fn should_start_library_search(close_requested: bool, query: &str) -> bool {
    !close_requested && !query.trim().is_empty()
}

fn should_sync_library_scope(close_requested: bool) -> bool {
    !close_requested
}

fn name_input_modal_can_close(pending_request: Option<u64>) -> bool {
    pending_request.is_none()
}

fn search_hit_from_passage(passage: PassageRecord, author: String, unit_index: usize) -> SearchHit {
    SearchHit {
        book_id: passage.book_id,
        book_title: passage.book_title,
        author,
        spine_index: Some(unit_index),
        chapter_title: Some(passage.unit_title),
        href: None,
        snippet: passage.text,
        relevance: passage.relevance.unwrap_or_default(),
    }
}

fn resolve_search_hits(
    library: &LibraryStore,
    passages: Vec<PassageRecord>,
) -> Result<Vec<SearchHit>> {
    let mut documents = HashMap::<String, BookDocument>::new();
    let mut hits = Vec::with_capacity(passages.len());
    for passage in passages {
        let book_id = passage.book_id.clone();
        if !documents.contains_key(&book_id) {
            documents.insert(
                book_id.clone(),
                library
                    .document(&book_id)
                    .with_context(|| format!("无法定位搜索结果对应的图书：{book_id}"))?,
            );
        }
        let document = &documents[&book_id];
        let unit_index = document
            .units
            .iter()
            .position(|unit| unit.id == passage.unit_id)
            .with_context(|| format!("搜索结果对应的内容单元已不存在：{}", passage.unit_id))?;
        let author = if document.authors.is_empty() {
            "未知作者".to_string()
        } else {
            document.authors.join("、")
        };
        hits.push(search_hit_from_passage(passage, author, unit_index));
    }
    Ok(hits)
}

fn open_pdf_reader_window(request: PdfReaderWindowRequest, cx: &mut App) {
    let PdfReaderWindowRequest {
        record,
        book_incarnation,
        bytes,
        pages,
        initial_page,
        library,
        services,
        library_view,
        persist_progress,
        preview_label,
    } = request;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1180.), px(820.)),
            cx,
        ))),
        window_min_size: Some(size(px(900.), px(600.))),
        titlebar: Some(TitlebarOptions {
            title: Some(
                if persist_progress {
                    format!("《{}》", record.title)
                } else {
                    format!("《{}》· Microsoft Office 增强预览", record.title)
                }
                .into(),
            ),
            ..Default::default()
        }),
        app_id: Some("dev.moye.epub-editor.pdf-reader".to_string()),
        ..Default::default()
    };
    let _ = cx.open_window(options, move |window, cx| {
        let parent = match ParentWindowHandle::capture(window) {
            Ok(parent) => parent,
            Err(error) => {
                tracing::error!(%error, "cannot capture PDF reader window handle");
                let message = format!("无法创建 PDF 视图：{error:#}");
                let reader = cx.new(|cx| {
                    PdfReaderApp::new(
                        PdfReaderInit {
                            book_id: record.id.clone(),
                            book_incarnation,
                            title: record.title.clone(),
                            pages,
                            initial_page,
                            library: library.clone(),
                            services: Arc::clone(&services),
                            library_view: library_view.clone(),
                            webview_building: false,
                            persist_progress,
                            preview_label: preview_label.clone(),
                        },
                        window,
                        cx,
                    )
                });
                reader.update(cx, |reader, cx| reader.set_error(message, cx));
                // Even without a child WebView, persistent readers still own
                // a progress writer and AI session. Keep the same close veto
                // so final progress persistence and cancellation cannot be
                // bypassed by this early-return error path.
                let close_weak = reader.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    close_weak
                        .update(cx, |reader, cx| reader.handle_window_close(window, cx))
                        .unwrap_or(false)
                });
                return cx.new(|cx| Root::new(reader, window, cx));
            }
        };

        let reader = cx.new(|cx| {
            PdfReaderApp::new(
                PdfReaderInit {
                    book_id: record.id.clone(),
                    book_incarnation,
                    title: record.title.clone(),
                    pages: pages.clone(),
                    initial_page,
                    library: library.clone(),
                    services: Arc::clone(&services),
                    library_view: library_view.clone(),
                    webview_building: true,
                    persist_progress,
                    preview_label: preview_label.clone(),
                },
                window,
                cx,
            )
        });
        let weak = reader.downgrade();
        let title = record.title.clone();
        window
            .spawn(cx, async move |cx| {
                match build_pdf_reader_webview(bytes, initial_page, &parent).await {
                    Ok((raw_webview, ipc_receiver)) => {
                        let _ = cx.update(|window, cx| {
                            let mut keep_open = false;
                            if let Some(reader) = weak.upgrade() {
                                let webview = cx.new(|cx| WebView::new(raw_webview, window, cx));
                                keep_open = reader.update(cx, |reader, cx| {
                                    reader.attach_webview(webview, window, cx)
                                });
                            }
                            if !keep_open {
                                return;
                            }
                            let sync_weak = weak.clone();
                            let sync_task = window.spawn(cx, async move |cx| {
                                while let Ok(message) = ipc_receiver.recv().await {
                                    let mut attempt = 0;
                                    loop {
                                        match sync_weak.update(cx, |reader, cx| {
                                            reader.handle_ipc(message.clone(), cx)
                                        }) {
                                            Ok(()) => break,
                                            Err(error)
                                                if attempt < 3 && sync_weak.upgrade().is_some() =>
                                            {
                                                attempt += 1;
                                                tracing::debug!(
                                                    attempt,
                                                    %error,
                                                    "retrying PDF page synchronization"
                                                );
                                                Timer::after(Duration::from_millis(8)).await;
                                            }
                                            Err(error) => {
                                                if sync_weak.upgrade().is_some() {
                                                    tracing::warn!(
                                                        %error,
                                                        "stopping PDF page synchronization"
                                                    );
                                                }
                                                return;
                                            }
                                        }
                                    }
                                }
                            });
                            let _ =
                                weak.update(cx, |reader, _| reader.ipc_sync_task = Some(sync_task));
                        });
                    }
                    Err(error) => {
                        let error = format!("无法打开《{title}》：{error:#}");
                        let _ = cx.update(|window, cx| {
                            let _ = weak.update(cx, |reader, cx| {
                                reader.fail_webview_build(error, window, cx)
                            });
                        });
                    }
                }
            })
            .detach();

        let close_weak = reader.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            close_weak
                .update(cx, |reader, cx| reader.handle_window_close(window, cx))
                .unwrap_or(false)
        });
        cx.new(|cx| Root::new(reader, window, cx))
    });
}

impl EpubReaderApp {
    pub fn new(
        library: LibraryStore,
        services: Arc<AppServices>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前范围的正文…"));
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let ai_sidebar = cx.new(|cx| {
            AiSidebar::new(
                library_ai_scope(&library, &GroupFilter::All),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        let mut ai_controller =
            AiSidebarController::new(Arc::clone(&services), ChatWindowKind::Library, None)
                .expect("library AI scope is valid");
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        ai_controller.restore(ai_sidebar.clone(), cx);
        let library_projection_generation = services.library_generation();
        let office_book_ids = library
            .books()
            .iter()
            .filter(|book| is_office_format_name(&book.format))
            .map(|book| book.id.clone())
            .collect::<Vec<_>>();
        let office_services = Arc::clone(&services);
        let office_state_task = cx.spawn(async move |view, cx| {
            let mut enabled = HashSet::new();
            for book_id in office_book_ids {
                match office_services
                    .office_enhancement_enabled(book_id.clone())
                    .await
                {
                    Ok(true) => {
                        enabled.insert(book_id);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(%error, %book_id, "cannot load Office enhancement setting");
                    }
                }
            }
            let _ = view.update(cx, |this, cx| {
                if this.office_settings_generation == 0 {
                    this.office_enabled_books = enabled;
                    cx.notify();
                }
            });
        });
        Self {
            library,
            services,
            library_window: gpui::Window::window_handle(window),
            library_mutations: LibraryMutationLifecycle::default(),
            ai_sidebar,
            ai_controller,
            search_input,
            search_query: String::new(),
            search_mode: SearchMode::Hybrid,
            search_results: Vec::new(),
            search_generation: 0,
            search_results_generation: None,
            search_loading: false,
            search_error: None,
            _search_subscription,
            _ai_subscription,
            notice: None,
            is_importing: false,
            library_projection_generation,
            library_request_generation: 0,
            library_notice_request: None,
            group_filter_generation: 0,
            office_preview_generation: 0,
            office_preview: None,
            office_enabled_books: HashSet::new(),
            office_settings_generation: 0,
            _office_state_task: office_state_task,
            group_filter: GroupFilter::All,
            collapsed_groups: HashSet::new(),
            group_modal: None,
        }
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
                    Ok(Ok(index)) => this.open_book_at_source(source, Some(index), window, cx),
                    Ok(Err(error)) => this.set_error(format!("引用已失效：{error:#}"), cx),
                    Err(error) => this.set_error(format!("引用定位任务已停止：{error}"), cx),
                });
            });
        })
        .detach();
    }

    fn open_ai_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = open_ai_settings_window(Arc::clone(&self.services), cx) {
            self.set_error(format!("无法打开 AI Provider 设置：{error:#}"), cx);
        }
    }

    fn open_background_jobs(&mut self, cx: &mut Context<Self>) {
        let indices = self.visible_book_indices();
        let books = indices
            .iter()
            .map(|index| {
                let book = &self.library.books()[*index];
                BackgroundJobBook {
                    id: book.id.clone(),
                    title: book.title.clone(),
                }
            })
            .collect();
        let (scope_label, _) = self.group_heading(indices.len());
        if let Err(error) =
            open_background_jobs_window(Arc::clone(&self.services), books, scope_label, cx)
        {
            self.set_error(format!("无法打开后台任务：{error:#}"), cx);
        }
    }

    fn begin_library_request(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        if !self.library_mutations.begin() {
            return None;
        }
        self.library_request_generation = self.library_request_generation.wrapping_add(1).max(1);
        self.library_notice_request = Some(self.library_request_generation);
        self.notice = Some(Notice {
            text: message.into(),
            error: false,
        });
        cx.notify();
        Some(self.library_request_generation)
    }

    fn finish_library_request(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyWindowHandle> {
        let should_close = self.library_mutations.finish();
        cx.notify();
        should_close.then_some(self.library_window)
    }

    fn request_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.library_mutations.close_requested() {
            // Invalidate every in-flight completion before mutation callbacks
            // can refresh the projection. Those callbacks may still finish,
            // but must not revive a search while this window is closing.
            self.search_generation = self.search_generation.wrapping_add(1);
            self.search_loading = false;
            self.search_results_generation = None;
        }
        self.cancel_ai_for_window_close(cx);
        self.group_modal = None;
        let should_close = self.library_mutations.request_close();
        cx.notify();
        should_close
    }

    /// Merges the projection captured under the service mutation lock. Service
    /// generations reflect actual commit order, while `merge_cached_projection`
    /// preserves a newer editor revision already applied to this window.
    fn apply_shared_library_projection(&mut self, generation: u64, snapshot: LibraryStore) -> bool {
        if !should_apply_library_projection(generation, self.library_projection_generation) {
            return false;
        }
        self.library.merge_cached_projection(snapshot);
        self.library_projection_generation = generation;
        self.collapsed_groups
            .retain(|group_id| self.library.group(group_id).is_some());
        if self
            .group_filter
            .group_id()
            .is_some_and(|group_id| self.library.group(group_id).is_none())
        {
            self.group_filter = GroupFilter::All;
            self.group_filter_generation = self.group_filter_generation.wrapping_add(1);
        }
        true
    }

    pub(super) fn refresh_after_projected_mutation(
        &mut self,
        generation: u64,
        snapshot: LibraryStore,
        cx: &mut Context<Self>,
    ) {
        if self.apply_shared_library_projection(generation, snapshot) {
            self.sync_ai_scope(cx);
            cx.notify();
        }
    }

    fn set_library_request_error(
        &mut self,
        request_id: u64,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if self.library_notice_request != Some(request_id) {
            tracing::warn!(request_id, %message, "较新的书库操作已开始，仍向用户显示较早操作的失败");
        }
        // A newer request may replace a progress/success notice, but it must
        // never make an independent failed import or mutation disappear.
        self.library_notice_request = Some(request_id);
        self.set_error(message, cx);
    }

    fn finish_name_modal_request(
        &mut self,
        request_id: u64,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(GroupModal::NameInput {
            pending_request,
            error: modal_error,
            ..
        }) = self.group_modal.as_mut()
        else {
            return;
        };
        if *pending_request != Some(request_id) {
            return;
        }
        if error.is_none() {
            self.group_modal = None;
        } else {
            *pending_request = None;
            *modal_error = error;
        }
        cx.notify();
    }

    fn visible_book_ids(&self) -> Vec<String> {
        self.visible_book_indices()
            .into_iter()
            .map(|index| self.library.books()[index].id.clone())
            .collect()
    }

    fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.search_mode == mode {
            return;
        }
        self.search_mode = mode;
        self.start_search(cx);
    }

    fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_results.clear();
        self.search_results_generation = None;
        self.search_loading = false;
        self.search_error = None;
        cx.notify();
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        if self.library_mutations.close_requested() {
            self.search_loading = false;
            cx.notify();
            return;
        }
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        self.search_results.clear();
        self.search_results_generation = None;
        self.search_error = None;

        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.search_loading = false;
            cx.notify();
            return;
        }

        let book_ids = self.visible_book_ids();
        if book_ids.is_empty() {
            self.search_loading = false;
            self.search_results_generation = Some(generation);
            cx.notify();
            return;
        }

        let search = match self.services.search() {
            Ok(search) => search,
            Err(error) => {
                self.search_loading = false;
                self.search_error = Some(format!("无法读取搜索服务：{error:#}"));
                cx.notify();
                return;
            }
        };
        self.search_loading = true;
        let mode = self.search_mode;
        let library = self.library.clone();
        let runtime = self.services.runtime();
        let task = runtime.spawn(async move {
            tokio::time::sleep(SEARCH_DEBOUNCE).await;
            let passages = search
                .search(SearchRequest {
                    query,
                    book_ids,
                    mode,
                    limit: LIBRARY_SEARCH_LIMIT,
                })
                .await?;
            tokio::task::spawn_blocking(move || resolve_search_hits(&library, passages))
                .await
                .context("搜索结果定位任务已停止")?
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let _ = view.update(cx, |this, cx| {
                if !is_current_search_completion(generation, this.search_generation) {
                    return;
                }
                this.search_loading = false;
                match outcome {
                    Ok(Ok(results)) => {
                        this.search_results = results;
                        this.search_results_generation = Some(generation);
                    }
                    Ok(Err(error)) => {
                        this.search_error = Some(format!("搜索失败：{error:#}"));
                    }
                    Err(error) => {
                        this.search_error = Some(format!("搜索任务已停止：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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
                let query = input.read(cx).value().trim().to_string();
                self.search_query = query;
                if self.search_query.is_empty() {
                    self.clear_search(cx);
                } else {
                    self.start_search(cx);
                }
            }
            InputEvent::PressEnter { .. } => {
                if self.search_results_generation == Some(self.search_generation)
                    && let Some(hit) = self.search_results.first().cloned()
                {
                    self.open_search_hit(hit, window, cx);
                }
            }
            _ => {}
        }
    }

    fn import_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_importing {
            return;
        }
        // Construct the native dialog while the GPUI window is available, but
        // show it only after this entity update has returned. Windows file
        // dialogs run a nested message loop; showing one while GPUI owns its
        // App borrow makes every requested frame fail with BorrowMutError.
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title("导入图书或文档")
            .add_filter(
                "支持的图书与文档",
                [
                    "epub", "pdf", "doc", "docx", "pptx", "xlsx", "mobi", "azw", "azw3",
                ],
            )
            .open_single_file();
        self.is_importing = true;
        let Some(request_id) = self.begin_library_request("请选择要导入的图书或文档…", cx)
        else {
            self.is_importing = false;
            return;
        };
        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            let path = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let close_window = view.update_in(cx, |this, _window, cx| {
                        this.is_importing = false;
                        if this.library_notice_request == Some(request_id) {
                            this.library_notice_request = None;
                            this.notice = None;
                        }
                        this.finish_library_request(cx)
                    });
                    if let Ok(Some(window)) = close_window {
                        schedule_library_window_removal(window, cx);
                    }
                    return;
                }
                Err(error) => {
                    let close_window = view.update_in(cx, |this, _window, cx| {
                        this.is_importing = false;
                        this.set_library_request_error(
                            request_id,
                            format!("无法打开文件选择器：{error}"),
                            cx,
                        );
                        this.finish_library_request(cx)
                    });
                    if let Ok(Some(window)) = close_window {
                        schedule_library_window_removal(window, cx);
                    }
                    return;
                }
            };
            let may_import = view.update_in(cx, |this, _window, cx| {
                if this.library_mutations.close_requested() {
                    return false;
                }
                if this.library_notice_request == Some(request_id) {
                    this.notice = Some(Notice {
                        text: "正在检查并导入图书…".to_string(),
                        error: false,
                    });
                }
                cx.notify();
                true
            });
            if !matches!(may_import, Ok(true)) {
                let close_window = view.update_in(cx, |this, _window, cx| {
                    this.is_importing = false;
                    this.finish_library_request(cx)
                });
                if let Ok(Some(window)) = close_window {
                    schedule_library_window_removal(window, cx);
                }
                return;
            };
            let task = services.spawn_library_projected(move |library| library.import(&path));
            let outcome = task.await;
            let close_window = view.update_in(cx, |this, _window, cx| {
                this.is_importing = false;
                match outcome {
                    Ok(Ok(mutation)) => {
                        let message = match &mutation.value {
                            ImportOutcome::Added(book) => format!("《{}》已加入图书库", book.title),
                            ImportOutcome::AlreadyExists(book) => {
                                format!("《{}》已经在图书库中", book.title)
                            }
                        };
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: message,
                                error: false,
                            });
                        }
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("导入失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("导入任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn create_book(&mut self, title: String, cx: &mut Context<Self>) {
        if self.is_importing {
            return;
        }
        self.is_importing = true;
        let Some(request_id) = self.begin_library_request("正在创建可编辑图书…", cx)
        else {
            self.is_importing = false;
            return;
        };
        if let Some(GroupModal::NameInput {
            pending_request,
            error,
            ..
        }) = self.group_modal.as_mut()
        {
            *pending_request = Some(request_id);
            *error = None;
        }
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| library.create_book(&title, ""));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                this.is_importing = false;
                match outcome {
                    Ok(Ok(mutation)) => {
                        let title = mutation.value.title.clone();
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("《{title}》已创建，可从右键菜单进入编辑器"),
                                error: false,
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        let message = format!("新建图书失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("新建图书任务异常停止：{error}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn export_book(
        &mut self,
        book_id: String,
        kind: LibraryExportKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind != LibraryExportKind::Original {
            self.open_export_dialog(book_id, kind, None, window, cx);
            return;
        }

        let library = self.library.clone();
        let lookup_id = book_id.clone();
        self.notice = Some(Notice {
            text: "正在读取原件信息…".to_string(),
            error: false,
        });
        let task = cx.background_spawn(async move {
            let document = library.document(&lookup_id)?;
            canonical_original_extension(&document).map(str::to_owned)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(extension) => {
                        this.notice = None;
                        this.open_export_dialog(book_id, kind, Some(extension), window, cx);
                    }
                    Err(error) => this.set_error(format!("无法导出原件：{error:#}"), cx),
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn open_export_dialog(
        &mut self,
        book_id: String,
        kind: LibraryExportKind,
        original_extension: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        let selection = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出《{}》{}", book.title, kind.label()))
            .set_filename(suggested_library_export_filename(
                &book,
                kind,
                original_extension.as_deref(),
            ))
            .add_filter(
                kind.label(),
                [export_extension(kind, original_extension.as_deref())],
            )
            .save_single_file();
        let title = book.title;
        let library = self.library.clone();
        // As with import, `show` must run outside the entity update that
        // created the dialog so its nested Windows message loop can draw GPUI.
        cx.spawn_in(window, async move |view, cx| {
            let target = match selection.show() {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.notice = Some(Notice {
                            text: format!("无法打开保存文件选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let target_display = target.display().to_string();
            let _ = view.update_in(cx, |this, _window, cx| {
                this.notice = Some(Notice {
                    text: format!("正在导出《{title}》…"),
                    error: false,
                });
                cx.notify();
            });
            let task =
                cx.background_spawn(
                    async move { library.export_as(&book_id, kind.format(), &target) },
                );
            let outcome = task.await;
            let _ = view.update_in(cx, |this, _window, cx| {
                match outcome {
                    Ok(_) => {
                        this.notice = Some(Notice {
                            text: format!("《{title}》已导出到「{target_display}」"),
                            error: false,
                        });
                    }
                    Err(error) => {
                        this.set_error(format!("导出《{title}》失败：{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_book(&mut self, book_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_book_at(book_id, None, window, cx);
    }

    fn open_search_hit(&mut self, hit: SearchHit, window: &mut Window, cx: &mut Context<Self>) {
        self.open_book_at(hit.book_id, hit.spine_index, window, cx);
    }

    fn start_office_preview(
        &mut self,
        book_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            self.set_error("图书已经不存在。".to_string(), cx);
            return;
        };
        if !is_office_format_name(&book.format) {
            self.set_error(
                "该图书格式不支持 Microsoft Office 增强预览。".to_string(),
                cx,
            );
            return;
        }

        self.cancel_office_preview(None, false, cx);
        self.office_settings_generation = self.office_settings_generation.wrapping_add(1).max(1);
        self.office_preview_generation = self.office_preview_generation.wrapping_add(1).max(1);
        let request_id = self.office_preview_generation;
        let cancellation = OfficeCancellation::default();
        self.office_preview = Some(OfficePreviewRun {
            request_id,
            book_id: book_id.clone(),
            title: book.title.clone(),
            cancellation: cancellation.clone(),
            enabling: true,
        });
        self.notice = Some(Notice {
            text: format!(
                "正在生成《{}》的 Microsoft Office 增强页面并写入本地对象存储…可右键该书取消；结构化预览仍可使用。",
                book.title
            ),
            error: false,
        });
        cx.notify();

        let services = Arc::clone(&self.services);
        let runtime = services.runtime();
        let enhancement = runtime.spawn(wait_for_office_enhanced_pages(
            Arc::clone(&services),
            book_id.clone(),
            cancellation,
        ));
        self.await_office_preview(request_id, book_id, true, enhancement, window, cx);
    }

    fn open_persisted_office_preview(
        &mut self,
        book_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            self.set_error("图书已经不存在。".to_string(), cx);
            return;
        };
        self.cancel_office_preview(None, false, cx);
        self.office_preview_generation = self.office_preview_generation.wrapping_add(1).max(1);
        let request_id = self.office_preview_generation;
        let cancellation = OfficeCancellation::default();
        self.office_preview = Some(OfficePreviewRun {
            request_id,
            book_id: book_id.clone(),
            title: book.title.clone(),
            cancellation: cancellation.clone(),
            enabling: false,
        });
        self.notice = Some(Notice {
            text: format!("正在校验并读取《{}》的 Office 增强页面…", book.title),
            error: false,
        });
        cx.notify();

        let services = Arc::clone(&self.services);
        let runtime = services.runtime();
        let load_book_id = book_id.clone();
        let enhancement = runtime.spawn(async move {
            ensure!(!cancellation.is_cancelled(), "Office 增强预览已取消");
            services.load_office_enhanced_pages(load_book_id).await
        });
        self.await_office_preview(request_id, book_id, false, enhancement, window, cx);
    }

    fn await_office_preview(
        &mut self,
        request_id: u64,
        book_id: String,
        generated: bool,
        enhancement: tokio::task::JoinHandle<Result<Vec<OfficeEnhancedPage>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let library = self.library.clone();
        let services = Arc::clone(&self.services);
        let library_view = cx.entity().clone();
        cx.spawn_in(window, async move |view, cx| {
            let outcome = match enhancement.await {
                Ok(outcome) => outcome,
                Err(error) => Err(anyhow::Error::new(error).context("Office 增强预览任务已停止")),
            };
            let launch = match view.update(cx, |this, cx| {
                this.finish_office_preview(request_id, &book_id, generated, outcome, cx)
            }) {
                Ok(launch) => launch,
                Err(_) => return,
            };
            let Some(launch) = launch else {
                return;
            };
            let opened = cx.update(|_, cx| {
                open_office_slides_window(
                    launch.book_id,
                    launch.title,
                    launch.pages,
                    library,
                    services,
                    library_view,
                    cx,
                )
            });
            if let Ok(Err(error)) = opened {
                let _ = view.update(cx, |this, cx| {
                    this.set_error(
                        format!(
                            "无法打开 Microsoft Office 增强预览：{error:#}；结构化预览仍可使用。"
                        ),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    fn finish_office_preview(
        &mut self,
        request_id: u64,
        book_id: &str,
        generated: bool,
        outcome: Result<Vec<OfficeEnhancedPage>>,
        cx: &mut Context<Self>,
    ) -> Option<OfficePreviewLaunch> {
        if !is_current_office_preview(self.office_preview.as_ref(), request_id, book_id) {
            return None;
        }
        let active = self
            .office_preview
            .take()
            .expect("current Office preview was checked above");
        let pages = match outcome {
            Ok(pages) => pages,
            Err(error) => {
                self.notice = Some(Notice {
                    text: format!(
                        "《{}》的 Microsoft Office 增强预览不可用：{error:#}；已保留结构化预览。",
                        active.title
                    ),
                    error: true,
                });
                cx.notify();
                return None;
            }
        };
        if active.enabling {
            self.office_enabled_books.insert(active.book_id.clone());
        }
        self.notice = Some(Notice {
            text: if generated {
                format!(
                    "《{}》的 Microsoft Office 增强页面已持久化并通过完整性校验；原件和阅读进度均未修改。",
                    active.title
                )
            } else {
                format!("已读取《{}》的 Microsoft Office 增强页面。", active.title)
            },
            error: false,
        });
        cx.notify();
        Some(OfficePreviewLaunch {
            book_id: active.book_id,
            title: active.title,
            pages,
        })
    }

    fn cancel_office_preview(
        &mut self,
        book_id: Option<&str>,
        notify: bool,
        cx: &mut Context<Self>,
    ) {
        let should_cancel = self
            .office_preview
            .as_ref()
            .is_some_and(|active| book_id.map_or(true, |book_id| active.book_id == book_id));
        if !should_cancel {
            return;
        }
        let active = self
            .office_preview
            .take()
            .expect("active Office preview was checked above");
        active.cancellation.cancel();
        if active.enabling {
            self.disable_office_enhancement(active.book_id, active.title, notify, cx);
        } else if notify {
            self.notice = Some(Notice {
                text: format!(
                    "已取消《{}》的 Microsoft Office 增强预览；结构化预览仍可使用。",
                    active.title
                ),
                error: false,
            });
            cx.notify();
        }
    }

    fn disable_office_enhancement(
        &mut self,
        book_id: String,
        title: String,
        notify: bool,
        cx: &mut Context<Self>,
    ) {
        self.office_settings_generation = self.office_settings_generation.wrapping_add(1).max(1);
        self.office_enabled_books.remove(&book_id);
        let restore_book_id = book_id.clone();
        let services = Arc::clone(&self.services);
        let runtime = services.runtime();
        let task = runtime.spawn(async move {
            services
                .set_office_enhancement_enabled(book_id.clone(), false)
                .await
                .map(|_| book_id)
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let _ = view.update(cx, |this, cx| match outcome {
                Ok(Ok(book_id)) => {
                    this.office_enabled_books.remove(&book_id);
                    if notify {
                        this.notice = Some(Notice {
                            text: format!(
                                "已禁用《{title}》的 Microsoft Office 增强预览；结构化预览仍可使用。"
                            ),
                            error: false,
                        });
                        cx.notify();
                    }
                }
                Ok(Err(error)) => {
                    this.office_enabled_books.insert(restore_book_id.clone());
                    tracing::warn!(
                        %error,
                        book_id = %restore_book_id,
                        "cannot disable Office enhancement"
                    );
                    if notify {
                        this.set_error(
                            format!("禁用《{title}》的 Office 增强预览失败：{error:#}"),
                            cx,
                        );
                    }
                }
                Err(error) => {
                    this.office_enabled_books.insert(restore_book_id.clone());
                    tracing::warn!(
                        %error,
                        book_id = %restore_book_id,
                        "Office enhancement disable task stopped"
                    );
                    if notify {
                        this.set_error(
                            format!("禁用《{title}》的 Office 增强预览任务异常停止：{error}"),
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn open_book_at(
        &mut self,
        book_id: String,
        requested_spine: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_request(book_id, requested_spine, None, window, cx);
    }

    pub(super) fn open_book_at_source(
        &mut self,
        source: AiSourceLink,
        requested_spine: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_request(
            source.book_id.clone(),
            requested_spine,
            Some(source),
            window,
            cx,
        );
    }

    fn open_book_request(
        &mut self,
        book_id: String,
        requested_spine: Option<usize>,
        source: Option<AiSourceLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_read(move |library| {
            let record = library.book_record(&book_id)?;
            let book_incarnation = library
                .progress_incarnation(&book_id)
                .context("图书阅读实例已失效")?;
            let document = library.document(&book_id)?;
            let source_index = source
                .as_ref()
                .map(|source| {
                    current_canonical_source_unit_index(source, &document)
                        .map_err(anyhow::Error::msg)
                })
                .transpose()?;
            if let (Some(requested), Some(exact)) = (requested_spine, source_index) {
                ensure!(requested == exact, "引用内容单元与请求的阅读位置不匹配");
            }
            if record.format == "pdf" {
                let pages = document
                    .units
                    .into_iter()
                    .enumerate()
                    .map(|(index, unit)| PdfReaderPage {
                        unit_id: Some(unit.id),
                        unit_index: Some(index),
                        title: if unit.title.trim().is_empty() {
                            format!("第 {} 页", index + 1)
                        } else {
                            unit.title
                        },
                        page_number: match unit.source_locator {
                            Some(SourceLocator::PdfPage { page }) => page,
                            _ => index.saturating_add(1) as u32,
                        },
                    })
                    .collect::<Vec<_>>();
                let initial_index = source_index
                    .or(requested_spine)
                    .unwrap_or(record.last_spine)
                    .min(pages.len().saturating_sub(1));
                let mut initial_page = pages
                    .get(initial_index)
                    .map(|page| page.page_number)
                    .unwrap_or(1);
                if let Some(source) = source.as_ref() {
                    match source
                        .validated_locator()
                        .and_then(|locator| locator.source.as_ref())
                    {
                        Some(SourceLocator::PdfPage { page }) => {
                            ensure!(
                                pages.get(initial_index).is_some_and(|candidate| {
                                    candidate.unit_id.as_deref() == Some(source.unit_id.as_str())
                                        && candidate.page_number == *page
                                }),
                                "引用对应的 PDF 页面已失效"
                            );
                            initial_page = *page;
                        }
                        Some(_) => anyhow::bail!("引用使用了 PDF 阅读器不支持的来源定位"),
                        None => {}
                    }
                }
                let bytes = Arc::new(library.source_bytes(&book_id)?);
                return Ok::<_, anyhow::Error>(OpenReaderPayload::Pdf {
                    record,
                    book_incarnation,
                    bytes,
                    pages,
                    initial_page,
                });
            }
            let opened = library
                .reader_epub_bytes(&book_id)
                .and_then(OpenedBook::open_bytes)?;
            let progress_locators = reflowable_progress_locators(&record, &document, &opened)?;
            let initial_citation = source
                .as_ref()
                .map(|source| reader::reflowable_citation_target(source, &document, &opened))
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let initial_spine = source_index
                .or(requested_spine)
                .unwrap_or(record.last_spine)
                .min(opened.spine.len().saturating_sub(1));
            Ok::<_, anyhow::Error>(OpenReaderPayload::Reflowable {
                record,
                book_incarnation,
                opened,
                initial_spine,
                initial_citation,
                progress_locators,
            })
        });
        cx.spawn_in(window, async move |view, cx| {
            match task.await {
                Ok(Ok(OpenReaderPayload::Pdf {
                    record,
                    book_incarnation,
                    bytes,
                    pages,
                    initial_page,
                })) => {
                    let (library, services, library_view) =
                        match view.update(cx, |this, cx| {
                            (
                                this.library.clone(),
                                Arc::clone(&this.services),
                                cx.entity().clone(),
                            )
                        }) {
                            Ok(values) => values,
                            Err(_) => return,
                        };
                    let _ = cx.update(move |_, cx| {
                        open_pdf_reader_window(
                            PdfReaderWindowRequest {
                                record,
                                book_incarnation,
                                bytes,
                                pages,
                                initial_page,
                                library,
                                services,
                                library_view,
                                persist_progress: true,
                                preview_label: "PDF".to_string(),
                            },
                            cx,
                        );
                    });
                }
                Ok(Ok(OpenReaderPayload::Reflowable {
                    record,
                    book_incarnation,
                    opened,
                    initial_spine: current_spine,
                    initial_citation,
                    progress_locators,
                })) => {
                    let title = record.title.clone();
                    let (library, services, library_view) = match view.update(cx, |this, cx| {
                        (
                            this.library.clone(),
                            Arc::clone(&this.services),
                            cx.entity().clone(),
                        )
                    }) {
                        Ok(values) => values,
                        Err(_) => return,
                    };
                    let _ = cx.update(move |_, cx| {
                        let options = WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                                None,
                                size(px(1180.), px(820.)),
                                cx,
                            ))),
                            window_min_size: Some(size(px(900.), px(600.))),
                            titlebar: Some(TitlebarOptions {
                                title: Some(format!("《{}》", record.title).into()),
                                ..Default::default()
                            }),
                            app_id: Some("dev.moye.epub-editor.reader".to_string()),
                            ..Default::default()
                        };
                        let _ = cx.open_window(options, move |window, cx| {
                            let parent = match ParentWindowHandle::capture(window) {
                                Ok(parent) => parent,
                                Err(error) => {
                                    tracing::error!(%error, "cannot capture reader window handle");
                                    let message = format!("无法创建正文视图：{error:#}");
                                    let reader = cx.new(|cx| {
                                        ReaderApp::new(
                                            record.id.clone(),
                                            book_incarnation,
                                            opened,
                                            current_spine,
                                            progress_locators,
                                            initial_citation,
                                            library.clone(),
                                            Arc::clone(&services),
                                            library_view.clone(),
                                            false,
                                            window,
                                            cx,
                                        )
                                    });
                                    let _ = reader.update(cx, |reader, cx| {
                                        reader.set_error(message, cx)
                                    });
                                    // Capturing the parent HWND failed before a
                                    // WebView was created, but this reader still
                                    // has a progress writer and AI session. The
                                    // normal close barrier remains mandatory.
                                    let close_weak = reader.downgrade();
                                    window.on_window_should_close(cx, move |window, cx| {
                                        close_weak
                                            .update(cx, |reader, cx| {
                                                reader.handle_window_close(window, cx)
                                            })
                                            .unwrap_or(false)
                                    });
                                    return cx.new(|cx| Root::new(reader, window, cx));
                                }
                            };

                            let reader = cx.new(|cx| {
                                ReaderApp::new(
                                    record.id.clone(),
                                    book_incarnation,
                                    opened.clone(),
                                    current_spine,
                                    progress_locators.clone(),
                                    initial_citation.clone(),
                                    library.clone(),
                                    Arc::clone(&services),
                                    library_view.clone(),
                                    true,
                                    window,
                                    cx,
                                )
                            });
                            let weak = reader.downgrade();

                            // Build the child WebView against this window's HWND and
                            // attach it once ready.
                            let book = opened.clone();
                            window
                                .spawn(cx, async move |cx| {
                                    match build_reader_webview(
                                        &book,
                                        current_spine,
                                        initial_citation
                                            .as_ref()
                                            .map(|target| target.href.as_str()),
                                        &parent,
                                        record.id.clone(),
                                        Arc::clone(&services),
                                    )
                                    .await
                                    {
                                        Ok((raw_webview, page_receiver, protocol_gate)) => {
                                            let _ = cx.update(|window, cx| {
                                                let mut keep_open = false;
                                                if let Some(reader) = weak.upgrade() {
                                                    let webview = cx.new(|cx| {
                                                        WebView::new(raw_webview, window, cx)
                                                    });
                                                    keep_open = reader.update(cx, |reader, cx| {
                                                        reader.attach_webview(
                                                            webview,
                                                            protocol_gate,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }
                                                if !keep_open {
                                                    return;
                                                }
                                                let sync_weak = weak.clone();
                                                let sync_task =
                                                    window.spawn(cx, async move |cx| {
                                                        while let Ok(event) =
                                                            page_receiver.recv().await
                                                        {
                                                            let mut attempt = 0;
                                                            loop {
                                                                match sync_weak.update(
                                                                    cx,
                                                                    |reader, cx| {
                                                                        reader.sync_web_event(
                                                                            event.clone(), cx,
                                                                        )
                                                                    },
                                                                ) {
                                                                    Ok(()) => break,
                                                                    Err(error)
                                                                        if attempt < 3
                                                                            && sync_weak
                                                                                .upgrade()
                                                                                .is_some() =>
                                                                    {
                                                                        attempt += 1;
                                                                        tracing::debug!(
                                                                            attempt,
                                                                            %error,
                                                                            "retrying EPUB page synchronization"
                                                                        );
                                                                        Timer::after(
                                                                            Duration::from_millis(
                                                                                8,
                                                                            ),
                                                                        )
                                                                        .await;
                                                                    }
                                                                    Err(error) => {
                                                                        if sync_weak
                                                                            .upgrade()
                                                                            .is_some()
                                                                        {
                                                                            tracing::warn!(
                                                                                %error,
                                                                                "stopping EPUB page synchronization"
                                                                            );
                                                                        }
                                                                        return;
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    });
                                                let _ = weak.update(cx, |reader, _| {
                                                    reader.page_sync_task = Some(sync_task)
                                                });
                                            });
                                        }
                                        Err(error) => {
                                            let error = format!("无法打开《{title}》：{error:#}");
                                            let _ = cx.update(|window, cx| {
                                                let _ = weak.update(cx, |reader, cx| {
                                                    reader.fail_webview_build(error, window, cx)
                                                });
                                            });
                                        }
                                    }
                                })
                                .detach();

                            // Persist the final reading position when the window closes.
                            let close_weak = reader.downgrade();
                            window.on_window_should_close(cx, move |window, cx| {
                                close_weak
                                    .update(cx, |reader, cx| {
                                        reader.handle_window_close(window, cx)
                                    })
                                    .unwrap_or(false)
                            });

                            // Wrap in `Root` like the library window: gpui-component
                            // widgets (e.g. `Input`) look up `window.root::<Root>()`
                            // when painting, and would panic without it.
                            cx.new(|cx| Root::new(reader, window, cx))
                        });
                    });
                }
                Ok(Err(error)) => {
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("无法打开图书：{error:#}"), cx);
                    });
                }
                Err(error) => {
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("打开图书后台任务异常停止：{error}"), cx);
                    });
                }
            }
        })
        .detach();
    }

    fn open_book_editor(&mut self, book_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_read(move |library| {
            let record = library.book_record(&book_id)?;
            let document = library.document(&book_id)?;
            let chapters = editor_chapters_from_document(&document)?;
            let cover = library.cover_bytes(&book_id)?.and_then(|bytes| {
                CoverDraft::from_arc(bytes)
                    .map_err(|error| {
                        tracing::warn!(book_id = %record.id, %error, "忽略无法解码的数据库封面");
                    })
                    .ok()
            });
            let initial_chapter_html = chapters
                .first()
                .map(|chapter| chapter.html.clone())
                .unwrap_or_default();
            let initial_chapter_href = chapters
                .first()
                .map(|chapter| chapter.href.clone())
                .unwrap_or_else(|| "chapter.xhtml".to_string());
            let web_state = EditorWebState::new(
                record.id.clone(),
                initial_chapter_href,
                initial_chapter_html,
            );
            web_state.authorize_media(&document)?;
            Ok::<_, anyhow::Error>((record, cover, chapters, document, web_state))
        });

        cx.spawn_in(window, async move |view, cx| {
            let (record, cover, chapters, document, web_state) = match task.await {
                Ok(Ok(editor_data)) => editor_data,
                Ok(Err(error)) => {
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("无法打开编辑器：{error:#}"), cx);
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("编辑器后台任务异常停止：{error}"), cx);
                    });
                    return;
                }
            };
            let (library, library_projection_generation, services) =
                match view.update(cx, |this, _| {
                    (
                        this.library.clone(),
                        this.library_projection_generation,
                        Arc::clone(&this.services),
                    )
                }) {
                    Ok(values) => values,
                    Err(_) => return,
                };
            let library_view = view.clone();
            let _ = cx.update(move |_, cx| {
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1080.), px(740.)),
                        cx,
                    ))),
                    window_min_size: Some(size(px(860.), px(560.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(format!("编辑《{}》", record.title).into()),
                        ..Default::default()
                    }),
                    app_id: Some("dev.moye.epub-editor.editor".to_string()),
                    ..Default::default()
                };
                let _ = cx.open_window(options, move |window, cx| {
                    let initial_chapter_html = chapters
                        .first()
                        .map(|chapter| chapter.html.clone())
                        .unwrap_or_default();
                    let initial_chapter_title = chapters
                        .first()
                        .map(|chapter| chapter.title.clone())
                        .unwrap_or_default();

                    let title_input = cx
                        .new(|cx| InputState::new(window, cx).default_value(record.title.clone()));
                    let author_input = cx
                        .new(|cx| InputState::new(window, cx).default_value(record.author.clone()));
                    let chapter_title_input = cx
                        .new(|cx| InputState::new(window, cx).default_value(initial_chapter_title));
                    let body_input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .multi_line(true)
                            .default_value(initial_chapter_html.clone())
                    });
                    let parent = match ParentWindowHandle::capture(window) {
                        Ok(parent) => parent,
                        Err(error) => {
                            tracing::error!(%error, "cannot capture editor window handle");
                            let editor = cx.new(|cx| {
                                EditorApp::new(
                                    record.id.clone(),
                                    library.clone(),
                                    library_projection_generation,
                                    Arc::clone(&services),
                                    library_view.clone(),
                                    cover.clone(),
                                    title_input,
                                    author_input,
                                    chapter_title_input,
                                    body_input,
                                    chapters.clone(),
                                    document.clone(),
                                    web_state,
                                    false,
                                    window,
                                    cx,
                                )
                            });
                            let _ = editor.update(cx, |editor, _cx| {
                                editor.notice = Some(Notice {
                                    text: format!("无法创建预览视图：{error:#}"),
                                    error: true,
                                });
                            });
                            let close_weak = editor.downgrade();
                            window.on_window_should_close(cx, move |window, cx| {
                                close_weak
                                    .update(cx, |editor, cx| editor.handle_window_close(window, cx))
                                    .unwrap_or(false)
                            });
                            return cx.new(|cx| Root::new(editor, window, cx));
                        }
                    };

                    let editor = cx.new(|cx| {
                        EditorApp::new(
                            record.id.clone(),
                            library.clone(),
                            library_projection_generation,
                            Arc::clone(&services),
                            library_view,
                            cover,
                            title_input,
                            author_input,
                            chapter_title_input,
                            body_input,
                            chapters,
                            document,
                            web_state.clone(),
                            true,
                            window,
                            cx,
                        )
                    });
                    let weak = editor.downgrade();

                    // Build the preview/rich-text webview in the background and
                    // attach it once ready. The source tab needs no webview.
                    window
                        .spawn(cx, async move |cx| {
                            match build_editor_webview(&parent, web_state, Arc::clone(&services))
                                .await
                            {
                                Ok((raw_webview, page_receiver)) => {
                                    let _ = cx.update(|window, cx| {
                                        let mut keep_open = false;
                                        if let Some(editor) = weak.upgrade() {
                                            let webview =
                                                cx.new(|cx| WebView::new(raw_webview, window, cx));
                                            keep_open = editor.update(cx, |editor, cx| {
                                                editor.attach_webview(webview, window, cx)
                                            });
                                        }
                                        if !keep_open {
                                            return;
                                        }
                                        let sync_weak = weak.clone();
                                        let sync_task = window.spawn(cx, async move |cx| {
                                            while let Ok(event) = page_receiver.recv().await {
                                                let _ = cx.update(|window, cx| {
                                                    let _ = sync_weak.update(cx, |editor, cx| {
                                                        editor.apply_web_event(event, window, cx)
                                                    });
                                                });
                                            }
                                        });
                                        let _ = weak.update(cx, |editor, _| {
                                            editor.ipc_sync_task = Some(sync_task)
                                        });
                                    });
                                }
                                Err(error) => {
                                    let error = format!("{error:#}");
                                    let _ = cx.update(|window, cx| {
                                        if let Some(editor) = weak.upgrade() {
                                            let _ = editor.update(cx, |editor, cx| {
                                                editor.fail_webview_build(error, window, cx)
                                            });
                                        }
                                    });
                                }
                            }
                        })
                        .detach();

                    // Rich-text close is vetoed until its explicit snapshot is
                    // acknowledged, saved, and the child WebView is released.
                    let close_weak = editor.downgrade();
                    window.on_window_should_close(cx, move |window, cx| {
                        close_weak
                            .update(cx, |editor, cx| editor.handle_window_close(window, cx))
                            .unwrap_or(false)
                    });

                    // Wrap in `Root` like the library window: gpui-component
                    // widgets (e.g. `Input`) look up `window.root::<Root>()`
                    // when painting and would panic without it.
                    cx.new(|cx| Root::new(editor, window, cx))
                });
            });
        })
        .detach();
    }

    fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice { text, error: true });
        cx.notify();
    }

    fn accent_button(&self, id: impl Into<gpui::ElementId>, cx: &App) -> Button {
        Button::new(id)
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(rgb(ACCENT).into())
                    .foreground(white())
                    .border(rgb(ACCENT).into())
                    .hover(rgb(ACCENT_DARK).into())
                    .active(rgb(ACCENT_DARK).into()),
            )
            .rounded(px(9.))
    }

    fn render_library(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let count = self.library.books().len();
        let mode_view = view.clone();
        let search_modes = TabBar::new("library-search-mode")
            .segmented()
            .small()
            .selected_index(search_mode_index(self.search_mode))
            .on_click(move |index, _, cx| {
                mode_view.update(cx, |this, cx| {
                    this.set_search_mode(search_mode_from_index(*index), cx);
                });
            })
            .child(Tab::new().label("关键词"))
            .child(Tab::new().label("语义"))
            .child(Tab::new().label("混合"));

        let header = div()
            .h_flex()
            .justify_between()
            .gap_6()
            .px_7()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_9()
                            .rounded(px(10.))
                            .bg(rgb(ACCENT))
                            .text_color(white())
                            .child(Icon::new(IconName::BookOpen)),
                    )
                    .child(
                        div()
                            .v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .text_color(rgb(INK))
                                    .child("墨页"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("你的私人图书馆"),
                            ),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .max_w(px(820.))
                    .justify_end()
                    .gap_3()
                    .child(div().flex_none().child(search_modes))
                    .child(
                        div().flex_1().min_w(px(220.)).max_w(px(420.)).child(
                            Input::new(&self.search_input)
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true),
                        ),
                    )
                    .child(
                        Button::new("ai-provider-settings")
                            .ghost()
                            .icon(IconName::Settings2)
                            .label("AI 设置")
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| this.open_ai_settings(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("background-jobs")
                            .ghost()
                            .icon(IconName::LoaderCircle)
                            .label("后台任务")
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| this.open_background_jobs(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("create-book")
                            .ghost()
                            .icon(IconName::Plus)
                            .label("新建图书")
                            .disabled(self.is_importing)
                            .on_click({
                                let view = view.clone();
                                move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_create_book_dialog(window, cx)
                                    });
                                }
                            }),
                    )
                    .child(
                        self.accent_button("import-book", cx)
                            .icon(IconName::Plus)
                            .label(if self.is_importing {
                                "正在导入…"
                            } else {
                                "导入图书"
                            })
                            .disabled(self.is_importing)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.import_book(window, cx));
                            }),
                    ),
            );

        let visible = self.visible_book_indices();
        let searching = !self.search_query.is_empty();
        let (heading, subtitle) = if searching {
            let detail = if self.search_loading {
                format!("{}检索中…", search_mode_label(self.search_mode))
            } else if self.search_error.is_some() {
                format!("{}检索失败", search_mode_label(self.search_mode))
            } else {
                format!(
                    "{}检索 · {} 个结果",
                    search_mode_label(self.search_mode),
                    self.search_results.len()
                )
            };
            (
                "当前范围搜索".to_string(),
                format!("“{}” · {detail}", self.search_query),
            )
        } else {
            self.group_heading(visible.len())
        };

        let title = div()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_3()
            .min_h(px(54.))
            .pb_4()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_0p5()
                    .child(
                        div()
                            .text_2xl()
                            .font_semibold()
                            .text_color(rgb(INK))
                            .child(heading),
                    )
                    .child(div().text_sm().text_color(rgb(MUTED)).child(subtitle)),
            )
            .when(!searching, |this| {
                this.when_some(self.group_filter.group_id(), |this, group_id| {
                    this.child(self.render_group_actions(group_id, cx))
                })
            });

        let content = if searching {
            self.render_library_search_results(cx)
        } else if count == 0 {
            self.render_empty_library(cx)
        } else if visible.is_empty() {
            self.render_empty_group(cx)
        } else {
            self.render_book_grid(&visible, cx)
        };

        div()
            .v_flex()
            .size_full()
            .bg(rgb(PAPER))
            .child(header)
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_5()
                    .p_5()
                    .child(self.render_group_sidebar(cx))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .h_full()
                            .min_w(px(0.))
                            .min_h(px(0.))
                            .gap_4()
                            .p_5()
                            .rounded(px(14.))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(SURFACE))
                            .child(title)
                            .child(content),
                    )
                    .child(self.ai_sidebar.clone()),
            )
            .child(self.render_library_status_bar(visible.len()))
            .when_some(self.notice.as_ref(), |this, notice| {
                this.child(self.render_notice(notice))
            })
            .into_any_element()
    }

    fn render_library_status_bar(&self, visible: usize) -> gpui::AnyElement {
        let (status_icon, status_text, status_color) = if self.search_loading {
            (IconName::Search, "正在检索…", ACCENT)
        } else if self.search_error.is_some() {
            (IconName::TriangleAlert, "检索失败", DANGER)
        } else if self.is_importing {
            (IconName::BookOpen, "正在导入图书…", ACCENT)
        } else {
            (IconName::CircleCheck, "书库已就绪", 0x376441)
        };
        let scope = if !self.search_query.is_empty() {
            format!(
                "{}检索 · {} 个结果",
                search_mode_label(self.search_mode),
                self.search_results.len()
            )
        } else {
            let label = match &self.group_filter {
                GroupFilter::All => "全部图书".to_string(),
                GroupFilter::Ungrouped => "未分组".to_string(),
                GroupFilter::Group(id) => {
                    let path = self
                        .library
                        .group_path(id)
                        .into_iter()
                        .map(|group| group.name)
                        .collect::<Vec<_>>()
                        .join(" / ");
                    if path.is_empty() {
                        "分组".to_string()
                    } else {
                        path
                    }
                }
            };
            format!("当前：{label} · {visible} 本")
        };

        render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            format!(
                "{} 本图书 · {} 个分组",
                self.library.books().len(),
                self.library.groups().len()
            ),
            scope,
        )
    }

    fn group_heading(&self, visible: usize) -> (String, String) {
        match &self.group_filter {
            GroupFilter::All => ("我的图书馆".to_string(), format!("{visible} 本图书")),
            GroupFilter::Ungrouped => ("未分组".to_string(), format!("{visible} 本图书")),
            GroupFilter::Group(id) => {
                let path = self.library.group_path(id);
                let name = path
                    .last()
                    .map(|group| group.name.clone())
                    .unwrap_or_else(|| "分组".to_string());
                let breadcrumb = if path.len() > 1 {
                    let names = path
                        .iter()
                        .map(|group| group.name.clone())
                        .collect::<Vec<_>>()
                        .join(" / ");
                    format!("{names} · {visible} 本图书")
                } else {
                    format!("{visible} 本图书")
                };
                (name, breadcrumb)
            }
        }
    }

    /// Indices into `library.books()` for the currently selected shelf. A group
    /// shows its own books plus everything filed under its descendants.
    fn visible_book_indices(&self) -> Vec<usize> {
        match &self.group_filter {
            GroupFilter::All => (0..self.library.books().len()).collect(),
            GroupFilter::Ungrouped => self
                .library
                .books()
                .iter()
                .enumerate()
                .filter(|(_, book)| book.group_id.is_none())
                .map(|(index, _)| index)
                .collect(),
            GroupFilter::Group(id) => {
                let subtree = self.library.group_subtree_ids(id);
                let ids: HashSet<&str> = subtree.iter().map(String::as_str).collect();
                self.library
                    .books()
                    .iter()
                    .enumerate()
                    .filter(|(_, book)| {
                        book.group_id
                            .as_deref()
                            .is_some_and(|group_id| ids.contains(group_id))
                    })
                    .map(|(index, _)| index)
                    .collect()
            }
        }
    }

    fn render_empty_library(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        div()
            .flex_1()
            .v_flex()
            .items_center()
            .justify_center()
            .pb_6()
            .child(
                div()
                    .v_flex()
                    .items_center()
                    .w(px(420.))
                    .p_8()
                    .gap_3()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PAPER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(56.))
                            .rounded_full()
                            .bg(rgb(ACCENT_SOFT))
                            .text_color(rgb(ACCENT))
                            .child(Icon::new(IconName::BookOpen).large()),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .text_color(rgb(INK))
                            .child("你的书架还是空的"),
                    )
                    .child(
                        div()
                            .max_w(px(330.))
                            .text_center()
                            .text_sm()
                            .line_height(gpui::relative(1.6))
                            .text_color(rgb(MUTED))
                            .child("导入 EPUB、PDF、Office 或 DRM-free Kindle 图书，原件会完整保存在本地。"),
                    )
                    .child(
                        self.accent_button("empty-import", cx)
                            .icon(IconName::Plus)
                            .label(if self.is_importing {
                                "正在导入…"
                            } else {
                                "选择图书文件"
                            })
                            .disabled(self.is_importing)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.import_book(window, cx));
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_library_search_results(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.search_loading {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .pb_6()
                .child(
                    div()
                        .v_flex()
                        .items_center()
                        .w(px(400.))
                        .p_8()
                        .gap_3()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(PAPER))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(56.))
                                .rounded_full()
                                .bg(rgb(ACCENT_SOFT))
                                .text_color(rgb(ACCENT))
                                .child(Icon::new(IconName::Search).large()),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child(format!(
                                    "正在进行{}检索…",
                                    search_mode_label(self.search_mode)
                                )),
                        )
                        .child(
                            div()
                                .max_w(px(330.))
                                .text_center()
                                .text_sm()
                                .line_height(gpui::relative(1.6))
                                .text_color(rgb(MUTED))
                                .child("搜索范围只包含当前分组及其子分组中的图书。"),
                        ),
                )
                .into_any_element();
        }

        if let Some(error) = self.search_error.as_ref() {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .pb_6()
                .child(
                    div()
                        .v_flex()
                        .items_center()
                        .w(px(420.))
                        .p_8()
                        .gap_3()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(rgba(0x9f302c2e))
                        .bg(rgb(0xf7e1df))
                        .child(
                            div()
                                .text_color(rgb(DANGER))
                                .child(Icon::new(IconName::TriangleAlert).large()),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child("无法完成搜索"),
                        )
                        .child(
                            div()
                                .max_w(px(350.))
                                .text_center()
                                .text_sm()
                                .line_height(gpui::relative(1.6))
                                .text_color(rgb(MUTED))
                                .child(error.clone()),
                        ),
                )
                .into_any_element();
        }

        if self.search_results.is_empty() {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .pb_6()
                .child(
                    div()
                        .v_flex()
                        .items_center()
                        .w(px(400.))
                        .p_8()
                        .gap_3()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(PAPER))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(56.))
                                .rounded_full()
                                .bg(rgb(ACCENT_SOFT))
                                .text_color(rgb(ACCENT))
                                .child(Icon::new(IconName::Search).large()),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child("没有找到相关内容"),
                        )
                        .child(
                            div()
                                .max_w(px(320.))
                                .text_center()
                                .text_sm()
                                .line_height(gpui::relative(1.6))
                                .text_color(rgb(MUTED))
                                .child(match self.search_mode {
                                    SearchMode::Keyword => "试试更短的关键词，或检查正文中的拼写。",
                                    SearchMode::Semantic | SearchMode::Hybrid => {
                                        "可换一种表达，或切换到关键词检索。"
                                    }
                                }),
                        ),
                )
                .into_any_element();
        }

        let view = cx.entity().clone();
        let rows = self
            .search_results
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let open_view = view.clone();
                let open_hit = hit.clone();
                let location = hit
                    .chapter_title
                    .as_deref()
                    .map(|chapter| format!("内容 · {chapter}"))
                    .unwrap_or_else(|| "内容位置".to_string());
                Button::new(("library-search-result", index))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .min_h(px(86.))
                    .justify_start()
                    .p_4()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .on_click(move |_, window, cx| {
                        open_view.update(cx, |this, cx| {
                            this.open_search_hit(open_hit.clone(), window, cx)
                        });
                    })
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .w_full()
                            .gap_1()
                            .child(
                                div()
                                    .h_flex()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_semibold()
                                            .text_color(rgb(INK))
                                            .child(hit.book_title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(rgb(ACCENT))
                                            .child(location),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(hit.author.clone()),
                            )
                            .child(
                                div()
                                    .line_clamp(2)
                                    .text_left()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child(hit.snippet.clone()),
                            ),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .id("library-search-scroll")
            .flex_1()
            .min_h(px(0.))
            .pb_8()
            .overflow_y_scrollbar()
            .child(div().v_flex().gap_3().children(rows))
            .into_any_element()
    }

    fn render_book_grid(&self, indices: &[usize], cx: &mut Context<Self>) -> gpui::AnyElement {
        let cards = indices
            .iter()
            .map(|index| self.render_book_card(&self.library.books()[*index], cx))
            .collect::<Vec<_>>();

        div()
            .id("library-scroll")
            .flex_1()
            .min_h(px(0.))
            .pb_8()
            .overflow_y_scrollbar()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_5()
                    .children(cards),
            )
            .into_any_element()
    }

    fn render_book_card(&self, book: &BookRecord, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        let cover_bytes = self.library.cover_bytes_cached(&book.id);
        let cover = if let (Some(bytes), Some(format)) = (
            cover_bytes.as_ref(),
            book.cover_mime.as_deref().and_then(image_format_from_mime),
        ) {
            // Binary cover data is hydrated from object storage before render.
            // The image element is laid out by its own bounds, so a `rounded`
            // style on the parent box would clip nothing — the raw pixels are
            // drawn into the parent's corner area. We have to round the image
            // itself so it stays within the 12px corner radius.
            let image = Arc::new(Image::from_bytes(format, (**bytes).clone()));
            img(image)
                .size_full()
                .rounded(px(10.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(rgb(SIDEBAR))
                .text_color(rgb(ACCENT))
                .child(Icon::new(IconName::BookOpen).large())
                .child(
                    div()
                        .text_center()
                        .text_xs()
                        .font_medium()
                        .child(book.format.to_ascii_uppercase()),
                )
                .into_any_element()
        };

        let open_view = view.clone();
        let open_id = book_id.clone();
        let card_id = SharedString::from(format!("book-card-{book_id}"));

        // Right-click menu: open / move / edit / export / delete. Group entries are collected
        // outside the menu builder (which receives a `Context<PopupMenu>` that
        // has no access to the app entity).
        let mut group_entries = Vec::new();
        self.collect_group_menu_entries(None, 0, &mut group_entries);
        let current_group = book.group_id.clone();
        let office_supported = is_office_format_name(&book.format);
        let office_running = self
            .office_preview
            .as_ref()
            .is_some_and(|active| active.book_id == book.id);
        let office_enabled = self.office_enabled_books.contains(&book.id);
        let office_title = book.title.clone();

        // A plain `div` is used instead of a `Button` so the children stretch
        // to the wrapper's 178px width and we don't inherit any button chrome
        // (padding, hover background, etc.).
        let card = div()
            .id(card_id)
            .v_flex()
            .w_full()
            .gap_2()
            .p_2()
            .rounded(px(14.))
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(PAPER)).border_color(rgb(BORDER)))
            .active(|style| style.bg(rgb(SIDEBAR)))
            .on_click(move |_, window, cx| {
                open_view.update(cx, |this, cx| this.open_book(open_id.clone(), window, cx));
            })
            .context_menu({
                let menu_view = view.clone();
                let menu_book_id = book_id.clone();
                let current_group = current_group.clone();
                let group_entries = group_entries.clone();
                move |mut menu, _window, _cx| {
                    let open_view = menu_view.clone();
                    let open_id = menu_book_id.clone();
                    menu = menu
                        .item(
                            PopupMenuItem::new("打开图书")
                                .icon(Icon::new(IconName::BookOpen))
                                .on_click(move |_, window, cx| {
                                    open_view.update(cx, |this, cx| {
                                        this.open_book(open_id.clone(), window, cx)
                                    });
                                }),
                        )
                        .separator();

                    let ungrouped_view = menu_view.clone();
                    let ungrouped_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("未分组")
                            .checked(current_group.is_none())
                            .on_click(move |_, _, cx| {
                                ungrouped_view.update(cx, |this, cx| {
                                    this.move_book_to_group(ungrouped_book_id.clone(), None, cx);
                                });
                            }),
                    );
                    for (group_id, label, _depth) in &group_entries {
                        let view = menu_view.clone();
                        let book_id = menu_book_id.clone();
                        let group_id = group_id.clone();
                        let selected = current_group.as_deref() == Some(group_id.as_str());
                        menu = menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(selected)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.move_book_to_group(
                                            book_id.clone(),
                                            Some(group_id.clone()),
                                            cx,
                                        );
                                    });
                                }),
                        );
                    }

                    let office_view = menu_view.clone();
                    let office_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(if office_running {
                            "取消 Microsoft Office 增强预览"
                        } else if office_enabled {
                            "打开 Microsoft Office 增强预览"
                        } else {
                            "可选：Microsoft Office 增强预览"
                        })
                        .icon(Icon::new(IconName::BookOpen))
                        .disabled(!office_supported)
                        .on_click(move |_, window, cx| {
                            office_view.update(cx, |this, cx| {
                                if office_running {
                                    this.cancel_office_preview(Some(&office_book_id), true, cx);
                                } else if office_enabled {
                                    this.open_persisted_office_preview(
                                        office_book_id.clone(),
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.open_office_trust_dialog(office_book_id.clone(), cx);
                                }
                            });
                        }),
                    );
                    if office_enabled && !office_running {
                        let disable_view = menu_view.clone();
                        let disable_book_id = menu_book_id.clone();
                        let disable_title = office_title.clone();
                        menu = menu.item(
                            PopupMenuItem::new("禁用 Microsoft Office 增强预览")
                                .icon(Icon::new(IconName::Close))
                                .on_click(move |_, _, cx| {
                                    disable_view.update(cx, |this, cx| {
                                        this.disable_office_enhancement(
                                            disable_book_id.clone(),
                                            disable_title.clone(),
                                            true,
                                            cx,
                                        );
                                    });
                                }),
                        );
                    }

                    let edit_view = menu_view.clone();
                    let edit_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("编辑图书")
                            .icon(Icon::new(IconName::Settings2))
                            .on_click(move |_, window, cx| {
                                edit_view.update(cx, |this, cx| {
                                    this.open_book_editor(edit_book_id.clone(), window, cx);
                                });
                            }),
                    );

                    for (label, kind) in [
                        ("导出 EPUB", LibraryExportKind::Epub),
                        ("导出 PDF", LibraryExportKind::Pdf),
                        ("导出原件", LibraryExportKind::Original),
                    ] {
                        let export_view = menu_view.clone();
                        let export_book_id = menu_book_id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .icon(Icon::new(IconName::ExternalLink))
                                .on_click(move |_, window, cx| {
                                    export_view.update(cx, |this, cx| {
                                        this.export_book(export_book_id.clone(), kind, window, cx);
                                    });
                                }),
                        );
                    }

                    let delete_view = menu_view.clone();
                    let delete_book_id = menu_book_id.clone();
                    menu.separator()
                        .item(
                            PopupMenuItem::new("删除图书")
                                .icon(Icon::new(IconName::Delete).text_color(rgb(DANGER)))
                                .on_click(move |_, window, cx| {
                                    delete_view.update(cx, |this, cx| {
                                        this.open_delete_book_dialog(
                                            delete_book_id.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                        .max_h(px(320.))
                        .scrollable(true)
                }
            })
            .child(
                div()
                    .flex()
                    .w_full()
                    .h(px(252.))
                    .overflow_hidden()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(cover),
            )
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap_1()
                    .text_left()
                    .child(
                        div()
                            .w_full()
                            .text_sm()
                            .font_semibold()
                            .line_clamp(2)
                            .text_color(rgb(INK))
                            .child(book.title.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .line_clamp(1)
                            .text_color(rgb(MUTED))
                            .child(book.author.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_1p5()
                            .py_0p5()
                            .rounded(px(4.))
                            .bg(rgb(ACCENT_SOFT))
                            .text_xs()
                            .font_medium()
                            .text_color(rgb(ACCENT_DARK))
                            .child(book.format.to_ascii_uppercase()),
                    )
                    .when_some(
                        book.group_id
                            .as_deref()
                            .and_then(|group_id| self.library.group(group_id)),
                        |this, group| {
                            this.child(
                                div()
                                    .h_flex()
                                    .w_full()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(rgb(ACCENT))
                                    .child(
                                        Icon::new(IconName::Folder).small().text_color(rgb(ACCENT)),
                                    )
                                    .child(
                                        div().min_w(px(0.)).truncate().child(group.name.clone()),
                                    ),
                            )
                        },
                    ),
            );

        div()
            .relative()
            .w(px(190.))
            .h_auto()
            .flex_none()
            .child(card)
            .child(
                div()
                    .id(SharedString::from(format!("book-group-overlay-{book_id}")))
                    .absolute()
                    .top_2()
                    .right_2()
                    // The folder button's popover is opened by the mouse-down that
                    // bubbles up to the card underneath it. Stop the propagation at
                    // this overlay so the click doesn't also open the book.
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(self.render_book_group_menu(book, cx)),
            )
            .into_any_element()
    }

    // Note: the previous version of this card relied on the outer wrapper being
    // a `relative` block for the floating folder button, and the Button taking
    // `w_full()`. The `items_start` on the Button stops the inner v_flex from
    // stretching, so the cover row and the text column collapsed onto whatever
    // their content sized to. The text column also lacked `w_full`, so it
    // shrank to the longest line and clipped the rest. Forcing both the cover
    // row and the text column to be flex containers with `w_full` makes the
    // children fill the available 178px and the `img().size_full()` actually
    // fills the cover box.

    /// The small folder button on a book card: files the book under any group,
    /// or takes it out of every group.
    fn render_book_group_menu(
        &self,
        book: &BookRecord,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        let current_group = book.group_id.clone();

        let mut entries = Vec::new();
        self.collect_group_menu_entries(None, 0, &mut entries);

        let menu_book_id = book_id.clone();
        let menu_view = view.clone();
        Button::new(SharedString::from(format!("book-group-{book_id}")))
            .ghost()
            .xsmall()
            .icon(IconName::Folder)
            .bg(rgba(0xfffefae6))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(7.))
            .tooltip("移动到分组")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu
                    .item(
                        PopupMenuItem::new("未分组")
                            .checked(current_group.is_none())
                            .on_click({
                                let view = menu_view.clone();
                                let book_id = menu_book_id.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.move_book_to_group(book_id.clone(), None, cx);
                                    });
                                }
                            }),
                    )
                    .separator();
                if entries.is_empty() {
                    menu = menu.item(PopupMenuItem::new("还没有分组").disabled(true));
                }
                for (group_id, label, _depth) in &entries {
                    let view = menu_view.clone();
                    let book_id = menu_book_id.clone();
                    let group_id = group_id.clone();
                    let selected = current_group.as_deref() == Some(group_id.as_str());
                    menu = menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(selected)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.move_book_to_group(
                                        book_id.clone(),
                                        Some(group_id.clone()),
                                        cx,
                                    );
                                });
                            }),
                    );
                }
                menu.separator()
                    .item(
                        PopupMenuItem::new("删除图书")
                            .icon(Icon::new(IconName::Delete).text_color(rgb(DANGER)))
                            .on_click({
                                let view = menu_view.clone();
                                let book_id = menu_book_id.clone();
                                move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_delete_book_dialog(book_id.clone(), window, cx);
                                    });
                                }
                            }),
                    )
                    .max_h(px(320.))
                    .scrollable(true)
            })
            .into_any_element()
    }

    fn collect_group_menu_entries(
        &self,
        parent_id: Option<&str>,
        depth: usize,
        out: &mut Vec<(String, String, usize)>,
    ) {
        for group in self.library.child_groups(parent_id) {
            let label = if depth == 0 {
                group.name.clone()
            } else {
                format!("{}{}", "　".repeat(depth), group.name)
            };
            out.push((group.id.clone(), label, depth));
            self.collect_group_menu_entries(Some(&group.id), depth + 1, out);
        }
    }

    fn render_group_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();

        let mut rows: Vec<gpui::AnyElement> = vec![
            div()
                .h_flex()
                .h(px(28.))
                .px_2()
                .text_xs()
                .font_semibold()
                .text_color(rgb(MUTED))
                .child("书架")
                .into_any_element(),
        ];
        rows.push(self.render_sidebar_entry(
            "shelf-all",
            IconName::BookOpen,
            "全部图书",
            self.library.books().len(),
            self.group_filter == GroupFilter::All,
            GroupFilter::All,
            cx,
        ));
        rows.push(self.render_sidebar_entry(
            "shelf-ungrouped",
            IconName::Inbox,
            "未分组",
            self.library.ungrouped_book_count(),
            self.group_filter == GroupFilter::Ungrouped,
            GroupFilter::Ungrouped,
            cx,
        ));
        rows.push(
            div()
                .h(px(1.))
                .my_1()
                .mx_2()
                .bg(rgb(BORDER))
                .into_any_element(),
        );

        let new_group_view = view.clone();
        rows.push(
            div()
                .h_flex()
                .h(px(34.))
                .px_2()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .text_xs()
                        .font_semibold()
                        .text_color(rgb(MUTED))
                        .child(Icon::new(IconName::Folder).small())
                        .child("分组"),
                )
                .child(
                    Button::new("new-root-group")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("新建顶级分组")
                        .on_click(move |_, window, cx| {
                            new_group_view.update(cx, |this, cx| {
                                this.open_create_group_dialog(None, window, cx);
                            });
                        }),
                )
                .into_any_element(),
        );

        let roots = self.library.child_groups(None);
        if roots.is_empty() {
            rows.push(
                div()
                    .px_3()
                    .py_3()
                    .text_xs()
                    .line_height(gpui::relative(1.6))
                    .text_color(rgb(MUTED))
                    .child("还没有分组，点击右上角 + 新建一个。")
                    .into_any_element(),
            );
        } else {
            for group in roots {
                self.collect_group_rows(group, 0, &mut rows, cx);
            }
        }

        div()
            .v_flex()
            .w(px(240.))
            .h_full()
            .flex_none()
            .p_2()
            .rounded(px(14.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SIDEBAR))
            .child(
                div()
                    .id("group-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scrollbar()
                    .child(div().v_flex().gap_0p5().pb_2().children(rows)),
            )
            .into_any_element()
    }

    fn render_sidebar_entry(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        count: usize,
        selected: bool,
        filter: GroupFilter,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        Button::new(id)
            .ghost()
            .w_full()
            .h(px(34.))
            .justify_start()
            .px_2()
            .gap_2()
            .rounded(px(8.))
            .when(selected, |this| {
                this.bg(rgb(ACCENT_SOFT))
                    .border_l_2()
                    .border_color(rgb(ACCENT))
            })
            .child(Icon::new(icon).small().text_color(if selected {
                rgb(ACCENT)
            } else {
                rgb(MUTED)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_left()
                    .truncate()
                    .text_sm()
                    .text_color(if selected { rgb(ACCENT) } else { rgb(INK) })
                    .when(selected, |this| this.font_medium())
                    .child(label),
            )
            .child(
                div()
                    .h_flex()
                    .h(px(20.))
                    .min_w(px(24.))
                    .justify_center()
                    .px(px(6.))
                    .rounded_full()
                    .bg(rgba(0xffffff99))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(count.to_string()),
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.select_group(filter.clone(), cx));
            })
            .into_any_element()
    }

    fn collect_group_rows(
        &self,
        group: &BookGroup,
        depth: usize,
        rows: &mut Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) {
        rows.push(self.render_group_row(group, depth, cx));
        if self.collapsed_groups.contains(&group.id) {
            return;
        }
        for child in self.library.child_groups(Some(&group.id)) {
            self.collect_group_rows(child, depth + 1, rows, cx);
        }
    }

    fn render_group_row(
        &self,
        group: &BookGroup,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let group_id = group.id.clone();
        let has_children = !self.library.child_groups(Some(&group.id)).is_empty();
        let expanded = has_children && !self.collapsed_groups.contains(&group.id);
        let selected = self.group_filter.group_id() == Some(group.id.as_str());
        let count = self.library.group_subtree_book_count(&group.id);
        let row_id = SharedString::from(format!("group-row-{}", group.id));
        let accent = selected.then_some(rgb(ACCENT));

        let toggle = if has_children {
            let toggle_view = view.clone();
            let toggle_id = group_id.clone();
            Button::new(SharedString::from(format!("group-toggle-{}", group.id)))
                .ghost()
                .xsmall()
                .icon(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .on_click(move |_, _, cx| {
                    toggle_view.update(cx, |this, cx| this.toggle_group(toggle_id.clone(), cx));
                })
                .into_any_element()
        } else {
            div().size(px(22.)).flex_none().into_any_element()
        };

        let select_view = view.clone();
        let select_id = group_id.clone();
        let label = Button::new(SharedString::from(format!("group-select-{}", group.id)))
            .ghost()
            .flex_1()
            .min_w(px(0.))
            .h(px(28.))
            .justify_start()
            .px_1()
            .gap_1p5()
            .child(
                Icon::new(if selected {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                })
                .small()
                .text_color(accent.unwrap_or(rgb(MUTED))),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .text_color(accent.unwrap_or(rgb(INK)))
                    .when(selected, |this| this.font_medium())
                    .child(group.name.clone()),
            )
            .on_click(move |_, _, cx| {
                select_view.update(cx, |this, cx| {
                    this.select_group(GroupFilter::Group(select_id.clone()), cx);
                });
            });

        let menu_view = view.clone();
        let menu_id = group_id.clone();
        let actions = Button::new(SharedString::from(format!("group-actions-{}", group.id)))
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .text_color(rgb(MUTED))
            .dropdown_menu(move |menu, _, _| {
                let view = menu_view.clone();
                let group_id = menu_id.clone();
                menu.item(
                    PopupMenuItem::new("新建子分组")
                        .icon(Icon::new(IconName::Plus))
                        .on_click({
                            let view = view.clone();
                            let group_id = group_id.clone();
                            move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.open_create_group_dialog(
                                        Some(group_id.clone()),
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                )
                .item(PopupMenuItem::new("重命名").on_click({
                    let view = view.clone();
                    let group_id = group_id.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.open_rename_group_dialog(group_id.clone(), window, cx);
                        });
                    }
                }))
                .separator()
                .item(PopupMenuItem::new("删除分组").on_click({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.open_delete_group_dialog(group_id.clone(), window, cx);
                        });
                    }
                }))
            });

        div()
            .id(row_id)
            .h_flex()
            .w_full()
            .h(px(34.))
            .items_center()
            .gap_0p5()
            .pl(px(4. + depth as f32 * 16.))
            .pr_1()
            .rounded(px(8.))
            .when(selected, |this| {
                this.bg(rgb(ACCENT_SOFT))
                    .border_l_2()
                    .border_color(rgb(ACCENT))
            })
            .when(!selected, |this| {
                this.hover(|style| style.bg(rgba(0x2926210a)))
            })
            .child(toggle)
            .child(label)
            .child(
                div()
                    .h_flex()
                    .h(px(20.))
                    .min_w(px(24.))
                    .justify_center()
                    .px(px(6.))
                    .rounded_full()
                    .bg(rgba(0xffffff99))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(count.to_string()),
            )
            .child(actions)
            .into_any_element()
    }

    fn render_group_actions(&self, group_id: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let new_child_id = group_id.to_string();
        let new_child_view = view.clone();
        let menu_view = view.clone();
        let menu_group_id = group_id.to_string();

        div()
            .h_flex()
            .gap_2()
            .child(
                Button::new("group-new-child")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label("新建子分组")
                    .on_click(move |_, window, cx| {
                        new_child_view.update(cx, |this, cx| {
                            this.open_create_group_dialog(Some(new_child_id.clone()), window, cx);
                        });
                    }),
            )
            .child(
                Button::new("group-more-actions")
                    .ghost()
                    .small()
                    .icon(IconName::Ellipsis)
                    .tooltip("更多分组操作")
                    .dropdown_menu(move |menu, _, _| {
                        let rename_view = menu_view.clone();
                        let rename_id = menu_group_id.clone();
                        let delete_view = menu_view.clone();
                        let delete_id = menu_group_id.clone();
                        menu.item(
                            PopupMenuItem::new("重命名")
                                .icon(Icon::new(IconName::Settings2))
                                .on_click(move |_, window, cx| {
                                    rename_view.update(cx, |this, cx| {
                                        this.open_rename_group_dialog(
                                            rename_id.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("删除分组")
                                .icon(Icon::new(IconName::Delete).text_color(rgb(DANGER)))
                                .on_click(move |_, window, cx| {
                                    delete_view.update(cx, |this, cx| {
                                        this.open_delete_group_dialog(
                                            delete_id.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_empty_group(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        div()
            .flex_1()
            .v_flex()
            .items_center()
            .justify_center()
            .pb_6()
            .child(
                div()
                    .v_flex()
                    .items_center()
                    .w(px(400.))
                    .p_8()
                    .gap_3()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PAPER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(56.))
                            .rounded_full()
                            .bg(rgb(ACCENT_SOFT))
                            .text_color(rgb(ACCENT))
                            .child(Icon::new(IconName::FolderOpen).large()),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .text_color(rgb(INK))
                            .child("这个分组还是空的"),
                    )
                    .child(
                        div()
                            .max_w(px(320.))
                            .text_center()
                            .text_sm()
                            .line_height(gpui::relative(1.6))
                            .text_color(rgb(MUTED))
                            .child("可在图书卡片右上角的文件夹菜单中，将图书移动到这里。"),
                    )
                    .child(
                        Button::new("empty-group-all-books")
                            .outline()
                            .icon(IconName::BookOpen)
                            .label("查看全部图书")
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.select_group(GroupFilter::All, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    fn select_group(&mut self, filter: GroupFilter, cx: &mut Context<Self>) {
        if self.group_filter == filter {
            return;
        }
        let path = match &filter {
            GroupFilter::Group(id) => self.library.group_path(id),
            _ => Vec::new(),
        };
        self.group_filter = filter;
        self.group_filter_generation = self.group_filter_generation.wrapping_add(1);
        for group in path {
            self.collapsed_groups.remove(&group.id);
        }
        self.notice = None;
        self.library_notice_request = None;
        self.sync_ai_scope(cx);
    }

    fn toggle_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if !self.collapsed_groups.remove(&group_id) {
            self.collapsed_groups.insert(group_id);
        }
        cx.notify();
    }

    fn create_group(&mut self, parent_id: Option<String>, name: String, cx: &mut Context<Self>) {
        let filter_generation = self.group_filter_generation;
        let Some(request_id) = self.begin_library_request("正在创建分组…", cx) else {
            return;
        };
        if let Some(GroupModal::NameInput {
            pending_request,
            error,
            ..
        }) = self.group_modal.as_mut()
        {
            *pending_request = Some(request_id);
            *error = None;
        }
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| {
            library.create_group(&name, parent_id.as_deref())
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        let group = mutation.value;
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            if request_id == this.library_request_generation
                                && filter_generation == this.group_filter_generation
                            {
                                this.group_filter = GroupFilter::Group(group.id.clone());
                                this.group_filter_generation =
                                    this.group_filter_generation.wrapping_add(1);
                            }
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("已创建分组「{}」", group.name),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => {
                        let message = format!("创建分组失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("创建分组任务异常停止：{error}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn rename_group(&mut self, group_id: String, name: String, cx: &mut Context<Self>) {
        let Some(request_id) = self.begin_library_request("正在重命名分组…", cx) else {
            return;
        };
        if let Some(GroupModal::NameInput {
            pending_request,
            error,
            ..
        }) = self.group_modal.as_mut()
        {
            *pending_request = Some(request_id);
            *error = None;
        }
        let services = Arc::clone(&self.services);
        let completed_name = name.clone();
        let task = services.spawn_library_projected(move |library| {
            library.rename_group(&group_id, &name)?;
            Ok(())
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("分组已重命名为「{completed_name}」"),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => {
                        let message = format!("重命名分组失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("重命名分组任务异常停止：{error}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn delete_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        let name = self
            .library
            .group(&group_id)
            .map(|group| group.name.clone())
            .unwrap_or_default();
        let Some(request_id) = self.begin_library_request("正在删除分组…", cx) else {
            return;
        };
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| library.delete_group(&group_id));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: if name.is_empty() {
                                    "分组已删除".to_string()
                                } else {
                                    format!("分组「{name}」已删除，其中的图书已移出分组")
                                },
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("删除分组失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("删除分组任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn move_book_to_group(
        &mut self,
        book_id: String,
        group_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let target = group_id
            .as_deref()
            .and_then(|group_id| self.library.group(group_id))
            .map(|group| group.name.clone());
        let Some(request_id) = self.begin_library_request("正在移动图书…", cx) else {
            return;
        };
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| {
            library.set_book_group(&book_id, group_id.as_deref())
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: match target {
                                    Some(name) => format!("已移动到「{name}」"),
                                    None => "已移出分组".to_string(),
                                },
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("移动图书失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("移动图书任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn open_create_book_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_importing {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("请输入图书名称")
                .default_value(String::new())
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self.group_modal = Some(GroupModal::NameInput {
            title: "新建图书".to_string(),
            confirm_label: "创建".to_string(),
            action: NameInputAction::CreateBook,
            input,
            pending_request: None,
            error: None,
        });
        cx.notify();
    }

    fn open_create_group_dialog(
        &mut self,
        parent_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(parent_id) = parent_id.as_deref() {
            if self.library.group(parent_id).is_none() {
                return;
            }
        }
        let title = match parent_id.as_deref().and_then(|id| self.library.group(id)) {
            Some(parent) => format!("在「{}」下新建子分组", parent.name),
            None => "新建分组".to_string(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(String::new()));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.group_modal = Some(GroupModal::NameInput {
            title,
            confirm_label: "创建".to_string(),
            action: NameInputAction::CreateGroup(parent_id),
            input,
            pending_request: None,
            error: None,
        });
        cx.notify();
    }

    fn open_rename_group_dialog(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.library.group(&group_id).cloned() else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(group.name.clone()));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.group_modal = Some(GroupModal::NameInput {
            title: format!("重命名「{}」", group.name),
            confirm_label: "保存".to_string(),
            action: NameInputAction::RenameGroup(group_id),
            input,
            pending_request: None,
            error: None,
        });
        cx.notify();
    }

    fn open_delete_group_dialog(
        &mut self,
        group_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.library.group(&group_id).cloned() else {
            return;
        };
        let descendants = self.library.group_subtree_ids(&group_id).len() - 1;
        let books = self.library.group_subtree_book_count(&group_id);
        let description = if descendants == 0 {
            format!(
                "将删除分组「{}」。其中 {} 本图书会变为未分组，图书本身不会被移除。",
                group.name, books
            )
        } else {
            format!(
                "将删除分组「{}」及其 {} 个子分组。其中 {} 本图书会变为未分组，图书本身不会被移除。",
                group.name, descendants, books
            )
        };
        self.group_modal = Some(GroupModal::DeleteConfirm {
            group_id,
            name: group.name.clone(),
            description,
        });
        cx.notify();
    }

    fn open_delete_book_dialog(
        &mut self,
        book_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        self.group_modal = Some(GroupModal::DeleteBook {
            book_id,
            title: book.title.clone(),
            description: format!(
                "将删除图书「{}」及书库中的副本与封面，源文件不会被移除。此操作不可撤销。",
                book.title
            ),
        });
        cx.notify();
    }

    fn open_office_trust_dialog(&mut self, book_id: String, cx: &mut Context<Self>) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            self.set_error("图书已经不存在。".to_string(), cx);
            return;
        };
        if !is_office_format_name(&book.format) {
            self.set_error(
                "该图书格式不支持 Microsoft Office 增强预览。".to_string(),
                cx,
            );
            return;
        }
        self.group_modal = Some(GroupModal::OfficeTrust {
            book_id,
            title: book.title.clone(),
            description: format!(
                "仅当你信任「{}」的原始文件时继续。墨页会在只读模式下调用本机 Microsoft Office，禁用宏并禁止更新外部链接，但 Office 自动化不是安全沙箱；结构化预览无需执行此步骤。",
                book.title
            ),
        });
        cx.notify();
    }

    fn close_group_modal(&mut self, cx: &mut Context<Self>) {
        if let Some(GroupModal::NameInput {
            pending_request, ..
        }) = self.group_modal.as_ref()
            && !name_input_modal_can_close(*pending_request)
        {
            return;
        }
        self.group_modal = None;
        cx.notify();
    }

    fn confirm_group_modal(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.group_modal.clone() else {
            return;
        };
        match modal {
            GroupModal::NameInput {
                action,
                input,
                pending_request,
                ..
            } => {
                if pending_request.is_some() {
                    return;
                }
                let name = input.read(cx).value().trim().to_string();
                if name.is_empty() {
                    if let Some(GroupModal::NameInput { error, .. }) = self.group_modal.as_mut() {
                        *error = Some(
                            match action {
                                NameInputAction::CreateBook => "图书名称不能为空",
                                NameInputAction::CreateGroup(_)
                                | NameInputAction::RenameGroup(_) => "分组名称不能为空",
                            }
                            .to_string(),
                        );
                    }
                    cx.notify();
                    return;
                }
                match action {
                    NameInputAction::CreateBook => self.create_book(name.clone(), cx),
                    NameInputAction::CreateGroup(parent_id) => {
                        self.create_group(parent_id.clone(), name.clone(), cx);
                    }
                    NameInputAction::RenameGroup(group_id) => {
                        self.rename_group(group_id.clone(), name.clone(), cx);
                    }
                }
            }
            GroupModal::DeleteConfirm { group_id, .. } => {
                self.delete_group(group_id.clone(), cx);
                self.group_modal = None;
                cx.notify();
            }
            GroupModal::DeleteBook { book_id, .. } => {
                self.delete_book(book_id.clone(), cx);
                self.group_modal = None;
                cx.notify();
            }
            GroupModal::OfficeTrust { .. } => {
                // Office trust confirmation is handled by its button callback
                // because starting the preview also needs the active Window.
            }
        }
    }

    fn delete_book(&mut self, book_id: String, cx: &mut Context<Self>) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        self.cancel_office_preview(Some(&book_id), false, cx);
        let Some(request_id) = self.begin_library_request("正在删除图书…", cx) else {
            return;
        };
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| library.remove_book(&book_id));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            // Reader windows hold their own copy of the store, so
                            // a book deleted here stays readable until those
                            // windows close.
                            this.notice = Some(Notice {
                                text: format!("已删除图书「{}」", book.title),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("删除图书失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("删除图书任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    fn sync_ai_scope(&mut self, cx: &mut Context<Self>) {
        if !should_sync_library_scope(self.library_mutations.close_requested()) {
            cx.notify();
            return;
        }
        let scope = library_ai_scope(&self.library, &self.group_filter);
        self.ai_sidebar
            .update(cx, |sidebar, cx| sidebar.set_scope(scope, cx));
        if should_start_library_search(self.library_mutations.close_requested(), &self.search_query)
        {
            self.start_search(cx);
        } else {
            cx.notify();
        }
    }

    fn cancel_ai_for_window_close(&mut self, cx: &mut Context<Self>) {
        self.cancel_office_preview(None, false, cx);
        self.ai_controller.close();
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
    }

    fn render_group_modal(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let modal = self.group_modal.clone()?;
        let view = cx.entity().clone();
        let modal_pending = matches!(
            &modal,
            GroupModal::NameInput {
                pending_request: Some(_),
                ..
            }
        );

        let (title, body) = match &modal {
            GroupModal::NameInput {
                title,
                input,
                confirm_label,
                pending_request,
                error,
                action: _,
            } => {
                let title = title.clone();
                let pending = pending_request.is_some();
                let confirm_label = if pending {
                    "处理中…".to_string()
                } else {
                    confirm_label.clone()
                };
                let input = input.clone();
                let error = error.clone();
                let body = div()
                    .w_full()
                    .v_flex()
                    .gap_2()
                    .child(Input::new(&input).disabled(pending))
                    .when_some(error, |body, error| {
                        body.child(div().text_sm().text_color(rgb(DANGER)).child(error))
                    })
                    .into_any_element();
                let cancel_view = view.clone();
                let confirm_view = view.clone();
                let buttons = div()
                    .h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("modal-cancel")
                            .label("取消")
                            .outline()
                            .disabled(pending)
                            .on_click(move |_, _, cx| {
                                cancel_view.update(cx, |this, cx| this.close_group_modal(cx));
                            }),
                    )
                    .child(
                        Button::new("modal-confirm")
                            .label(confirm_label)
                            .primary()
                            .disabled(pending)
                            .on_click(move |_, _, cx| {
                                confirm_view.update(cx, |this, cx| this.confirm_group_modal(cx));
                            }),
                    );
                (
                    title,
                    div()
                        .v_flex()
                        .gap_4()
                        .child(body)
                        .child(buttons)
                        .into_any_element(),
                )
            }
            GroupModal::DeleteConfirm {
                name, description, ..
            } => {
                let title = format!("删除分组「{name}」？");
                let description = description.clone();
                (
                    title,
                    render_delete_confirm_body(description, &view).into_any_element(),
                )
            }
            GroupModal::DeleteBook {
                title, description, ..
            } => {
                let title = format!("删除图书「{title}」？");
                let description = description.clone();
                (
                    title,
                    render_delete_confirm_body(description, &view).into_any_element(),
                )
            }
            GroupModal::OfficeTrust {
                book_id,
                title,
                description,
            } => {
                let title = format!("信任并启用「{title}」的 Office 增强预览？");
                let description = description.clone();
                let book_id = book_id.clone();
                let cancel_view = view.clone();
                let confirm_view = view.clone();
                let body = div()
                    .v_flex()
                    .gap_4()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(INK))
                            .line_height(gpui::relative(1.6))
                            .child(description),
                    )
                    .child(
                        div()
                            .h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("modal-office-cancel")
                                    .label("取消")
                                    .outline()
                                    .on_click(move |_, _, cx| {
                                        cancel_view
                                            .update(cx, |this, cx| this.close_group_modal(cx));
                                    }),
                            )
                            .child(
                                Button::new("modal-office-confirm")
                                    .label("信任并生成")
                                    .primary()
                                    .on_click(move |_, window, cx| {
                                        confirm_view.update(cx, |this, cx| {
                                            this.group_modal = None;
                                            this.start_office_preview(book_id.clone(), window, cx);
                                        });
                                    }),
                            ),
                    )
                    .into_any_element();
                (title, body)
            }
        };

        let cancel_view = view.clone();
        Some(
            div()
                .id("group-modal-overlay")
                .absolute()
                .inset_0()
                // Painting the overlay last does not exclude hitboxes from the
                // library below it. Make the modal a real mouse barrier so a
                // dismissal can never continue into a book card.
                .occlude()
                .bg(rgba(0x1f1d1a80))
                .v_flex()
                .items_center()
                .justify_center()
                .on_any_mouse_down(move |event, _, cx| {
                    cx.stop_propagation();
                    if event.button == gpui::MouseButton::Left && !modal_pending {
                        cancel_view.update(cx, |this, cx| this.close_group_modal(cx));
                    }
                })
                .child(
                    div()
                        // Keep clicks inside the panel away from the overlay's
                        // dismissal handler while leaving its controls active.
                        .occlude()
                        .v_flex()
                        .w(px(380.))
                        .p_5()
                        .gap_4()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(SURFACE))
                        .shadow_lg()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child(title),
                        )
                        .child(body),
                )
                .into_any_element(),
        )
    }

    fn render_notice(&self, notice: &Notice) -> gpui::AnyElement {
        let (background, foreground, icon) = if notice.error {
            (rgb(0xf7e1df), rgb(0x9f302c), IconName::TriangleAlert)
        } else {
            (rgb(0xe3efe5), rgb(0x376441), IconName::CircleCheck)
        };
        div()
            .absolute()
            .right_6()
            .bottom(px(STATUS_BAR_HEIGHT + 16.))
            .h_flex()
            .max_w(px(520.))
            .gap_2()
            .px_4()
            .py_3()
            .rounded(px(10.))
            .border_1()
            .border_color(if notice.error {
                rgba(0x9f302c2e)
            } else {
                rgba(0x3764412e)
            })
            .bg(background)
            .text_sm()
            .text_color(foreground)
            .child(Icon::new(icon).small())
            .child(notice.text.clone())
            .into_any_element()
    }
}

impl Render for EpubReaderApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.library_mutations.close_requested() {
            return div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(rgb(PAPER))
                .text_color(rgb(INK))
                .child(div().font_semibold().child("正在关闭墨页…"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child("正在完成已提交的书库操作，完成后将自动关闭。"),
                )
                .into_any_element();
        }
        div()
            .relative()
            .size_full()
            .child(self.render_library(cx))
            .when_some(self.render_group_modal(cx), |this, modal| this.child(modal))
            .into_any_element()
    }
}

/// Shared confirmation body for the destructive "delete group" and
/// "delete book" modals.
fn render_delete_confirm_body(
    description: String,
    view: &Entity<EpubReaderApp>,
) -> gpui::AnyElement {
    let cancel_view = view.clone();
    let confirm_view = view.clone();
    div()
        .v_flex()
        .gap_4()
        .child(
            div()
                .text_sm()
                .text_color(rgb(INK))
                .line_height(gpui::relative(1.6))
                .child(description),
        )
        .child(
            div()
                .h_flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("modal-delete-cancel")
                        .label("取消")
                        .outline()
                        .on_click(move |_, _, cx| {
                            cancel_view.update(cx, |this, cx| this.close_group_modal(cx));
                        }),
                )
                .child(
                    Button::new("modal-delete-confirm")
                        .label("删除")
                        .danger()
                        .on_click(move |_, _, cx| {
                            confirm_view.update(cx, |this, cx| this.confirm_group_modal(cx));
                        }),
                ),
        )
        .into_any_element()
}

fn schedule_library_window_removal(window_handle: gpui::AnyWindowHandle, cx: &mut gpui::AsyncApp) {
    if let Err(error) = window_handle.update(cx, |_, window, cx| {
        remove_window_after_current_frame(window, cx, None);
    }) {
        tracing::warn!(%error, "cannot schedule library window removal after mutation completion");
    }
}

pub fn wrap_root(app: Entity<EpubReaderApp>, window: &mut Window, cx: &mut Context<Root>) -> Root {
    // Close only the library window. On Windows `App::quit()` posts WM_QUIT
    // directly and bypasses child windows' close vetoes, which could destroy a
    // parent HWND while a Reader/Editor WebView build still borrows it. Veto
    // default WM_CLOSE as well: DefWindowProc would destroy the HWND before
    // GPUI's own RevokeDragDrop/DestroyWindow teardown runs.
    let close_requested = Cell::new(false);
    let close_app = app.downgrade();
    window.on_window_should_close(cx, move |window, cx| {
        window.minimize_window();
        if !close_requested.replace(true) {
            match close_app.update(cx, |app, cx| app.request_window_close(cx)) {
                Ok(true) => remove_window_after_current_frame(window, cx, None),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, "library state unavailable during close request");
                    remove_window_after_current_frame(window, cx, None);
                }
            }
        }
        false
    });
    Root::new(app, window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moye_epub_editor::document::{
        BlockDocument, ContentUnit, ContentUnitKind, DocumentLocator, Revision, SourceKind,
    };

    #[test]
    fn search_modes_default_and_round_trip_to_selector_indices() {
        assert_eq!(SearchMode::default(), SearchMode::Hybrid);
        for mode in [
            SearchMode::Keyword,
            SearchMode::Semantic,
            SearchMode::Hybrid,
        ] {
            assert_eq!(search_mode_from_index(search_mode_index(mode)), mode);
        }
    }

    #[test]
    fn stale_search_completion_cannot_replace_current_results() {
        assert!(is_current_search_completion(7, 7));
        assert!(!is_current_search_completion(6, 7));
        assert!(!is_current_search_completion(u64::MAX, 0));
    }

    #[test]
    fn closing_library_never_restarts_a_nonempty_search() {
        assert!(should_start_library_search(false, "  关键词  "));
        assert!(!should_start_library_search(true, "关键词"));
        assert!(!should_start_library_search(false, "   "));
    }

    #[test]
    fn pending_name_modal_cannot_be_dismissed_or_resync_ai_during_close() {
        assert!(name_input_modal_can_close(None));
        assert!(!name_input_modal_can_close(Some(7)));
        assert!(should_sync_library_scope(false));
        assert!(!should_sync_library_scope(true));
    }

    #[test]
    fn older_library_projection_cannot_replace_a_newer_group_projection() {
        assert!(should_apply_library_projection(12, 12));
        assert!(should_apply_library_projection(13, 12));
        assert!(!should_apply_library_projection(11, 12));
    }

    #[test]
    fn library_close_waits_for_every_pending_mutation() {
        let mut lifecycle = LibraryMutationLifecycle::default();
        assert!(lifecycle.begin());
        assert!(lifecycle.begin());
        assert_eq!(lifecycle.pending, 2);

        assert!(!lifecycle.request_close());
        assert!(lifecycle.close_requested());
        assert!(!lifecycle.begin());
        assert!(!lifecycle.finish());
        assert_eq!(lifecycle.pending, 1);
        assert!(lifecycle.finish());
        assert_eq!(lifecycle.pending, 0);

        assert!(!lifecycle.request_close());
    }

    #[test]
    fn library_close_without_pending_mutations_is_ready_once() {
        let mut lifecycle = LibraryMutationLifecycle::default();
        assert!(lifecycle.request_close());
        assert!(!lifecycle.request_close());
        assert!(!lifecycle.begin());
    }

    #[test]
    fn hybrid_passage_maps_to_existing_open_target_and_author() {
        let hit = search_hit_from_passage(
            PassageRecord {
                passage_id: "passage-1".to_string(),
                book_id: "book-1".to_string(),
                book_title: "测试图书".to_string(),
                unit_id: "unit-2".to_string(),
                unit_title: "第二章".to_string(),
                document_revision: moye_epub_editor::document::Revision::new(1),
                unit_revision: moye_epub_editor::document::Revision::new(1),
                text: "命中的正文".to_string(),
                locator: DocumentLocator::unit("book-1", "unit-2"),
                relevance: Some(0.75),
            },
            "作者".to_string(),
            1,
        );
        assert_eq!(hit.spine_index, Some(1));
        assert_eq!(hit.chapter_title.as_deref(), Some("第二章"));
        assert_eq!(hit.author, "作者");
        assert_eq!(hit.relevance, 0.75);
    }

    #[test]
    fn original_export_extension_comes_from_canonical_source() {
        let document = BookDocument::new(
            "book-1",
            "测试图书",
            CanonicalBookSource::imported(
                BookFormat::Azw3,
                "asset-original",
                Some("original.azw3".to_string()),
            ),
        );
        assert_eq!(canonical_original_extension(&document).unwrap(), "azw3");

        let created = BookDocument::created("book-2", "新建图书");
        assert!(canonical_original_extension(&created).is_err());
    }

    #[test]
    fn library_dispatch_binds_native_locator_to_target_document() {
        let mut document = BookDocument::new(
            "book-pdf",
            "PDF",
            CanonicalBookSource::imported(
                BookFormat::Pdf,
                "original-pdf",
                Some("book.pdf".to_string()),
            ),
        );
        document.revision = Revision::new(3);
        document.units.push(
            ContentUnit::new(
                "unit-page",
                ContentUnitKind::Page,
                "Page 1",
                SourceKind::Markdown,
                "page",
                BlockDocument::default(),
            )
            .with_source_locator(SourceLocator::pdf_page(1)),
        );
        let unit_revision = document.units[0].revision;
        let exact = AiSourceLink {
            citation_id: "citation-pdf".to_string(),
            book_id: document.id.clone(),
            unit_id: document.units[0].id.clone(),
            unit_index: Some(0),
            document_revision: document.revision,
            unit_revision,
            locator: Some(
                DocumentLocator::unit(&document.id, &document.units[0].id)
                    .with_source(SourceLocator::pdf_page(1)),
            ),
            label: "PDF page".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
        };
        assert_eq!(
            current_canonical_source_unit_index(&exact, &document),
            Ok(0)
        );

        let wrong_coordinate = AiSourceLink {
            locator: Some(
                DocumentLocator::unit(&document.id, &document.units[0].id)
                    .with_source(SourceLocator::pdf_page(2)),
            ),
            ..exact.clone()
        };
        assert!(current_canonical_source_unit_index(&wrong_coordinate, &document).is_err());

        let mut corrupt_document = document.clone();
        corrupt_document.units[0].source_locator = Some(SourceLocator::slide(1));
        let wrong_type = AiSourceLink {
            locator: Some(
                DocumentLocator::unit(&document.id, &document.units[0].id)
                    .with_source(SourceLocator::slide(1)),
            ),
            ..exact
        };
        assert_eq!(wrong_type.current_unit_index(&corrupt_document), Some(0));
        assert!(
            current_canonical_source_unit_index(&wrong_type, &corrupt_document).is_err(),
            "library cross-book dispatch must reject a canonical unit locator whose type conflicts with the target book"
        );
    }

    #[test]
    fn cancelled_or_superseded_office_completion_is_stale() {
        let cancellation = OfficeCancellation::default();
        let active = OfficePreviewRun {
            request_id: 8,
            book_id: "book-8".to_string(),
            title: "测试".to_string(),
            cancellation: cancellation.clone(),
            enabling: true,
        };
        assert!(is_current_office_preview(Some(&active), 8, "book-8"));
        assert!(!is_current_office_preview(Some(&active), 7, "book-8"));
        assert!(!is_current_office_preview(Some(&active), 8, "book-9"));
        assert!(!is_current_office_preview(None, 8, "book-8"));
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
    }
}
