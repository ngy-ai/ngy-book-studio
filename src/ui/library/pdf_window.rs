//! Opening a detached PDF reader window.

use super::*;

pub(crate) fn open_pdf_reader_window(request: PdfReaderWindowRequest, cx: &mut App) {
    let PdfReaderWindowRequest {
        record,
        book_incarnation,
        bytes,
        pages,
        initial_page,
        document_revision,
        library,
        services,
        library_view,
        persist_progress,
        preview_label,
        singleton_key,
    } = request;
    if application_is_exiting(cx) {
        if let Some(key) = singleton_key {
            release_singleton_window(&key, cx);
        }
        return;
    }
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
        app_id: Some("dev.ngy.book-studio.pdf-reader".to_string()),
        ..Default::default()
    };
    let register_key = singleton_key.clone();
    // Page spacing is a global reading preference, read once per window so the
    // first frame already uses the saved value.
    let compact_reading = services
        .provider_settings()
        .map(|settings| settings.pdf_compact_reading)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot read the PDF reading preference");
            false
        });
    // 皮肤同样只读一次：viewer 的原生底色要立刻是当前皮肤的颜色。
    let appearance = current_appearance(cx);
    let opened = cx.open_window(options, move |window, cx| {
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
                            document_revision,
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
                register_pdf_reader_window(close_weak.clone(), cx);
                if let Some(key) = &register_key {
                    register_singleton_pdf_reader(key, close_weak.clone(), cx);
                }
                on_window_close(window, cx, move |window, cx| {
                    close_weak
                        .update(cx, |reader, cx| reader.handle_window_close(window, cx))
                        .unwrap_or(false)
                });
                let removed_weak = reader.downgrade();
                register_book_window(
                    record.id.clone(),
                    window,
                    move |window, cx| {
                        if let Some(reader) = removed_weak.upgrade() {
                            reader
                                .update(cx, |reader, cx| reader.close_for_removed_book(window, cx));
                        }
                    },
                    cx,
                );
                return cx.new(|cx| Root::new(reader, window, cx));
            }
        };

        let reader = cx.new(|cx| {
            PdfReaderApp::new(
                PdfReaderInit {
                    book_id: record.id.clone(),
                    book_incarnation,
                    document_revision,
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
        register_pdf_reader_window(weak.clone(), cx);
        if let Some(key) = &register_key {
            register_singleton_pdf_reader(key, weak.clone(), cx);
        }
        let title = record.title.clone();
        let removed_weak = weak.clone();
        register_book_window(
            record.id.clone(),
            window,
            move |window, cx| {
                if let Some(reader) = removed_weak.upgrade() {
                    reader.update(cx, |reader, cx| reader.close_for_removed_book(window, cx));
                }
            },
            cx,
        );
        window
            .spawn(cx, async move |cx| {
                match build_pdf_reader_webview(
                    bytes,
                    initial_page,
                    compact_reading,
                    &parent,
                    appearance,
                )
                .await
                {
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
        on_window_close(window, cx, move |window, cx| {
            close_weak
                .update(cx, |reader, cx| reader.handle_window_close(window, cx))
                .unwrap_or(false)
        });
        cx.new(|cx| Root::new(reader, window, cx))
    });
    match (opened, singleton_key) {
        (Ok(handle), Some(key)) => complete_singleton_window(&key, handle.into(), cx),
        (Err(_), Some(key)) => release_singleton_window(&key, cx),
        _ => {}
    }
}
