//! Building the reader webview and serving its document.

use super::*;

pub(crate) async fn build_reader_webview(
    opened: &OpenedBook,
    current_spine: usize,
    initial_href: Option<&str>,
    parent: &ParentWindowHandle,
    book_id: String,
    services: Arc<AppServices>,
    appearance: Appearance,
) -> Result<(
    wry::WebView,
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

    let raw_webview = wry::WebViewBuilder::new()
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
        .with_initialization_script(include_str!("annotations.js"))
        .with_initialization_script(include_str!("translations.js"))
        .with_initialization_script(include_str!("read_aloud.js"))
        .with_initialization_script(include_str!("position.js"))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, wry::PageLoadEvent::Finished) {
                let _ = page_sender.try_send(ReaderWebEvent::PageLoaded(url));
            }
        })
        .with_download_started_handler(|_, _| false)
        // 章节到达之前 WebView 显示的是这个底色：深色皮肤下必须是深色，否则每次
        // 翻页都会闪一帧米白色。
        .with_background_color(Palette::bytes(Palette::for_appearance(appearance).paper))
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
