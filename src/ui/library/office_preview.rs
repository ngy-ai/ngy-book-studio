//! Starting, awaiting and cancelling the Office/DjVu preview flows.

use super::*;

impl EpubReaderApp {
    pub(super) fn start_office_preview(
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
            kind: VisualPageSourceKind::OfficeEnhanced,
            initial_page: 1,
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

    pub(super) fn open_persisted_office_preview(
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
            kind: VisualPageSourceKind::OfficeEnhanced,
            initial_page: 1,
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
            services
                .load_published_visual_pages(load_book_id, VisualPageSourceKind::OfficeEnhanced)
                .await
        });
        self.await_office_preview(request_id, book_id, false, enhancement, window, cx);
    }

    /// Opens (or prepares and then opens) the DjVu page reader for a book.
    ///
    /// Page images are published by the local `ngy-djvu-png` render job, so
    /// the first open may show a preparation notice; the render resumes from
    /// its durable cursor and the user can cancel it from the book menu.
    pub(super) fn open_persisted_djvu_reader(
        &mut self,
        book_id: String,
        initial_page: u32,
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
            kind: VisualPageSourceKind::Djvu,
            initial_page: initial_page.max(1),
            cancellation: cancellation.clone(),
            enabling: false,
        });
        self.notice = Some(Notice {
            text: format!("正在准备《{}》的 DjVu 页面…", book.title),
            error: false,
        });
        cx.notify();

        let services = Arc::clone(&self.services);
        let runtime = services.runtime();
        let load_book_id = book_id.clone();
        let preparation = runtime.spawn(prepare_djvu_pages(services, load_book_id, cancellation));
        self.await_office_preview(request_id, book_id, false, preparation, window, cx);
    }

    pub(super) fn await_office_preview(
        &mut self,
        request_id: u64,
        book_id: String,
        generated: bool,
        pages: tokio::task::JoinHandle<Result<Vec<PublishedVisualPage>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let library = self.library.clone();
        let services = Arc::clone(&self.services);
        let library_view = cx.entity().clone();
        cx.spawn_in(window, async move |view, cx| {
            let outcome = match pages.await {
                Ok(outcome) => outcome,
                Err(error) => Err(anyhow::Error::new(error).context("视觉页面准备任务已停止")),
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
            let kind = launch.kind;
            let initial_page = launch.initial_page;
            let opened = cx.update(|_, cx| {
                open_page_image_window(
                    launch.book_id,
                    launch.title,
                    kind,
                    initial_page,
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
                        match kind {
                            VisualPageSourceKind::OfficeEnhanced => format!(
                                "无法打开 Microsoft Office 增强预览：{error:#}；结构化预览仍可使用。"
                            ),
                            VisualPageSourceKind::Djvu => {
                                format!("无法打开 DjVu 阅读窗口：{error:#}")
                            }
                        },
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    pub(super) fn finish_office_preview(
        &mut self,
        request_id: u64,
        book_id: &str,
        generated: bool,
        outcome: Result<Vec<PublishedVisualPage>>,
        cx: &mut Context<Self>,
    ) -> Option<OfficePreviewLaunch> {
        if !is_current_office_preview(self.office_preview.as_ref(), request_id, book_id) {
            return None;
        }
        let active = self
            .office_preview
            .take()
            .expect("current page preview was checked above");
        let pages = match outcome {
            Ok(pages) => pages,
            Err(error) => {
                self.notice = Some(Notice {
                    text: match active.kind {
                        VisualPageSourceKind::OfficeEnhanced => format!(
                            "《{}》的 Microsoft Office 增强预览不可用：{error:#}；已保留结构化预览。",
                            active.title
                        ),
                        VisualPageSourceKind::Djvu => {
                            format!("《{}》的 DjVu 页面不可用：{error:#}", active.title)
                        }
                    },
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
            text: match active.kind {
                VisualPageSourceKind::OfficeEnhanced if generated => format!(
                    "《{}》的 Microsoft Office 增强页面已持久化并通过完整性校验；原件和阅读进度均未修改。",
                    active.title
                ),
                VisualPageSourceKind::OfficeEnhanced => {
                    format!("已读取《{}》的 Microsoft Office 增强页面。", active.title)
                }
                VisualPageSourceKind::Djvu => {
                    format!(
                        "已准备《{}》的 DjVu 页面，共 {} 页。",
                        active.title,
                        pages.len()
                    )
                }
            },
            error: false,
        });
        cx.notify();
        Some(OfficePreviewLaunch {
            book_id: active.book_id,
            title: active.title,
            kind: active.kind,
            initial_page: active.initial_page,
            pages,
        })
    }

    pub(super) fn cancel_office_preview(
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
                text: match active.kind {
                    VisualPageSourceKind::OfficeEnhanced => format!(
                        "已取消《{}》的 Microsoft Office 增强预览；结构化预览仍可使用。",
                        active.title
                    ),
                    VisualPageSourceKind::Djvu => {
                        format!("已取消《{}》的 DjVu 页面准备。", active.title)
                    }
                },
                error: false,
            });
            cx.notify();
        }
    }

    pub(super) fn disable_office_enhancement(
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
}
