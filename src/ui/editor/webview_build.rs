//! Building the two WebViews and the navigation URLs they start from.

use super::*;

/// Builds the WebView used for the editor's preview and WYSIWYG tabs. The
/// trusted ProseMirror shell and canonical chapter projection use distinct
/// private origins; only the content origin can serve authorized book media.
pub(crate) async fn build_editor_webview(
    parent: &ParentWindowHandle,
    state: EditorWebState,
    services: Arc<AppServices>,
    appearance: Appearance,
) -> Result<(wry::WebView, async_channel::Receiver<EditorWebEvent>)> {
    let initial_page = state.page(0).context("编辑器缺少初始章节")?;
    let initial_url = editor_custom_url(
        EditorTab::Preview,
        state.session_id(),
        &initial_page.chapter_id,
        0,
        &initial_page.href,
    );
    let (sender, receiver) = async_channel::unbounded::<EditorWebEvent>();
    let page_sender = sender.clone();
    let ipc_state = state.clone();
    let protocol_state = state.clone();
    let protocol_runtime = services.runtime();
    let raw_webview = wry::WebViewBuilder::new()
        .with_ipc_handler(move |request| {
            if request.body().len() > MAX_EDITOR_IPC_BYTES {
                return;
            }
            let Some(document) = editor_document_request(request.uri()) else {
                return;
            };
            if let Some(message) = parse_editor_citation_navigation_result(request.body()) {
                if document.kind != EditorDocumentKind::Preview
                    || message.session_id != document.session_id
                    || message.chapter_id != document.chapter_id
                    || message.revision != document.revision
                    || normalized_editor_path(&message.href).as_deref()
                        != Some(document.href.as_str())
                {
                    return;
                }
                let _ = sender.try_send(EditorWebEvent::CitationNavigationResult {
                    session_id: message.session_id,
                    chapter_id: message.chapter_id,
                    href: message.href,
                    revision: message.revision,
                    request_id: message.request_id,
                    found: message.found,
                    reason: message.reason,
                });
                return;
            }
            let Some(message) = parse_editor_ipc_message(request.body()) else {
                return;
            };
            if !editor_ipc_matches_document_request(&document, &message) {
                return;
            }
            if let Some(update) = ipc_state.apply_message(message) {
                let _ = sender.try_send(EditorWebEvent::Ipc(update));
            }
        })
        .with_asynchronous_custom_protocol(
            "epubeditor".to_string(),
            move |_, request, responder| {
                if !protocol_state.protocol_is_open() {
                    return;
                }
                let protocol_asset = if editor_origin(request.uri()) == Some(EditorOrigin::Content)
                {
                    match editor_asset_request(request.uri()) {
                        EditorAssetRequest::Asset(asset_id) => {
                            let snapshot = protocol_state.media_snapshot();
                            snapshot
                                .assets
                                .get(&asset_id)
                                .cloned()
                                .map(|asset| (asset_id, asset, snapshot))
                        }
                        EditorAssetRequest::Invalid | EditorAssetRequest::NotAsset => None,
                    }
                } else {
                    None
                };
                let Some((asset_id, asset, snapshot)) = protocol_asset else {
                    let response = editor_owned_response(editor_protocol_response(
                        &protocol_state,
                        &request,
                    ));
                    let open = lock_shared(&protocol_state.protocol_open);
                    if *open {
                        responder.respond(response);
                    }
                    return;
                };
                let range_header = request
                    .headers()
                    .get("Range")
                    .map(|value| value.to_str().unwrap_or("invalid").to_string());
                let book_id = protocol_state.book_id.to_string();
                let task = spawn_editor_media(
                    &services,
                    snapshot,
                    book_id.clone(),
                    asset_id.clone(),
                    asset,
                    range_header,
                );
                let response_state = protocol_state.clone();
                protocol_runtime.spawn(async move {
                    let response = match task.await {
                        Ok(Ok(response)) => editor_media_response(response),
                        Ok(Err(error)) => {
                            tracing::warn!(%book_id, %asset_id, %error, "cannot serve editor media");
                            editor_error_response(404, "编辑器媒体不存在")
                        }
                        Err(error) => {
                            tracing::warn!(%book_id, %asset_id, %error, "editor media task stopped");
                            editor_error_response(404, "编辑器媒体不存在")
                        }
                    };
                    let open = lock_shared(&response_state.protocol_open);
                    if *open {
                        responder.respond(editor_owned_response(response));
                    }
                });
            },
        )
        .with_initialization_script(EDITOR_INITIALIZATION_SCRIPT)
        .with_navigation_handler(|url| is_editor_navigation_url(&url))
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, wry::PageLoadEvent::Finished) {
                let _ = page_sender.try_send(EditorWebEvent::PageLoaded(url));
            }
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        // 章节 XHTML 到达之前显示的就是这个底色，所以它必须已经是当前皮肤的颜色。
        .with_background_color(Palette::bytes(Palette::for_appearance(appearance).paper))
        .with_hotkeys_zoom(false)
        .with_incognito(true)
        .with_url(initial_url)
        .build_as_child_async(parent)
        .await
        .context("无法创建编辑器视图")?;

    Ok((raw_webview, receiver))
}
