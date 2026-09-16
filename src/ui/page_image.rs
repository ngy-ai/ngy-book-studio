//! Shared page-image reading window for persisted visual page sets.
//!
//! Two sources reuse this window: Microsoft Office enhanced previews
//! (`OfficeEnhanced`) and imported DjVu scans (`Djvu`). They differ only in
//! copy, AI-reference eligibility and whether a page text panel is offered;
//! page loading, ordering, zoom, window registration, close handling and the
//! AI sidebar are identical.

use std::collections::{HashMap, HashSet};

use gpui::{AnyElement, ScrollWheelEvent, rems};
use gpui_component::text::{TextView, TextViewStyle};

use super::reader::{ReadingProgressWrite, ReadingProgressWriteEvent, ReadingProgressWriter};
use super::*;
use ngy_book_studio::{
    document::{DocumentLocator, SourceLocator},
    services::{PublishedVisualPage, ReaderZoomSurface, VisualPageSourceKind},
};

/// Zoom steps are thousandths; `ZOOM_FIT` means "fit the window".
const ZOOM_FIT: u32 = 0;
const ZOOM_MIN_MILLI: u32 = 250;
const ZOOM_MAX_MILLI: u32 = 4_000;
const ZOOM_STEP_MILLI: u32 = 250;
/// Ctrl + wheel notches arrive in bursts; the page size is written after the
/// reader stops changing it instead of once per notch.
const ZOOM_SAVE_DEBOUNCE_MS: u64 = 400;
const PAGE_TEXT_PANEL_WIDTH: f32 = 330.;
/// One page of hidden text is shown at a time; the cap only guards the panel
/// against a pathological page, the full text stays in the content unit.
const MAX_PAGE_TEXT_CHARS: usize = 20_000;

pub(super) fn open_page_image_window(
    book_id: String,
    book_title: String,
    kind: VisualPageSourceKind,
    initial_page: u32,
    pages: Vec<PublishedVisualPage>,
    library: LibraryStore,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    anyhow::ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
    let pages = pages_from_persisted_pages(&book_id, kind, pages)?;
    // Reading progress is written against the book's reading incarnation so a
    // later re-import or reopen cannot resurrect a stale position.
    let book_incarnation = library.progress_incarnation(&book_id);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1180.), px(820.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(520.))),
        titlebar: Some(TitlebarOptions {
            title: Some(format!("《{book_title}》· {}", page_window_title(kind)).into()),
            ..Default::default()
        }),
        app_id: Some(page_window_app_id(kind).to_string()),
        ..Default::default()
    };
    let window_book_id = book_id.clone();
    cx.open_window(options, move |window, cx| {
        let reader = cx.new(|cx| {
            PageImageApp::new(
                book_id,
                book_title,
                kind,
                initial_page,
                book_incarnation,
                pages,
                library,
                services,
                library_view,
                window,
                cx,
            )
        });
        let close_reader = reader.downgrade();
        on_window_close(window, cx, move |_window, cx| {
            close_reader
                .update(cx, |reader, cx| reader.handle_window_close(cx))
                .unwrap_or(true)
        });
        // Deleting the book must take this window with it. There is no child
        // WebView and nothing left to save, so the window can go after the
        // current frame.
        let removed_reader = reader.downgrade();
        register_book_window(
            window_book_id,
            window,
            move |window, cx| {
                if let Some(reader) = removed_reader.upgrade() {
                    reader.update(cx, |reader, cx| reader.close_for_removed_book(cx));
                }
                remove_window_after_current_frame(window, cx, None);
            },
            cx,
        );
        cx.new(|cx| Root::new(reader, window, cx))
    })?;
    Ok(())
}

fn page_window_title(kind: VisualPageSourceKind) -> &'static str {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => "Office 增强预览",
        VisualPageSourceKind::Djvu => "DjVu 阅读",
    }
}

fn page_window_app_id(kind: VisualPageSourceKind) -> &'static str {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => "dev.ngy.book-studio.office-slides",
        VisualPageSourceKind::Djvu => "dev.ngy.book-studio.djvu-reader",
    }
}

struct PageImage {
    file_name: String,
    page_number: u32,
    /// Natural pixel size of the published page image, used for reader zoom.
    width: u32,
    height: u32,
    image: Arc<Image>,
    locator: DocumentLocator,
}

fn pages_from_persisted_pages(
    book_id: &str,
    kind: VisualPageSourceKind,
    pages: Vec<PublishedVisualPage>,
) -> Result<Vec<PageImage>> {
    anyhow::ensure!(!pages.is_empty(), "视觉任务尚未发布任何页面");
    let mut locators = HashSet::with_capacity(pages.len());
    let mut file_names = HashSet::with_capacity(pages.len());
    let mut previous_file_name = None;
    let mut previous_source_page = None;
    pages
        .into_iter()
        .enumerate()
        .map(|(page_index, page)| {
            page.locator.validate().context("视觉页面定位信息无效")?;
            anyhow::ensure!(page.locator.book_id == book_id, "视觉页面不属于当前图书");
            anyhow::ensure!(page.width > 0 && page.height > 0, "视觉页面尺寸无效");
            let page_number = u32::try_from(page_index + 1).context("视觉页面数量超出支持范围")?;
            match kind {
                VisualPageSourceKind::OfficeEnhanced => match page.locator.source.as_ref() {
                    Some(SourceLocator::OfficeRenderedPage { .. }) => anyhow::ensure!(
                        page.content_unit_id.is_none(),
                        "Word/Excel 增强预览页不能关联内容单元"
                    ),
                    _ => anyhow::ensure!(
                        page.content_unit_id.as_deref() == Some(page.locator.unit_id.as_str()),
                        "Office 增强页面与内容单元定位不一致"
                    ),
                },
                VisualPageSourceKind::Djvu => {
                    let Some(SourceLocator::DjvuPage { page: source_page }) =
                        page.locator.source.as_ref()
                    else {
                        anyhow::bail!("DjVu 页面缺少源页码定位");
                    };
                    anyhow::ensure!(
                        page.content_unit_id.as_deref() == Some(page.locator.unit_id.as_str()),
                        "DjVu 页面与内容单元定位不一致"
                    );
                    anyhow::ensure!(
                        previous_source_page.is_none_or(|previous| previous < *source_page),
                        "DjVu 页面源页码顺序与持久化页面顺序不一致"
                    );
                    anyhow::ensure!(
                        page_number == *source_page,
                        "DjVu 页面源页码与持久化页面顺序不一致"
                    );
                    previous_source_page = Some(*source_page);
                }
            }
            anyhow::ensure!(
                matches!(
                    page.locator.source.as_ref(),
                    Some(
                        SourceLocator::Slide { .. }
                            | SourceLocator::OfficeSection { .. }
                            | SourceLocator::Worksheet { .. }
                            | SourceLocator::OfficeRenderedPage { .. }
                            | SourceLocator::DjvuPage { .. }
                    )
                ),
                "视觉页面使用了不支持的源定位"
            );
            let locator_key =
                serde_json::to_string(&page.locator).context("无法序列化视觉页面定位信息")?;
            anyhow::ensure!(locators.insert(locator_key), "视觉页面包含重复的定位信息");
            anyhow::ensure!(!page.file_name.trim().is_empty(), "视觉页面文件名为空");
            anyhow::ensure!(
                file_names.insert(page.file_name.clone()),
                "视觉页面包含重复的文件名"
            );
            anyhow::ensure!(
                previous_file_name
                    .as_deref()
                    .is_none_or(|previous| previous < page.file_name.as_str()),
                "视觉页面文件名顺序与持久化页面顺序不一致"
            );
            previous_file_name = Some(page.file_name.clone());
            let format = image_format_from_mime(&page.media_type)
                .with_context(|| format!("不支持的视觉页面图片格式：{}", page.media_type))?;
            Ok(PageImage {
                file_name: page.file_name,
                page_number,
                width: page.width,
                height: page.height,
                image: Arc::new(Image::from_bytes(format, page.bytes)),
                locator: page.locator,
            })
        })
        .collect()
}

struct PageImageApp {
    book_id: String,
    book_title: String,
    kind: VisualPageSourceKind,
    pages: Vec<PageImage>,
    current: usize,
    /// `ZOOM_FIT` renders the whole page fitted to the window; any other value
    /// is an explicit pixel zoom in thousandths.
    zoom_milli: u32,
    /// Pending write of this book's page size, replaced on every new change.
    zoom_save_task: Option<Task<()>>,
    /// Page text loaded from the canonical units; present only for DjVu, where
    /// the hidden text layer is the page's searchable text.
    page_texts: Vec<Option<String>>,
    text_panel_open: bool,
    progress_writer: Option<ReadingProgressWriter>,
    progress_sync_task: Option<Task<()>>,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    ai_sidebar: Entity<AiSidebar>,
    ai_controller: AiSidebarController,
    _ai_subscription: Subscription,
    notice: Option<Notice>,
    closing: bool,
}

impl PageImageApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
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
    fn queue_progress(&mut self, cx: &mut Context<Self>) {
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

    fn handle_progress_write_event(
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
    fn load_page_texts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                    .submit(request.clone(), self.ai_sidebar.clone(), cx)
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
            let index = current_canonical_source_unit_index(&lookup, &document)
                .map_err(anyhow::Error::msg)?;
            Ok(index)
        });
        let kind = self.kind;
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) => {
                        if source.book_id == this.book_id {
                            if let Some(page_index) =
                                page_index_for_source(&this.pages, kind, this.current, &source)
                            {
                                this.set_current(page_index, cx);
                            } else {
                                this.notice = Some(Notice {
                                    text: "引用对应的页面已失效，未跳转到其它页面。".to_string(),
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

    fn set_current(&mut self, index: usize, cx: &mut Context<Self>) {
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

    fn previous(&mut self, cx: &mut Context<Self>) {
        self.set_current(self.current.saturating_sub(1), cx);
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        self.set_current(self.current.saturating_add(1), cx);
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let next = if self.zoom_milli == ZOOM_FIT {
            ZOOM_MIN_MILLI * 4
        } else {
            self.zoom_milli.saturating_add(ZOOM_STEP_MILLI)
        };
        self.set_zoom_milli(next.min(ZOOM_MAX_MILLI), cx);
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
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

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom_milli(ZOOM_FIT, cx);
    }

    /// Ctrl + wheel, the same scale the zoom buttons drive. The page size
    /// belongs to the book, so every change below is also remembered.
    fn zoom_from_wheel(&mut self, zoom_in: bool, cx: &mut Context<Self>) {
        self.set_zoom_milli(zoom_after_wheel(self.zoom_milli, zoom_in), cx);
    }

    fn set_zoom_milli(&mut self, zoom_milli: u32, cx: &mut Context<Self>) {
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
    fn load_zoom(&mut self, cx: &mut Context<Self>) {
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

    fn schedule_zoom_save(&mut self, cx: &mut Context<Self>) {
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

    fn toggle_text_panel(&mut self, cx: &mut Context<Self>) {
        self.text_panel_open = !self.text_panel_open;
        cx.notify();
    }

    fn sync_ai_references(&mut self, cx: &mut Context<Self>) {
        let references = page_reference_hints(&self.book_id, &self.pages, self.current);
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }

    fn handle_window_close(&mut self, cx: &mut Context<Self>) -> bool {
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
    fn close_for_removed_book(&mut self, cx: &mut Context<Self>) {
        self.handle_window_close(cx);
    }
}

fn page_index_for_source(
    pages: &[PageImage],
    kind: VisualPageSourceKind,
    current: usize,
    source: &AiSourceLink,
) -> Option<usize> {
    if source.stale {
        return None;
    }
    if let Some(locator) = source.validated_locator()
        && let Some(source_locator) = locator.source.as_ref()
    {
        match source_locator {
            SourceLocator::OfficeRenderedPage { .. } => return None,
            source_locator if is_exact_page_source(kind, source_locator) => {
                let mut matching = pages
                    .iter()
                    .enumerate()
                    .filter(|(_, page)| page.locator == *locator)
                    .map(|(index, _)| index);
                let page_index = matching.next()?;
                return matching.next().is_none().then_some(page_index);
            }
            _ => return None,
        }
    }
    if source.locator.is_some() && source.validated_locator().is_none() {
        return None;
    }
    pages
        .get(current)
        .filter(|page| page.locator.unit_id == source.unit_id)
        .map(|_| current)
        .or_else(|| {
            pages
                .iter()
                .position(|page| page.locator.unit_id == source.unit_id)
        })
}

/// Source coordinates that identify exactly one page in a page-image set.
fn is_exact_page_source(kind: VisualPageSourceKind, source: &SourceLocator) -> bool {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => matches!(
            source,
            SourceLocator::Slide { .. }
                | SourceLocator::OfficeSection { .. }
                | SourceLocator::Worksheet { .. }
        ),
        VisualPageSourceKind::Djvu => matches!(source, SourceLocator::DjvuPage { .. }),
    }
}

fn page_has_exact_unit_mapping(page: &PageImage) -> bool {
    !matches!(
        page.locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    )
}

/// AI references for a page set. Only pages with an exact content-unit mapping
/// become references: Office repagination previews deliberately stay out, while
/// every DjVu page maps 1:1 onto its page unit.
fn page_reference_hints(
    book_id: &str,
    pages: &[PageImage],
    current: usize,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..pages.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != current, *index));
    indices
        .into_iter()
        .filter(|index| page_has_exact_unit_mapping(&pages[*index]))
        .map(|index| {
            let page = &pages[index];
            AiReferenceHint {
                book_id: book_id.to_string(),
                unit_id: page.locator.unit_id.clone(),
                unit_index: None,
                locator: Some(page.locator.clone()),
                label: if index == current {
                    format!("当前页面 · 第 {} 页", page.page_number)
                } else {
                    format!("第 {} 页", page.page_number)
                },
                frozen_text: None,
                displayed_text: None,
                revision: None,
            }
        })
        .collect::<Vec<_>>()
}

fn page_subtitle(page: &PageImage, kind: VisualPageSourceKind) -> String {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => {
            if page_has_exact_unit_mapping(page) {
                format!(
                    "Microsoft Office 只读导出的持久化增强页面 · {}",
                    page.file_name
                )
            } else {
                format!(
                    "Microsoft Office 只读导出的增强预览页 · {} · 与章节/工作表无精确对应，仅供预览，不用于搜索或 AI 引用",
                    page.file_name
                )
            }
        }
        VisualPageSourceKind::Djvu => {
            format!(
                "DjVu 页面 · {} · 第 {} 页",
                page.file_name, page.page_number
            )
        }
    }
}

/// Escapes a page's plain text into the HTML subset `TextView` renders.
fn page_text_html(text: &str, has_hidden_text: bool) -> String {
    if !has_hidden_text {
        return "<p>本页没有文字层；页面图片仅供阅读，不参与搜索或 AI 引用。</p>".to_string();
    }
    let mut html = String::with_capacity(text.len() + 16);
    for paragraph in text.split('\n') {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        html.push_str("<p>");
        for character in paragraph.chars() {
            match character {
                '&' => html.push_str("&amp;"),
                '<' => html.push_str("&lt;"),
                '>' => html.push_str("&gt;"),
                other => html.push(other),
            }
        }
        html.push_str("</p>");
    }
    html
}

fn page_text_preview(text: &str) -> String {
    let mut preview = String::new();
    for character in text.chars().take(MAX_PAGE_TEXT_CHARS) {
        preview.push(character);
    }
    preview
}

fn zoomed_size(width: u32, height: u32, zoom_milli: u32) -> (f32, f32) {
    let scale = zoom_milli as f32 / 1_000.0;
    (
        (width as f32 * scale).max(1.0),
        (height as f32 * scale).max(1.0),
    )
}

/// One Ctrl + wheel notch moves the page by one zoom step.
///
/// `ZOOM_FIT` is the reader's own "fit the window", not a size to step from: a
/// page smaller than the window would only add empty margin, so scrolling down
/// there stays put while scrolling up leaves it at 100%.
fn zoom_after_wheel(zoom_milli: u32, zoom_in: bool) -> u32 {
    if zoom_milli == ZOOM_FIT {
        return if zoom_in {
            ZOOM_MIN_MILLI * 4
        } else {
            ZOOM_FIT
        };
    }
    if zoom_in {
        zoom_milli
            .saturating_add(ZOOM_STEP_MILLI)
            .min(ZOOM_MAX_MILLI)
    } else {
        let next = zoom_milli.saturating_sub(ZOOM_STEP_MILLI);
        if next < ZOOM_MIN_MILLI {
            ZOOM_FIT
        } else {
            next
        }
    }
}

/// Ctrl + wheel over the page.
///
/// A plain notch is left to the reader so it keeps scrolling the page; with
/// Ctrl held the notch becomes one zoom step and is swallowed, so a scroller
/// under the pointer cannot also scroll by the same amount.
fn zoom_wheel_listener(
    app: Entity<PageImageApp>,
) -> impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if !event.modifiers.control {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height()).y;
        if f32::from(delta) == 0.0 {
            return;
        }
        cx.stop_propagation();
        app.update(cx, |this, cx| {
            this.zoom_from_wheel(f32::from(delta) > 0.0, cx);
        });
    }
}

/// A selectable, naturally sized page text view inside an outer scroller.
///
/// `TextView` 0.5.1 keeps selection endpoints in its own bounds, so its
/// internal virtual list must stay disabled and the whole view has to move
/// with the outer scroll container.
fn scrollable_page_text(
    id: SharedString,
    html: SharedString,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let scroll_handle = window
        .use_keyed_state(SharedString::from(format!("{id}/scroll")), cx, |_, _| {
            gpui::ScrollHandle::default()
        })
        .read(cx)
        .clone();
    div()
        .id(SharedString::from(format!("{id}/viewport")))
        .relative()
        .flex_1()
        .min_h(px(0.))
        .child(
            div()
                .id(SharedString::from(format!("{id}/body")))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .p_3()
                .child(
                    TextView::html(id, html, window, cx)
                        .style(TextViewStyle::default().paragraph_gap(rems(0.5)))
                        .selectable(true)
                        .scrollable(false)
                        .w(px(PAGE_TEXT_PANEL_WIDTH - 40.))
                        .h_auto()
                        .flex_shrink_0(),
                ),
        )
        .vertical_scrollbar(&scroll_handle)
        .into_any_element()
}

impl Render for PageImageApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let previous = cx.entity().clone();
        let next = cx.entity().clone();
        let zoom_in = cx.entity().clone();
        let zoom_out = cx.entity().clone();
        let reset_zoom = cx.entity().clone();
        let toggle_text = cx.entity().clone();
        let page = &self.pages[self.current];
        let (subtitle, subtitle_color) = self
            .notice
            .as_ref()
            .map(|notice| {
                (
                    notice.text.clone(),
                    if notice.error { DANGER } else { MUTED },
                )
            })
            .unwrap_or_else(|| (page_subtitle(page, self.kind), MUTED));
        let page_image = Arc::clone(&page.image);
        let (natural_width, natural_height) = (page.width, page.height);
        let page_number = self.current + 1;
        let page_count = self.pages.len();
        let zoom_milli = self.zoom_milli;
        let kind = self.kind;
        let text_panel_open = self.text_panel_open && kind == VisualPageSourceKind::Djvu;
        let page_text = self
            .page_texts
            .get(self.current)
            .and_then(|text| text.as_deref())
            .unwrap_or_default();
        let has_hidden_text = !page_text.trim().is_empty();

        let page_area: AnyElement = if zoom_milli == ZOOM_FIT {
            div()
                .id("page-image-fit")
                .size_full()
                .p_5()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .on_scroll_wheel(zoom_wheel_listener(cx.entity()))
                .child(img(page_image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element()
        } else {
            let (width, height) = zoomed_size(natural_width, natural_height, zoom_milli);
            let scroll = window
                .use_keyed_state(SharedString::from("page-image-zoom/scroll"), cx, |_, _| {
                    gpui::ScrollHandle::default()
                })
                .read(cx)
                .clone();
            div()
                .id("page-image-zoom")
                .size_full()
                .relative()
                .child(
                    div()
                        .id("page-image-zoom-scroll")
                        .size_full()
                        .overflow_x_scroll()
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .child(
                            // The listener sits inside the scroller on purpose:
                            // bubble handlers run from the innermost element
                            // outwards, so this one can stop a Ctrl + wheel
                            // notch before the scroller turns it into a scroll.
                            div()
                                .id("page-image-zoom-content")
                                .p_4()
                                .flex_shrink_0()
                                .on_scroll_wheel(zoom_wheel_listener(cx.entity()))
                                .child(img(page_image).w(px(width)).h(px(height))),
                        ),
                )
                .vertical_scrollbar(&scroll)
                .into_any_element()
        };

        let text_panel = text_panel_open.then(|| {
            div()
                .w(px(PAGE_TEXT_PANEL_WIDTH))
                .h_full()
                .flex_none()
                .v_flex()
                .border_l_1()
                .border_color(rgb(BORDER))
                .bg(rgb(SURFACE))
                .child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(BORDER))
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child("本页文字（可选中复制）"),
                        ),
                )
                .child(scrollable_page_text(
                    SharedString::from(format!("page-text-{}", self.current)),
                    SharedString::from(page_text_html(
                        &page_text_preview(page_text),
                        has_hidden_text,
                    )),
                    window,
                    cx,
                ))
        });

        div()
            .size_full()
            .v_flex()
            .bg(rgb(PAPER))
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .justify_between()
                    .gap_4()
                    .px_5()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .font_semibold()
                                    .text_color(rgb(INK))
                                    .child(self.book_title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(subtitle_color))
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .flex_none()
                            .gap_2()
                            .when(kind == VisualPageSourceKind::Djvu, |bar| {
                                bar.child(
                                    Button::new("page-image-text-panel")
                                        .outline()
                                        .label(if text_panel_open {
                                            "隐藏文字"
                                        } else {
                                            "显示文字"
                                        })
                                        .disabled(!has_hidden_text)
                                        .on_click(move |_, _, cx| {
                                            toggle_text
                                                .update(cx, |this, cx| this.toggle_text_panel(cx));
                                        }),
                                )
                            })
                            .child(
                                Button::new("page-image-zoom-out")
                                    .outline()
                                    .label("缩小")
                                    .disabled(zoom_milli == ZOOM_FIT)
                                    .on_click(move |_, _, cx| {
                                        zoom_out.update(cx, |this, cx| this.zoom_out(cx));
                                    }),
                            )
                            .child(
                                div()
                                    .min_w(px(64.))
                                    .text_center()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child(if zoom_milli == ZOOM_FIT {
                                        "适应窗口".to_string()
                                    } else {
                                        format!("{}%", zoom_milli / 10)
                                    }),
                            )
                            .child(
                                Button::new("page-image-zoom-in")
                                    .outline()
                                    .label("放大")
                                    .disabled(zoom_milli == ZOOM_MAX_MILLI)
                                    .on_click(move |_, _, cx| {
                                        zoom_in.update(cx, |this, cx| this.zoom_in(cx));
                                    }),
                            )
                            .child(
                                Button::new("page-image-zoom-fit")
                                    .outline()
                                    .label("适应窗口")
                                    .disabled(zoom_milli == ZOOM_FIT)
                                    .on_click(move |_, _, cx| {
                                        reset_zoom.update(cx, |this, cx| this.reset_zoom(cx));
                                    }),
                            )
                            .child(
                                Button::new("page-image-previous")
                                    .outline()
                                    .icon(IconName::ChevronLeft)
                                    .label("上一页")
                                    .disabled(self.current == 0)
                                    .on_click(move |_, _, cx| {
                                        previous.update(cx, |this, cx| this.previous(cx));
                                    }),
                            )
                            .child(
                                div()
                                    .min_w(px(86.))
                                    .text_center()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child(format!("{page_number} / {page_count}")),
                            )
                            .child(
                                Button::new("page-image-next")
                                    .outline()
                                    .icon(IconName::ChevronRight)
                                    .label("下一页")
                                    .disabled(self.current + 1 >= self.pages.len())
                                    .on_click(move |_, _, cx| {
                                        next.update(cx, |this, cx| this.next(cx));
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .overflow_hidden()
                            .child(page_area),
                    )
                    .children(text_panel)
                    .child(self.ai_sidebar.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_fixture() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("encode PNG fixture");
        output.into_inner()
    }

    fn page(
        book_id: &str,
        unit_id: &str,
        page_number: u32,
        source: SourceLocator,
    ) -> PublishedVisualPage {
        PublishedVisualPage {
            file_name: format!("Page{page_number:05}.png"),
            media_type: "image/png".to_string(),
            bytes: png_fixture(),
            content_unit_id: Some(unit_id.to_string()),
            locator: DocumentLocator::unit(book_id, unit_id).with_source(source),
            width: 800,
            height: 1_200,
        }
    }

    fn slide_page(book_id: &str, unit_id: &str, page_number: u32) -> PublishedVisualPage {
        page(
            book_id,
            unit_id,
            page_number,
            SourceLocator::slide(page_number),
        )
    }

    fn rendered_page(book_id: &str, unit_id: &str, page_number: u32) -> PublishedVisualPage {
        let mut page = page(
            book_id,
            unit_id,
            page_number,
            SourceLocator::office_rendered_page(page_number),
        );
        page.content_unit_id = None;
        page
    }

    fn section_page(
        book_id: &str,
        unit_id: &str,
        persisted_page_number: u32,
        section_index: u32,
    ) -> PublishedVisualPage {
        page(
            book_id,
            unit_id,
            persisted_page_number,
            SourceLocator::office_section(section_index),
        )
    }

    fn worksheet_page(
        book_id: &str,
        unit_id: &str,
        persisted_page_number: u32,
        name: &str,
        range: &str,
    ) -> PublishedVisualPage {
        page(
            book_id,
            unit_id,
            persisted_page_number,
            SourceLocator::worksheet(name, Some(range.to_string())),
        )
    }

    fn djvu_page(book_id: &str, unit_id: &str, page_number: u32) -> PublishedVisualPage {
        page(
            book_id,
            unit_id,
            page_number,
            SourceLocator::djvu_page(page_number),
        )
    }

    #[test]
    fn persisted_pages_accept_exact_and_rendered_office_locators() {
        let slide_pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![slide_page("book", "unit-1", 1)],
        )
        .expect("valid persisted slide page");
        assert_eq!(slide_pages.len(), 1);
        assert_eq!(slide_pages[0].locator.unit_id, "unit-1");
        assert_eq!(slide_pages[0].page_number, 1);
        assert!(
            !page_subtitle(&slide_pages[0], VisualPageSourceKind::OfficeEnhanced).contains("临时")
        );

        let rendered_pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![
                rendered_page("book", "unit-1", 1),
                rendered_page("book", "unit-1", 2),
            ],
        )
        .expect("one Word or Excel unit may span multiple pages");
        assert_eq!(rendered_pages.len(), 2);

        let section_pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![section_page("book", "unit-1", 1, 7)],
        )
        .expect("one-to-one Word page keeps its exact section locator");
        assert_eq!(section_pages[0].page_number, 1);
        assert_eq!(
            section_pages[0].locator.source,
            Some(SourceLocator::office_section(7))
        );

        let worksheet_pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![worksheet_page("book", "unit-1", 1, "汇总", "B2:G18")],
        )
        .expect("one-to-one Excel page keeps its exact worksheet locator");
        assert_eq!(worksheet_pages[0].page_number, 1);
        assert_eq!(
            worksheet_pages[0].locator.source,
            Some(SourceLocator::worksheet("汇总", Some("B2:G18".to_string())))
        );

        assert!(
            pages_from_persisted_pages(
                "other",
                VisualPageSourceKind::OfficeEnhanced,
                vec![slide_page("book", "unit-1", 1)],
            )
            .is_err()
        );

        let mut mismatched = slide_page("book", "unit-1", 1);
        mismatched.content_unit_id = Some("unit-2".to_string());
        assert!(
            pages_from_persisted_pages(
                "book",
                VisualPageSourceKind::OfficeEnhanced,
                vec![mismatched],
            )
            .is_err()
        );

        let unsupported = page("book", "unit-1", 1, SourceLocator::pdf_page(1));
        assert!(
            pages_from_persisted_pages(
                "book",
                VisualPageSourceKind::OfficeEnhanced,
                vec![unsupported],
            )
            .is_err()
        );
    }

    #[test]
    fn djvu_pages_require_a_one_to_one_ordered_unit_mapping() {
        let pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::Djvu,
            vec![
                djvu_page("book", "unit-1", 1),
                djvu_page("book", "unit-2", 2),
            ],
        )
        .expect("valid persisted DjVu pages");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].locator.source, Some(SourceLocator::djvu_page(2)));
        assert!(page_subtitle(&pages[0], VisualPageSourceKind::Djvu).contains("DjVu"));
        assert_eq!(
            page_reference_hints("book", &pages, 1)
                .iter()
                .map(|hint| hint.label.clone())
                .collect::<Vec<_>>(),
            vec!["当前页面 · 第 2 页".to_string(), "第 1 页".to_string()]
        );

        let mut out_of_order = djvu_page("book", "unit-2", 2);
        out_of_order.locator =
            DocumentLocator::unit("book", "unit-2").with_source(SourceLocator::djvu_page(3));
        assert!(
            pages_from_persisted_pages(
                "book",
                VisualPageSourceKind::Djvu,
                vec![djvu_page("book", "unit-1", 1), out_of_order],
            )
            .is_err()
        );

        let mut missing_unit = djvu_page("book", "unit-1", 1);
        missing_unit.content_unit_id = None;
        assert!(
            pages_from_persisted_pages("book", VisualPageSourceKind::Djvu, vec![missing_unit])
                .is_err()
        );

        let wrong_source = page("book", "unit-1", 1, SourceLocator::pdf_page(1));
        assert!(
            pages_from_persisted_pages("book", VisualPageSourceKind::Djvu, vec![wrong_source])
                .is_err()
        );
    }

    #[test]
    fn persisted_page_locators_and_file_names_must_be_unique_and_ordered() {
        assert!(
            pages_from_persisted_pages(
                "book",
                VisualPageSourceKind::OfficeEnhanced,
                vec![
                    rendered_page("book", "unit-1", 1),
                    rendered_page("book", "unit-1", 1),
                ],
            )
            .is_err()
        );

        let mut duplicate_file = slide_page("book", "unit-2", 2);
        duplicate_file.file_name = "Page00001.png".to_string();
        assert!(
            pages_from_persisted_pages(
                "book",
                VisualPageSourceKind::OfficeEnhanced,
                vec![slide_page("book", "unit-1", 1), duplicate_file],
            )
            .is_err()
        );

        let mut first = slide_page("book", "unit-1", 1);
        first.file_name = "Page00002.png".to_string();
        let mut second = slide_page("book", "unit-2", 2);
        second.file_name = "Page00001.png".to_string();
        assert!(
            pages_from_persisted_pages(
                "book",
                VisualPageSourceKind::OfficeEnhanced,
                vec![first, second],
            )
            .is_err()
        );

        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![
                section_page("book", "unit-1", 1, 1),
                rendered_page("book", "unit-2", 2),
            ],
        )
        .expect("the persisted order does not depend on a homogeneous source locator kind");
    }

    #[test]
    fn references_and_source_links_keep_exact_pages_for_repeated_units() {
        let pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![
                section_page("book", "unit-1", 1, 1),
                section_page("book", "unit-1", 2, 2),
                worksheet_page("book", "unit-2", 3, "汇总", "A1:C8"),
            ],
        )
        .expect("valid persisted pages");
        let references = page_reference_hints("book", &pages, 1);
        assert_eq!(references.len(), 3);
        assert_eq!(references[0].unit_id, pages[1].locator.unit_id);
        assert_eq!(references[0].label, "当前页面 · 第 2 页");
        assert_eq!(references[0].locator.as_ref(), Some(&pages[1].locator));
        assert_eq!(references[1].unit_id, pages[0].locator.unit_id);
        assert_eq!(references[1].locator.as_ref(), Some(&pages[0].locator));
        assert_eq!(references[2].unit_id, pages[2].locator.unit_id);
        assert!(references.iter().all(|reference| {
            reference.book_id == "book"
                && reference.unit_index.is_none()
                && reference.frozen_text.is_none()
        }));

        let exact_source = AiSourceLink {
            citation_id: "citation-1".to_string(),
            book_id: "book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_index: None,
            document_revision: ngy_book_studio::document::Revision::new(1),
            unit_revision: ngy_book_studio::document::Revision::new(1),
            locator: Some(pages[0].locator.clone()),
            label: "第一页".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &exact_source
            ),
            Some(0),
            "an exact locator must win over the current page with the same unit ID"
        );

        let exact_source_coordinate = pages[0].locator.source.clone().unwrap();
        let differing_locators =
            [
                DocumentLocator::block("book", "unit-1", "block-1")
                    .with_source(exact_source_coordinate.clone()),
                DocumentLocator::text("book", "unit-1", "block-1", 0, 1)
                    .with_source(exact_source_coordinate.clone()),
                pages[0].locator.clone().with_region(
                    ngy_book_studio::document::NormalizedRect::new(0, 0, 100, 100),
                ),
            ];
        for locator in differing_locators {
            let differing_source = AiSourceLink {
                locator: Some(locator),
                ..exact_source.clone()
            };
            assert_eq!(
                page_index_for_source(
                    &pages,
                    VisualPageSourceKind::OfficeEnhanced,
                    1,
                    &differing_source
                ),
                None,
                "an exact source coordinate must not ignore block, text range, or region"
            );
        }

        let ambiguous_pages = vec![
            PageImage {
                file_name: "Page00001.png".to_string(),
                page_number: 1,
                width: 800,
                height: 1_200,
                image: Arc::clone(&pages[0].image),
                locator: pages[0].locator.clone(),
            },
            PageImage {
                file_name: "Page00002.png".to_string(),
                page_number: 2,
                width: 800,
                height: 1_200,
                image: Arc::clone(&pages[0].image),
                locator: pages[0].locator.clone(),
            },
        ];
        assert_eq!(
            page_index_for_source(
                &ambiguous_pages,
                VisualPageSourceKind::OfficeEnhanced,
                0,
                &exact_source
            ),
            None,
            "an ambiguous exact locator must be rejected"
        );

        let wrong_source_type = AiSourceLink {
            locator: Some(
                DocumentLocator::unit("book", "unit-1").with_source(SourceLocator::pdf_page(1)),
            ),
            ..exact_source.clone()
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &wrong_source_type
            ),
            None,
            "a supplied non-Office source locator must not use the unit fallback"
        );

        let unit_only_source = AiSourceLink {
            locator: None,
            ..exact_source
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &unit_only_source
            ),
            Some(1),
            "unit-only citations retain the previous current-page behavior"
        );

        let block_source = AiSourceLink {
            locator: Some(DocumentLocator::block("book", "unit-1", "block-1")),
            ..unit_only_source.clone()
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &block_source
            ),
            Some(1),
            "a block locator without a visual source coordinate remains a unit-level target"
        );

        let stale_source = AiSourceLink {
            locator: Some(
                DocumentLocator::unit("book", "unit-1")
                    .with_source(SourceLocator::office_rendered_page(99)),
            ),
            ..unit_only_source
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &stale_source
            ),
            None,
            "a preview-only locator must not silently fall back to a guessed unit page"
        );
    }

    #[test]
    fn repaginated_office_pages_are_preview_only_and_not_ai_references() {
        let pages = pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![
                rendered_page("book", "unit-1", 1),
                rendered_page("book", "unit-1", 2),
                rendered_page("book", "unit-2", 3),
            ],
        )
        .expect("repaginated Word or Excel pages remain viewable");

        assert!(page_reference_hints("book", &pages, 1).is_empty());
        let subtitle = page_subtitle(&pages[1], VisualPageSourceKind::OfficeEnhanced);
        assert!(subtitle.contains("仅供预览"));
        assert!(subtitle.contains("不用于搜索或 AI 引用"));

        let guessed_source = AiSourceLink {
            citation_id: "citation-preview-only".to_string(),
            book_id: "book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_index: None,
            document_revision: ngy_book_studio::document::Revision::new(1),
            unit_revision: ngy_book_studio::document::Revision::new(1),
            locator: Some(pages[0].locator.clone()),
            label: "增强预览第一页".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &guessed_source
            ),
            None,
            "a page without an exact unit mapping cannot be an AI citation target"
        );
    }

    #[test]
    fn djvu_page_text_is_escaped_and_zoom_is_clamped() {
        let html = page_text_html("First line & <tag>\n\nSecond line", true);
        assert_eq!(
            html,
            "<p>First line &amp; &lt;tag&gt;</p><p>Second line</p>"
        );
        assert!(page_text_html("", false).contains("没有文字层"));

        assert_eq!(zoomed_size(800, 1_200, 500), (400.0, 600.0));
        assert_eq!(zoomed_size(1, 1, 250), (1.0, 1.0));
    }

    #[test]
    fn wheel_zoom_steps_within_range_and_returns_to_fit() {
        // Fit is the floor: a page can be enlarged out of it, never shrunk into
        // empty margin.
        assert_eq!(zoom_after_wheel(ZOOM_FIT, false), ZOOM_FIT);
        assert_eq!(zoom_after_wheel(ZOOM_FIT, true), 1_000);

        assert_eq!(zoom_after_wheel(1_000, true), 1_250);
        assert_eq!(zoom_after_wheel(1_000, false), 750);
        assert_eq!(zoom_after_wheel(ZOOM_MAX_MILLI, true), ZOOM_MAX_MILLI);
        // One notch below the smallest explicit size falls back to fit instead
        // of leaving a size the fit button could never reach again.
        assert_eq!(zoom_after_wheel(ZOOM_MIN_MILLI, false), ZOOM_FIT);
        assert_eq!(zoom_after_wheel(ZOOM_FIT + 1, false), ZOOM_FIT);
    }
}
