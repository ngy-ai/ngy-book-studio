//! Opening the editor for a book.

use super::*;

impl EpubReaderApp {
    pub(super) fn open_book_editor(
        &mut self,
        book_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One editing window per book: a second editor would keep a second
        // unsaved draft for the same document.
        let editor_key = singleton_window_key("editor", &book_id);
        match reserve_singleton_window(&editor_key, cx) {
            SingletonWindowReservation::Activate(handle) => {
                activate_singleton_window(handle, cx);
                return;
            }
            SingletonWindowReservation::InFlight => return,
            SingletonWindowReservation::Reserved => {}
        }
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
            let release_key = editor_key.clone();
            let (record, cover, chapters, document, web_state) = match task.await {
                Ok(Ok(editor_data)) => editor_data,
                Ok(Err(error)) => {
                    let _ = cx.update(move |_, cx| release_singleton_window(&release_key, cx));
                    let _ = view.update(cx, |this, cx| {
                        this.set_error(format!("无法打开编辑器：{error:#}"), cx);
                    });
                    return;
                }
                Err(error) => {
                    let _ = cx.update(move |_, cx| release_singleton_window(&release_key, cx));
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
                    Err(_) => {
                        let key = release_key;
                        let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
                        return;
                    }
                };
            let library_view = view.clone();
            let applied = cx.update(move |_, cx| {
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
                    app_id: Some("dev.ngy.book-studio.editor".to_string()),
                    ..Default::default()
                };
                if application_is_exiting(cx) {
                    release_singleton_window(&editor_key, cx);
                    return;
                }
                let opened = cx.open_window(options, move |window, cx| {
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
                            on_window_close(window, cx, move |window, cx| {
                                close_weak
                                    .update(cx, |editor, cx| editor.handle_window_close(window, cx))
                                    .unwrap_or(false)
                            });
                            let removed_weak = editor.downgrade();
                            register_book_window(
                                record.id.clone(),
                                window,
                                move |window, cx| {
                                    if let Some(editor) = removed_weak.upgrade() {
                                        editor.update(cx, |editor, cx| {
                                            editor.close_for_removed_book(window, cx)
                                        });
                                    }
                                },
                                cx,
                            );
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
                    let removed_weak = weak.clone();
                    register_book_window(
                        record.id.clone(),
                        window,
                        move |window, cx| {
                            if let Some(editor) = removed_weak.upgrade() {
                                editor.update(cx, |editor, cx| {
                                    editor.close_for_removed_book(window, cx)
                                });
                            }
                        },
                        cx,
                    );

                    // Build the preview/rich-text webview in the background and
                    // attach it once ready. The source tab needs no webview.
                    let appearance = current_appearance(cx);
                    window
                        .spawn(cx, async move |cx| {
                            match build_editor_webview(
                                &parent,
                                web_state,
                                Arc::clone(&services),
                                appearance,
                            )
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
                    on_window_close(window, cx, move |window, cx| {
                        close_weak
                            .update(cx, |editor, cx| editor.handle_window_close(window, cx))
                            .unwrap_or(false)
                    });

                    // Wrap in `Root` like the library window: gpui-component
                    // widgets (e.g. `Input`) look up `window.root::<Root>()`
                    // when painting and would panic without it.
                    cx.new(|cx| Root::new(editor, window, cx))
                });
                match opened {
                    Ok(handle) => complete_singleton_window(&editor_key, handle.into(), cx),
                    Err(error) => {
                        tracing::error!(%error, "cannot open editor window");
                        release_singleton_window(&editor_key, cx);
                    }
                }
            });
            if applied.is_err() {
                let key = release_key.clone();
                let _ = cx.update(move |_, cx| release_singleton_window(&key, cx));
            }
        })
        .detach();
    }
}
