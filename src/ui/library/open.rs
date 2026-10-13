//! Opening a book, including at a locator or an annotation.

use super::*;

impl EpubReaderApp {
    pub(crate) fn open_book_at(
        &mut self,
        book_id: String,
        requested_spine: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_request(book_id, requested_spine, None, None, window, cx);
    }

    pub(crate) fn open_book_at_source(
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
            None,
            window,
            cx,
        );
    }

    pub(crate) fn open_book_at_annotation(
        &mut self,
        note: Annotation,
        on_error: impl Fn(String, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_request(
            note.book_id.clone(),
            None,
            None,
            Some((note, Rc::new(on_error))),
            window,
            cx,
        );
    }

    pub(super) fn open_book_request(
        &mut self,
        book_id: String,
        requested_spine: Option<usize>,
        source: Option<AiSourceLink>,
        annotation: Option<(Annotation, AnnotationOpenError)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One reading window per book: a second PDF or reflowable reader would
        // keep a second progress writer and AI session for the same document.
        // An already-open window is activated and navigated to the target once
        // the asynchronous document load resolves it.
        let reader_key = singleton_window_key("reader", &book_id);
        let existing = match reserve_singleton_window(&reader_key, cx) {
            SingletonWindowReservation::Activate(handle) => Some(handle),
            SingletonWindowReservation::InFlight => return,
            SingletonWindowReservation::Reserved => None,
        };
        let services = Arc::clone(&self.services);
        let (annotation, annotation_error): (Option<_>, Option<_>) = annotation.unzip();
        let task = services.spawn_library_read(move |library| {
            let record = library.book_record(&book_id)?;
            let book_incarnation = library
                .progress_incarnation(&book_id)
                .context("图书阅读实例已失效")?;
            let document = library.document(&book_id)?;
            let annotation_index = annotation
                .as_ref()
                .map(|note| {
                    let current = library
                        .list_annotations(&book_id, Some(&note.content_unit_id))?
                        .into_iter()
                        .find(|current| current.id == note.id)
                        .context("笔记已被删除，请刷新笔记列表")?;
                    current_annotation_unit_index(&current, &document)
                })
                .transpose()?;
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
            // 只有从书架打开这本书才回到上次停下的章内位置；引用、笔记和显式
            // 章节目标都必须落在目标处，而不是旧位置。
            let restore_position =
                requested_spine.is_none() && annotation_index.is_none() && source_index.is_none();
            let requested_spine = annotation_index.or(requested_spine);
            if record.format == "pdf" {
                let pages = document
                    .units
                    .into_iter()
                    .enumerate()
                    .map(|(index, unit)| PdfReaderPage {
                        unit_id: Some(unit.id),
                        unit_index: Some(index),
                        unit_revision: unit.revision.0,
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
                    document_revision: document.revision.0,
                });
            }
            if record.format == "djvu" {
                let page_count = document.units.len();
                ensure!(page_count > 0, "该 DjVu 图书没有可阅读的页面");
                let initial_index = source_index
                    .or(requested_spine)
                    .unwrap_or(record.last_spine)
                    .min(page_count - 1);
                let mut initial_page = match document.units[initial_index].source_locator.as_ref() {
                    Some(SourceLocator::DjvuPage { page }) => *page,
                    _ => u32::try_from(initial_index + 1).unwrap_or(u32::MAX),
                };
                if let Some(source) = source.as_ref() {
                    match source
                        .validated_locator()
                        .and_then(|locator| locator.source.as_ref())
                    {
                        Some(SourceLocator::DjvuPage { page }) => {
                            ensure!(
                                document.units.get(initial_index).is_some_and(|unit| {
                                    unit.id == source.unit_id
                                        && unit.source_locator
                                            == Some(SourceLocator::djvu_page(*page))
                                }),
                                "引用对应的 DjVu 页面已失效"
                            );
                            initial_page = *page;
                        }
                        Some(_) => anyhow::bail!("引用使用了 DjVu 阅读器不支持的来源定位"),
                        None => {}
                    }
                }
                return Ok::<_, anyhow::Error>(OpenReaderPayload::PageImages {
                    record,
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
            // 保存的锚点只在它属于正要打开的章节时才用于恢复；章节换了就只
            // 落到章节开头。
            let initial_anchor = if restore_position {
                library
                    .reading_position(&book_id)?
                    .and_then(|(unit_id, anchor)| {
                        (progress_locators
                            .get(initial_spine)
                            .map(|locator| locator.unit_id.as_str())
                            == Some(unit_id.as_str()))
                        .then_some(anchor)
                    })
            } else {
                None
            };
            Ok::<_, anyhow::Error>(OpenReaderPayload::Reflowable {
                record,
                book_incarnation,
                opened,
                initial_spine,
                initial_anchor,
                initial_citation,
                progress_locators,
                annotation_revisions: (
                    document.revision.0,
                    document.units.iter().map(|unit| unit.revision.0).collect(),
                ),
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
                    document_revision,
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
                            Err(_) => {
                                let key = reader_key.clone();
                                let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
                                return;
                            }
                        };
                    let _ = cx.update(move |_, cx| {
                        if let Some(handle) = existing {
                            if let Some(reader) = existing_singleton_pdf_reader(&reader_key, cx) {
                                let _ = reader.update(cx, |reader, cx| {
                                    reader.request_page(initial_page, cx)
                                });
                                activate_singleton_window(handle, cx);
                                return;
                            }
                            // The reader closed while the document loaded:
                            // reserve again and open a fresh window.
                            match reserve_singleton_window(&reader_key, cx) {
                                SingletonWindowReservation::Activate(handle) => {
                                    activate_singleton_window(handle, cx);
                                    return;
                                }
                                SingletonWindowReservation::InFlight => return,
                                SingletonWindowReservation::Reserved => {}
                            }
                        }
                        open_pdf_reader_window(
                            PdfReaderWindowRequest {
                                record,
                                book_incarnation,
                                bytes,
                                pages,
                                initial_page,
                                document_revision,
                                library,
                                services,
                                library_view,
                                persist_progress: true,
                                preview_label: "PDF".to_string(),
                                singleton_key: Some(reader_key),
                            },
                            cx,
                        );
                    });
                }
                Ok(Ok(OpenReaderPayload::PageImages {
                    record,
                    initial_page,
                })) => {
                    // The page-image reader is its own window kind, so release
                    // the reading-window reservation here. The window still
                    // joins the per-book window registry, and the shared
                    // preparation state (request id plus cancellation) keeps a
                    // second open from racing the first.
                    let key = reader_key.clone();
                    let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
                    let book_id = record.id.clone();
                    if view
                        .update_in(cx, |this, window, cx| {
                            this.open_persisted_djvu_reader(book_id, initial_page, window, cx)
                        })
                        .is_err()
                    {
                        let key = reader_key.clone();
                        let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
                    }
                }
                Ok(Ok(OpenReaderPayload::Reflowable {
                    record,
                    book_incarnation,
                    opened,
                    initial_spine: current_spine,
                    initial_anchor,
                    initial_citation,
                    progress_locators,
                    annotation_revisions,
                })) => {
                    let title = record.title.clone();
                    let release_key = reader_key.clone();
                    let (library, services, library_view) = match view.update(cx, |this, cx| {
                        (
                            this.library.clone(),
                            Arc::clone(&this.services),
                            cx.entity().clone(),
                        )
                    }) {
                        Ok(values) => values,
                        Err(_) => {
                            let key = release_key;
                            let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
                            return;
                        }
                    };
                    let applied = cx.update(move |_, cx| {
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
                            app_id: Some("dev.ngy.book-studio.reader".to_string()),
                            ..Default::default()
                        };
                        if application_is_exiting(cx) {
                            release_singleton_window(&reader_key, cx);
                            return;
                        }
                        if let Some(handle) = existing {
                            if let Some(reader) = existing_singleton_reader(&reader_key, cx) {
                                let citation = initial_citation.clone();
                                let spine = current_spine;
                                let _ = reader.update(cx, |reader, cx| {
                                    match citation {
                                        Some(target) => reader.navigate_to_citation(target, cx),
                                        None => reader.navigate_to_spine(spine, cx),
                                    }
                                });
                                activate_singleton_window(handle, cx);
                                return;
                            }
                            // The reader closed while the document loaded:
                            // reserve again and open a fresh window.
                            match reserve_singleton_window(&reader_key, cx) {
                                SingletonWindowReservation::Activate(handle) => {
                                    activate_singleton_window(handle, cx);
                                    return;
                                }
                                SingletonWindowReservation::InFlight => return,
                                SingletonWindowReservation::Reserved => {}
                            }
                        }
                        let register_key = reader_key.clone();
                        let opened = cx.open_window(options, move |window, cx| {
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
                                            annotation_revisions,
                                            initial_citation,
                                            initial_anchor.clone(),
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
                                    register_singleton_reader(&register_key, close_weak.clone(), cx);
                                    on_window_close(window, cx, move |window, cx| {
                                        close_weak
                                            .update(cx, |reader, cx| {
                                                reader.handle_window_close(window, cx)
                                            })
                                            .unwrap_or(false)
                                    });
                                    let removed_weak = reader.downgrade();
                                    register_book_window(
                                        record.id.clone(),
                                        window,
                                        move |window, cx| {
                                            if let Some(reader) = removed_weak.upgrade() {
                                                reader.update(cx, |reader, cx| {
                                                    reader.close_for_removed_book(window, cx)
                                                });
                                            }
                                        },
                                        cx,
                                    );
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
                                    annotation_revisions.clone(),
                                    initial_citation.clone(),
                                    initial_anchor.clone(),
                                    library.clone(),
                                    Arc::clone(&services),
                                    library_view.clone(),
                                    true,
                                    window,
                                    cx,
                                )
                            });
                            let weak = reader.downgrade();
                            register_singleton_reader(&register_key, weak.clone(), cx);
                            let removed_weak = weak.clone();
                            register_book_window(
                                record.id.clone(),
                                window,
                                move |window, cx| {
                                    if let Some(reader) = removed_weak.upgrade() {
                                        reader.update(cx, |reader, cx| {
                                            reader.close_for_removed_book(window, cx)
                                        });
                                    }
                                },
                                cx,
                            );

                            // Build the child WebView against this window's HWND and
                            // attach it once ready.
                            let book = opened.clone();
                            // 皮肤只在窗口创建时读一次：WebView 的原生底色要立刻是
                            // 对的那一种。之后换皮肤由阅读器窗口自己重放外观脚本。
                            let appearance = current_appearance(cx);
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
                                        appearance,
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
                                                                        cx.background_executor().timer(
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
                            on_window_close(window, cx, move |window, cx| {
                                close_weak
                                    .update(cx, |reader, cx| {
                                        reader.handle_window_close(window, cx)
                                    })
                                    .unwrap_or(false)
                            });

                            // Wrap in `Root` like the library window: gpui-kit
                            // widgets (e.g. `Input`) look up `window.root::<Root>()`
                            // when painting, and would panic without it.
                            cx.new(|cx| Root::new(reader, window, cx))
                        });
                        match opened {
                            Ok(handle) => complete_singleton_window(&reader_key, handle.into(), cx),
                            Err(error) => {
                                tracing::error!(%error, "cannot open reader window");
                                release_singleton_window(&reader_key, cx);
                            }
                        }
                    });
                    if applied.is_err() {
                        let key = release_key.clone();
                        let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
                    }
                }
                Ok(Err(error)) => {
                    let _ = cx.update(move |_, cx| release_singleton_window(&reader_key, cx));
                    if let Some(on_error) = annotation_error.as_ref() {
                        let _ = cx.update(|_, cx| on_error(format!("无法打开笔记所在章节：{error:#}"), cx));
                    }
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("无法打开图书：{error:#}"), cx);
                    });
                }
                Err(error) => {
                    let _ = cx.update(move |_, cx| release_singleton_window(&reader_key, cx));
                    if let Some(on_error) = annotation_error.as_ref() {
                        let _ = cx.update(|_, cx| on_error(format!("打开章节后台任务异常停止：{error}"), cx));
                    }
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("打开图书后台任务异常停止：{error}"), cx);
                    });
                }
            }
        })
        .detach();
    }
}
