use std::collections::HashSet;

use super::*;
use moye_epub_editor::{
    document::{DocumentLocator, SourceLocator},
    services::OfficeEnhancedPage,
};

pub(super) fn open_office_slides_window(
    book_id: String,
    book_title: String,
    pages: Vec<OfficeEnhancedPage>,
    library: LibraryStore,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    anyhow::ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
    let pages = pages_from_persisted_pages(&book_id, pages)?;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1180.), px(820.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(520.))),
        titlebar: Some(TitlebarOptions {
            title: Some(format!("《{book_title}》· Office 增强预览").into()),
            ..Default::default()
        }),
        app_id: Some("dev.moye.epub-editor.office-slides".to_string()),
        ..Default::default()
    };
    let window_book_id = book_id.clone();
    cx.open_window(options, move |window, cx| {
        let preview = cx.new(|cx| {
            OfficePagesApp::new(
                book_id,
                book_title,
                pages,
                library,
                services,
                library_view,
                window,
                cx,
            )
        });
        let close_preview = preview.downgrade();
        on_window_close(window, cx, move |_window, cx| {
            close_preview
                .update(cx, |preview, cx| preview.handle_window_close(cx))
                .unwrap_or(true)
        });
        // Deleting the book must take this preview with it. There is no child
        // WebView and nothing left to save, so the window can go after the
        // current frame.
        let removed_preview = preview.downgrade();
        register_book_window(
            window_book_id,
            window,
            move |window, cx| {
                if let Some(preview) = removed_preview.upgrade() {
                    preview.update(cx, |preview, cx| preview.close_for_removed_book(cx));
                }
                remove_window_after_current_frame(window, cx, None);
            },
            cx,
        );
        cx.new(|cx| Root::new(preview, window, cx))
    })?;
    Ok(())
}

struct OfficePage {
    file_name: String,
    page_number: u32,
    image: Arc<Image>,
    locator: DocumentLocator,
}

fn pages_from_persisted_pages(
    book_id: &str,
    pages: Vec<OfficeEnhancedPage>,
) -> Result<Vec<OfficePage>> {
    anyhow::ensure!(!pages.is_empty(), "Office 增强任务尚未发布任何页面");
    let mut locators = HashSet::with_capacity(pages.len());
    let mut file_names = HashSet::with_capacity(pages.len());
    let mut previous_file_name = None;
    pages
        .into_iter()
        .enumerate()
        .map(|(page_index, page)| {
            page.locator
                .validate()
                .context("Office 增强页面定位信息无效")?;
            anyhow::ensure!(
                page.locator.book_id == book_id,
                "Office 增强页面不属于当前图书"
            );
            match page.locator.source.as_ref() {
                Some(SourceLocator::OfficeRenderedPage { .. }) => anyhow::ensure!(
                    page.content_unit_id.is_none(),
                    "Word/Excel 增强预览页不能关联内容单元"
                ),
                _ => anyhow::ensure!(
                    page.content_unit_id.as_deref() == Some(page.locator.unit_id.as_str()),
                    "Office 增强页面与内容单元定位不一致"
                ),
            }
            anyhow::ensure!(
                matches!(
                    page.locator.source.as_ref(),
                    Some(
                        SourceLocator::Slide { .. }
                            | SourceLocator::OfficeSection { .. }
                            | SourceLocator::Worksheet { .. }
                            | SourceLocator::OfficeRenderedPage { .. }
                    )
                ),
                "Office 增强页面使用了不支持的源定位"
            );
            let locator_key = serde_json::to_string(&page.locator)
                .context("无法序列化 Office 增强页面定位信息")?;
            anyhow::ensure!(
                locators.insert(locator_key),
                "Office 增强页面包含重复的定位信息"
            );
            anyhow::ensure!(
                !page.file_name.trim().is_empty(),
                "Office 增强页面文件名为空"
            );
            anyhow::ensure!(
                file_names.insert(page.file_name.clone()),
                "Office 增强页面包含重复的文件名"
            );
            anyhow::ensure!(
                previous_file_name
                    .as_deref()
                    .is_none_or(|previous| previous < page.file_name.as_str()),
                "Office 增强页面文件名顺序与持久化页面顺序不一致"
            );
            previous_file_name = Some(page.file_name.clone());
            let page_number =
                u32::try_from(page_index + 1).context("Office 增强页面数量超出支持范围")?;
            let format = image_format_from_mime(&page.media_type).with_context(|| {
                format!("不支持的 Office 增强页面图片格式：{}", page.media_type)
            })?;
            Ok(OfficePage {
                file_name: page.file_name,
                page_number,
                image: Arc::new(Image::from_bytes(format, page.bytes)),
                locator: page.locator,
            })
        })
        .collect()
}

struct OfficePagesApp {
    book_id: String,
    book_title: String,
    pages: Vec<OfficePage>,
    current: usize,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    ai_sidebar: Entity<AiSidebar>,
    ai_controller: AiSidebarController,
    _ai_subscription: Subscription,
    notice: Option<Notice>,
    closing: bool,
}

impl OfficePagesApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        book_id: String,
        book_title: String,
        pages: Vec<OfficePage>,
        library: LibraryStore,
        services: Arc<AppServices>,
        library_view: Entity<EpubReaderApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
            sidebar.set_reference_hints(office_page_reference_hints(&book_id, &pages, 0), cx);
        });
        let _ai_subscription = cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event);
        let mut ai_controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book_id.clone()),
        )
        .expect("Office page reader AI scope is valid");
        ai_controller.restore(ai_sidebar.clone(), cx);
        Self {
            book_id,
            book_title,
            pages,
            current: 0,
            services,
            library_view,
            ai_sidebar,
            ai_controller,
            _ai_subscription,
            notice: None,
            closing: false,
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
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) => {
                        if source.book_id == this.book_id {
                            if let Some(page_index) =
                                office_page_index_for_source(&this.pages, this.current, &source)
                            {
                                this.set_current(page_index, cx);
                            } else {
                                this.notice = Some(Notice {
                                    text: "引用对应的增强页面已失效，未跳转到其它页面。"
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

    fn set_current(&mut self, index: usize, cx: &mut Context<Self>) {
        let index = index.min(self.pages.len().saturating_sub(1));
        self.notice = None;
        if self.current == index {
            cx.notify();
            return;
        }
        self.current = index;
        self.sync_ai_references(cx);
        cx.notify();
    }

    fn previous(&mut self, cx: &mut Context<Self>) {
        self.set_current(self.current.saturating_sub(1), cx);
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        self.set_current(self.current.saturating_add(1), cx);
    }

    fn sync_ai_references(&mut self, cx: &mut Context<Self>) {
        let references = office_page_reference_hints(&self.book_id, &self.pages, self.current);
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

    /// Cancels this preview because its book left the library. The caller
    /// removes the native window; nothing here is left to save.
    fn close_for_removed_book(&mut self, cx: &mut Context<Self>) {
        self.handle_window_close(cx);
    }
}

fn office_page_index_for_source(
    pages: &[OfficePage],
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
            source_locator if is_exact_office_page_source(source_locator) => {
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

fn is_exact_office_page_source(source: &SourceLocator) -> bool {
    matches!(
        source,
        SourceLocator::Slide { .. }
            | SourceLocator::OfficeSection { .. }
            | SourceLocator::Worksheet { .. }
    )
}

fn office_page_has_exact_unit_mapping(page: &OfficePage) -> bool {
    !matches!(
        page.locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    )
}

fn office_page_reference_hints(
    book_id: &str,
    pages: &[OfficePage],
    current: usize,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..pages.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != current, *index));
    indices
        .into_iter()
        .filter(|index| office_page_has_exact_unit_mapping(&pages[*index]))
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
                revision: None,
            }
        })
        .collect()
}

fn persisted_page_subtitle(page: &OfficePage) -> String {
    if office_page_has_exact_unit_mapping(page) {
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

impl Render for OfficePagesApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let previous = cx.entity().clone();
        let next = cx.entity().clone();
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
            .unwrap_or_else(|| (persisted_page_subtitle(page), MUTED));
        let page_image = Arc::clone(&page.image);
        let page_number = self.current + 1;
        let page_count = self.pages.len();
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
                            .child(
                                Button::new("office-slide-previous")
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
                                Button::new("office-slide-next")
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
                            .p_5()
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .child(img(page_image).size_full().object_fit(ObjectFit::Contain)),
                    )
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
    ) -> OfficeEnhancedPage {
        OfficeEnhancedPage {
            file_name: format!("Page{page_number:05}.png"),
            media_type: "image/png".to_string(),
            bytes: png_fixture(),
            content_unit_id: Some(unit_id.to_string()),
            locator: DocumentLocator::unit(book_id, unit_id).with_source(source),
        }
    }

    fn slide_page(book_id: &str, unit_id: &str, page_number: u32) -> OfficeEnhancedPage {
        page(
            book_id,
            unit_id,
            page_number,
            SourceLocator::slide(page_number),
        )
    }

    fn rendered_page(book_id: &str, unit_id: &str, page_number: u32) -> OfficeEnhancedPage {
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
    ) -> OfficeEnhancedPage {
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
    ) -> OfficeEnhancedPage {
        page(
            book_id,
            unit_id,
            persisted_page_number,
            SourceLocator::worksheet(name, Some(range.to_string())),
        )
    }

    #[test]
    fn persisted_pages_accept_exact_and_rendered_office_locators() {
        let slide_pages = pages_from_persisted_pages("book", vec![slide_page("book", "unit-1", 1)])
            .expect("valid persisted slide page");
        assert_eq!(slide_pages.len(), 1);
        assert_eq!(slide_pages[0].locator.unit_id, "unit-1");
        assert_eq!(slide_pages[0].page_number, 1);
        assert!(!persisted_page_subtitle(&slide_pages[0]).contains("临时"));

        let rendered_pages = pages_from_persisted_pages(
            "book",
            vec![
                rendered_page("book", "unit-1", 1),
                rendered_page("book", "unit-1", 2),
            ],
        )
        .expect("one Word or Excel unit may span multiple pages");
        assert_eq!(rendered_pages.len(), 2);

        let section_pages =
            pages_from_persisted_pages("book", vec![section_page("book", "unit-1", 1, 7)])
                .expect("one-to-one Word page keeps its exact section locator");
        assert_eq!(section_pages[0].page_number, 1);
        assert_eq!(
            section_pages[0].locator.source,
            Some(SourceLocator::office_section(7))
        );

        let worksheet_pages = pages_from_persisted_pages(
            "book",
            vec![worksheet_page("book", "unit-1", 1, "汇总", "B2:G18")],
        )
        .expect("one-to-one Excel page keeps its exact worksheet locator");
        assert_eq!(worksheet_pages[0].page_number, 1);
        assert_eq!(
            worksheet_pages[0].locator.source,
            Some(SourceLocator::worksheet("汇总", Some("B2:G18".to_string())))
        );

        assert!(
            pages_from_persisted_pages("other", vec![slide_page("book", "unit-1", 1)]).is_err()
        );

        let mut mismatched = slide_page("book", "unit-1", 1);
        mismatched.content_unit_id = Some("unit-2".to_string());
        assert!(pages_from_persisted_pages("book", vec![mismatched]).is_err());

        let unsupported = page("book", "unit-1", 1, SourceLocator::pdf_page(1));
        assert!(pages_from_persisted_pages("book", vec![unsupported]).is_err());
    }

    #[test]
    fn persisted_page_locators_and_file_names_must_be_unique_and_ordered() {
        assert!(
            pages_from_persisted_pages(
                "book",
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
                vec![slide_page("book", "unit-1", 1), duplicate_file,],
            )
            .is_err()
        );

        let mut first = slide_page("book", "unit-1", 1);
        first.file_name = "Page00002.png".to_string();
        let mut second = slide_page("book", "unit-2", 2);
        second.file_name = "Page00001.png".to_string();
        assert!(pages_from_persisted_pages("book", vec![first, second]).is_err());

        pages_from_persisted_pages(
            "book",
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
            vec![
                section_page("book", "unit-1", 1, 1),
                section_page("book", "unit-1", 2, 2),
                worksheet_page("book", "unit-2", 3, "汇总", "A1:C8"),
            ],
        )
        .expect("valid persisted pages");
        let references = office_page_reference_hints("book", &pages, 1);
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
            document_revision: moye_epub_editor::document::Revision::new(1),
            unit_revision: moye_epub_editor::document::Revision::new(1),
            locator: Some(pages[0].locator.clone()),
            label: "第一页".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        assert_eq!(
            office_page_index_for_source(&pages, 1, &exact_source),
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
                    moye_epub_editor::document::NormalizedRect::new(0, 0, 100, 100),
                ),
            ];
        for locator in differing_locators {
            let differing_source = AiSourceLink {
                locator: Some(locator),
                ..exact_source.clone()
            };
            assert_eq!(
                office_page_index_for_source(&pages, 1, &differing_source),
                None,
                "an exact source coordinate must not ignore block, text range, or region"
            );
        }

        let ambiguous_pages = vec![
            OfficePage {
                file_name: "Page00001.png".to_string(),
                page_number: 1,
                image: Arc::clone(&pages[0].image),
                locator: pages[0].locator.clone(),
            },
            OfficePage {
                file_name: "Page00002.png".to_string(),
                page_number: 2,
                image: Arc::clone(&pages[0].image),
                locator: pages[0].locator.clone(),
            },
        ];
        assert_eq!(
            office_page_index_for_source(&ambiguous_pages, 0, &exact_source),
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
            office_page_index_for_source(&pages, 1, &wrong_source_type),
            None,
            "a supplied non-Office source locator must not use the unit fallback"
        );

        let unit_only_source = AiSourceLink {
            locator: None,
            ..exact_source
        };
        assert_eq!(
            office_page_index_for_source(&pages, 1, &unit_only_source),
            Some(1),
            "unit-only citations retain the previous current-page behavior"
        );

        let block_source = AiSourceLink {
            locator: Some(DocumentLocator::block("book", "unit-1", "block-1")),
            ..unit_only_source.clone()
        };
        assert_eq!(
            office_page_index_for_source(&pages, 1, &block_source),
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
            office_page_index_for_source(&pages, 1, &stale_source),
            None,
            "a preview-only locator must not silently fall back to a guessed unit page"
        );
    }

    #[test]
    fn repaginated_office_pages_are_preview_only_and_not_ai_references() {
        let pages = pages_from_persisted_pages(
            "book",
            vec![
                rendered_page("book", "unit-1", 1),
                rendered_page("book", "unit-1", 2),
                rendered_page("book", "unit-2", 3),
            ],
        )
        .expect("repaginated Word or Excel pages remain viewable");

        assert!(office_page_reference_hints("book", &pages, 1).is_empty());
        assert!(persisted_page_subtitle(&pages[1]).contains("仅供预览"));
        assert!(persisted_page_subtitle(&pages[1]).contains("不用于搜索或 AI 引用"));

        let guessed_source = AiSourceLink {
            citation_id: "citation-preview-only".to_string(),
            book_id: "book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_index: None,
            document_revision: moye_epub_editor::document::Revision::new(1),
            unit_revision: moye_epub_editor::document::Revision::new(1),
            locator: Some(pages[0].locator.clone()),
            label: "增强预览第一页".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        assert_eq!(
            office_page_index_for_source(&pages, 1, &guessed_source),
            None,
            "a page without an exact unit mapping cannot be an AI citation target"
        );
    }
}
