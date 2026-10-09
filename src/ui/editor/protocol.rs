//! Parsing and answering the custom URL scheme: origins, assets, media and
//! the responses handed back to the WebView.

use super::*;

pub(crate) fn normalize_editor_selection(value: &str) -> Option<Option<String>> {
    if value.len() > MAX_EDITOR_SELECTION_BYTES {
        return None;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    Some((!normalized.is_empty()).then_some(normalized))
}

pub(crate) fn valid_editor_ipc_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EDITOR_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub(crate) fn parse_editor_ipc_message(body: &str) -> Option<EditorIpcMessage> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let object = value.as_object()?;
    // `Option<u64>` deliberately distinguishes explicit `null` (automatic
    // snapshot/Ready) from an exact host request, but the key itself is part
    // of the versioned contract and cannot be omitted by legacy senders.
    if !object.contains_key("request_id") {
        return None;
    }
    let message = serde_json::from_value::<EditorIpcMessage>(value).ok()?;
    let request_id_valid = message
        .request_id
        .is_none_or(|request_id| (1..=MAX_EDITOR_REQUEST_ID).contains(&request_id));
    let shape_valid = if message.ready {
        message.request_id.is_none() && message.body.is_none() && !message.too_large
    } else {
        !message.too_large || message.body.is_none()
    };
    (valid_editor_ipc_id(&message.session_id)
        && valid_editor_ipc_id(&message.chapter_id)
        && request_id_valid
        && shape_valid)
        .then_some(message)
}

pub(crate) fn parse_editor_citation_navigation_result(
    body: &str,
) -> Option<EditorCitationNavigationResult> {
    let message = serde_json::from_str::<EditorCitationNavigationResult>(body).ok()?;
    (message.kind == "citation_navigation_result"
        && valid_editor_ipc_id(&message.session_id)
        && valid_editor_ipc_id(&message.chapter_id)
        && (1..=MAX_EDITOR_REQUEST_ID).contains(&message.request_id)
        && message.reason.len() <= 64)
        .then_some(message)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorDocumentRequest {
    pub(crate) kind: EditorDocumentKind,
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) revision: u64,
    pub(crate) href: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorDocumentKind {
    TrustedShell,
    Preview,
    RichSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorOrigin {
    Shell,
    Content,
}

pub(crate) fn editor_query_value<'a>(uri: &'a wry::http::Uri, key: &str) -> Option<&'a str> {
    let mut matches = uri.query()?.split('&').filter_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then_some(value)
    });
    let value = matches.next()?;
    matches.next().is_none().then_some(value)
}

pub(crate) fn editor_query_id(uri: &wry::http::Uri, key: &str) -> Option<String> {
    let decoded = urlencoding::decode(editor_query_value(uri, key)?).ok()?;
    valid_editor_ipc_id(&decoded).then(|| decoded.into_owned())
}

pub(crate) fn editor_origin(uri: &wry::http::Uri) -> Option<EditorOrigin> {
    let custom_origin = match (uri.scheme_str(), uri.host()) {
        (Some("epubeditor"), Some(EDITOR_SHELL_HOST)) => Some(EditorOrigin::Shell),
        (Some("epubeditor"), Some(EDITOR_CONTENT_HOST)) => Some(EditorOrigin::Content),
        _ => None,
    };
    if custom_origin.is_some() {
        return custom_origin;
    }
    #[cfg(target_os = "windows")]
    {
        match (uri.scheme_str(), uri.host()) {
            (Some("http"), Some("epubeditor.shell")) => Some(EditorOrigin::Shell),
            (Some("http"), Some("epubeditor.content")) => Some(EditorOrigin::Content),
            _ => None,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

pub(crate) fn normalized_editor_path(path: &str) -> Option<String> {
    let decoded = urlencoding::decode(path).ok()?;
    if decoded.contains('\\') || decoded.as_bytes().contains(&0) {
        return None;
    }
    Some(decoded.trim_start_matches('/').to_string())
}

pub(crate) fn ensure_editor_asset_id(asset_id: &str) -> Result<()> {
    anyhow::ensure!(
        !asset_id.is_empty()
            && asset_id.len() <= MAX_EDITOR_ASSET_ID_BYTES
            && asset_id != "."
            && asset_id != ".."
            && asset_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
            }),
        "编辑器媒体 ID 无效"
    );
    Ok(())
}

pub(crate) fn editor_media_asset_ids(document: &BookDocument) -> HashSet<String> {
    document
        .assets
        .iter()
        .filter(|asset| {
            [
                AssetRole::Cover,
                AssetRole::ContentImage,
                AssetRole::Poster,
                AssetRole::Audio,
                AssetRole::Video,
            ]
            .iter()
            .any(|role| asset.has_role(*role))
        })
        .map(|asset| asset.id.clone())
        .collect()
}

pub(crate) fn editor_asset_path(asset_id: &str) -> String {
    format!("/{EDITOR_ASSET_PATH_PREFIX}{asset_id}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorAssetRequest {
    NotAsset,
    Invalid,
    Asset(String),
}

pub(crate) fn editor_asset_request(uri: &wry::http::Uri) -> EditorAssetRequest {
    let Some(path) = normalized_editor_path(uri.path()) else {
        return if uri.path().contains(EDITOR_ASSET_PATH_PREFIX) {
            EditorAssetRequest::Invalid
        } else {
            EditorAssetRequest::NotAsset
        };
    };
    let Some(asset_id) = path.strip_prefix(EDITOR_ASSET_PATH_PREFIX) else {
        return EditorAssetRequest::NotAsset;
    };
    if uri.query().is_some() || ensure_editor_asset_id(asset_id).is_err() {
        return EditorAssetRequest::Invalid;
    }
    EditorAssetRequest::Asset(asset_id.to_string())
}

/// Rewrites only quoted `src` and `poster` attribute values while they are
/// inside a tag. Text nodes, other attributes, and unknown URLs are preserved
/// byte-for-byte.
pub(crate) fn rewrite_editor_resource_attributes(
    input: &str,
    mut replacement: impl FnMut(&str) -> Option<String>,
) -> String {
    pub(crate) fn attribute_at(bytes: &[u8], start: usize, name: &[u8]) -> bool {
        bytes
            .get(start..start + name.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
            && bytes
                .get(start + name.len())
                .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'=')
    }

    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied_until = 0usize;
    let mut cursor = 0usize;
    let mut in_tag = false;
    let mut quoted = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if !in_tag {
            if byte == b'<' {
                in_tag = true;
            }
            cursor += 1;
            continue;
        }
        if let Some(quote) = quoted {
            if byte == quote {
                quoted = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'>' {
            in_tag = false;
            cursor += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quoted = Some(byte);
            cursor += 1;
            continue;
        }

        let name_len = if attribute_at(bytes, cursor, b"src") {
            3
        } else if attribute_at(bytes, cursor, b"poster") {
            6
        } else {
            cursor += 1;
            continue;
        };
        let boundary = cursor == 0
            || bytes[cursor - 1].is_ascii_whitespace()
            || matches!(bytes[cursor - 1], b'<' | b'/');
        if !boundary {
            cursor += name_len;
            continue;
        }
        let mut value_start = cursor + name_len;
        while bytes
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        if bytes.get(value_start) != Some(&b'=') {
            cursor += name_len;
            continue;
        }
        value_start += 1;
        while bytes
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        let Some(quote @ (b'\'' | b'"')) = bytes.get(value_start).copied() else {
            cursor += name_len;
            continue;
        };
        value_start += 1;
        let Some(relative_end) = bytes[value_start..].iter().position(|byte| *byte == quote) else {
            break;
        };
        let value_end = value_start + relative_end;
        if let Some(value) = input.get(value_start..value_end)
            && let Some(rewritten) = replacement(value)
        {
            output.push_str(&input[copied_until..value_start]);
            output.push_str(&rewritten);
            copied_until = value_end;
        }
        cursor = value_end + 1;
    }
    output.push_str(&input[copied_until..]);
    output
}

pub(crate) fn rewrite_editor_asset_urls(
    html: &str,
    assets: &HashMap<String, EditorMediaAsset>,
) -> String {
    rewrite_editor_resource_attributes(html, |value| {
        let asset_id = value.strip_prefix("ngy-asset:")?;
        if ensure_editor_asset_id(asset_id).is_err() || !assets.contains_key(asset_id) {
            return None;
        }
        Some(editor_asset_path(asset_id))
    })
}

pub(crate) fn canonicalize_editor_asset_urls(
    html: &str,
    assets: &HashMap<String, EditorMediaAsset>,
) -> String {
    rewrite_editor_resource_attributes(html, |value| {
        let relative_prefix = format!("/{EDITOR_ASSET_PATH_PREFIX}");
        let asset_id = if let Some(asset_id) = value.strip_prefix(&relative_prefix) {
            asset_id.to_string()
        } else {
            let uri = value.parse::<wry::http::Uri>().ok()?;
            if editor_origin(&uri) != Some(EditorOrigin::Content) || uri.query().is_some() {
                return None;
            }
            normalized_editor_path(uri.path())?
                .strip_prefix(EDITOR_ASSET_PATH_PREFIX)?
                .to_string()
        };
        if ensure_editor_asset_id(&asset_id).is_err() || !assets.contains_key(&asset_id) {
            return None;
        }
        Some(format!("ngy-asset:{asset_id}"))
    })
}

pub(crate) fn editor_hrefs_match(left: &str, right: &str) -> bool {
    match (normalized_editor_path(left), normalized_editor_path(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub(crate) fn editor_ipc_matches_document_request(
    document: &EditorDocumentRequest,
    message: &EditorIpcMessage,
) -> bool {
    document.kind == EditorDocumentKind::TrustedShell
        && message.session_id == document.session_id
        && message.chapter_id == document.chapter_id
        && message.revision == document.revision
        && normalized_editor_path(&message.href).as_deref() == Some(document.href.as_str())
}

pub(crate) fn has_trusted_editor_shell_origin<B>(request: &wry::http::Request<B>) -> bool {
    request
        .headers()
        .get("Origin")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == EDITOR_SHELL_ORIGIN)
}

pub(crate) fn editor_document_request(uri: &wry::http::Uri) -> Option<EditorDocumentRequest> {
    let kind = match (editor_origin(uri)?, editor_query_value(uri, "mode")?) {
        (EditorOrigin::Shell, "edit") => EditorDocumentKind::TrustedShell,
        (EditorOrigin::Content, "preview") => EditorDocumentKind::Preview,
        (EditorOrigin::Content, "source") => EditorDocumentKind::RichSource,
        _ => return None,
    };
    let session_id = editor_query_id(uri, "session_id")?;
    let chapter_id = editor_query_id(uri, "chapter_id")?;
    let revision = editor_query_value(uri, "rev")?.parse().ok()?;
    let href = normalized_editor_path(uri.path())?;
    (!href.is_empty()).then_some(EditorDocumentRequest {
        kind,
        session_id,
        chapter_id,
        revision,
        href,
    })
}

pub(crate) fn editor_custom_url(
    tab: EditorTab,
    session_id: &str,
    chapter_id: &str,
    revision: u64,
    href: &str,
) -> String {
    let (host, mode) = match tab {
        EditorTab::RichText => (EDITOR_SHELL_HOST, "edit"),
        EditorTab::Preview => (EDITOR_CONTENT_HOST, "preview"),
        EditorTab::Source => unreachable!("source mode does not use the WebView"),
    };
    format!(
        "epubeditor://{host}/{}?mode={mode}&session_id={}&chapter_id={}&rev={revision}",
        href.trim_start_matches('/'),
        urlencoding::encode(session_id),
        urlencoding::encode(chapter_id),
    )
}

pub(crate) fn editor_navigation_url(
    tab: EditorTab,
    session_id: &str,
    chapter_id: &str,
    revision: u64,
    href: &str,
) -> String {
    let url = editor_custom_url(tab, session_id, chapter_id, revision, href);
    #[cfg(target_os = "windows")]
    {
        url.replacen("epubeditor://", "http://epubeditor.", 1)
    }
    #[cfg(not(target_os = "windows"))]
    {
        url
    }
}

pub(crate) fn is_editor_navigation_url(url: &str) -> bool {
    url.parse()
        .ok()
        .and_then(|uri| editor_document_request(&uri))
        .is_some_and(|request| {
            matches!(
                request.kind,
                EditorDocumentKind::TrustedShell | EditorDocumentKind::Preview
            )
        })
}

pub(crate) fn editor_error_response(
    status: u16,
    message: impl Into<String>,
) -> wry::http::Response<Cow<'static, [u8]>> {
    wry::http::Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Security-Policy", "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(message.into().into_bytes()))
        .expect("valid editor error response")
}

pub(crate) fn editor_media_response(
    response: MediaResponse,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let mut builder = wry::http::Response::builder()
        .status(response.status)
        .header("Content-Type", response.media_type)
        .header("Accept-Ranges", response.accept_ranges)
        .header("Content-Length", response.content_length.to_string())
        .header("Content-Security-Policy", "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store");
    if let Some(content_range) = response.content_range {
        builder = builder.header("Content-Range", content_range);
    }
    builder
        .body(Cow::Owned(response.body))
        .expect("valid editor media response")
}

pub(crate) fn editor_owned_response(
    response: wry::http::Response<Cow<'static, [u8]>>,
) -> wry::http::Response<Vec<u8>> {
    let (parts, body) = response.into_parts();
    wry::http::Response::from_parts(parts, body.into_owned())
}

pub(crate) fn spawn_persisted_editor_media(
    services: &Arc<AppServices>,
    book_id: String,
    asset_id: String,
    expected: EditorMediaAsset,
    range_header: Option<String>,
) -> tokio::task::JoinHandle<Result<MediaResponse>> {
    services.spawn_library_read(move |library| {
        anyhow::ensure!(expected.bytes.is_none(), "未保存媒体应从内存快照读取");
        let (media_type, byte_len) = library.asset_metadata(&book_id, &asset_id)?;
        anyhow::ensure!(
            media_type == expected.media_type && byte_len == expected.byte_len,
            "持久媒体与编辑器授权快照不匹配"
        );
        MediaService::new((*library).clone()).serve(&book_id, &asset_id, range_header.as_deref())
    })
}

pub(crate) fn spawn_editor_media(
    services: &Arc<AppServices>,
    snapshot: EditorMediaSnapshot,
    book_id: String,
    asset_id: String,
    expected: EditorMediaAsset,
    range_header: Option<String>,
) -> tokio::task::JoinHandle<Result<MediaResponse>> {
    if expected.bytes.is_none() {
        return spawn_persisted_editor_media(services, book_id, asset_id, expected, range_header);
    }

    services.runtime().handle().spawn_blocking(move || {
        let authorized = snapshot
            .assets
            .get(&asset_id)
            .context("编辑器媒体授权已撤销")?;
        anyhow::ensure!(
            authorized.media_type == expected.media_type
                && authorized.byte_len == expected.byte_len
                && authorized.content_hash == expected.content_hash
                && authorized.bytes.is_some(),
            "未保存媒体与编辑器授权快照不匹配"
        );
        MediaService::new(snapshot).serve(&book_id, &asset_id, range_header.as_deref())
    })
}

pub(crate) fn editor_protocol_response<B>(
    state: &EditorWebState,
    request: &wry::http::Request<B>,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let uri = request.uri();
    let Some(origin) = editor_origin(uri) else {
        return editor_error_response(403, "拒绝非编辑器来源");
    };

    if let Some(document) = editor_document_request(uri) {
        if document.kind == EditorDocumentKind::RichSource
            && !has_trusted_editor_shell_origin(request)
        {
            return editor_error_response(403, "正文快照只允许受信编辑器读取");
        }
        if document.session_id != state.session_id() {
            return editor_error_response(404, "编辑会话已过期");
        }
        let Some(page) = state.page(document.revision) else {
            return editor_error_response(404, "编辑页面已过期");
        };
        if page.chapter_id != document.chapter_id
            || normalized_editor_path(&page.href).as_deref() != Some(document.href.as_str())
        {
            return editor_error_response(404, "编辑页面与章节不匹配");
        }
        let (html, csp) = match document.kind {
            EditorDocumentKind::TrustedShell => {
                (EDITOR_TRUSTED_SHELL.to_string(), EDITOR_SHELL_CSP)
            }
            EditorDocumentKind::Preview | EditorDocumentKind::RichSource => {
                let media = state.media_snapshot();
                (
                    rewrite_editor_asset_urls(&page.html, &media.assets),
                    EDITOR_CONTENT_CSP,
                )
            }
        };
        let content_length = html.len().to_string();
        let mut response = wry::http::Response::builder()
            .status(200)
            .header("Content-Type", "application/xhtml+xml; charset=utf-8")
            .header("Content-Length", content_length)
            .header("Content-Security-Policy", csp)
            .header("X-Content-Type-Options", "nosniff")
            .header("Cache-Control", "no-store")
            .header("Referrer-Policy", "no-referrer");
        if document.kind == EditorDocumentKind::RichSource {
            response = response
                .header("Access-Control-Allow-Origin", EDITOR_SHELL_ORIGIN)
                .header("Cross-Origin-Resource-Policy", "cross-origin")
                .header("Vary", "Origin");
        } else {
            response = response.header("Cross-Origin-Resource-Policy", "same-origin");
        }
        return response
            .body(Cow::Owned(html.into_bytes()))
            .expect("valid editor document response");
    }

    if origin != EditorOrigin::Content {
        return editor_error_response(404, "编辑器资源不存在");
    }
    match editor_asset_request(uri) {
        EditorAssetRequest::Asset(asset_id) => {
            let range_header = request
                .headers()
                .get("Range")
                .map(|value| value.to_str().unwrap_or("invalid"));
            let snapshot = state.media_snapshot();
            let response = match MediaService::new(snapshot.clone()).serve(
                snapshot.book_id.as_ref(),
                &asset_id,
                range_header,
            ) {
                Ok(response) => response,
                Err(_) => return editor_error_response(404, "编辑器媒体不存在"),
            };
            return editor_media_response(response);
        }
        EditorAssetRequest::Invalid => {
            return editor_error_response(404, "编辑器媒体不存在");
        }
        EditorAssetRequest::NotAsset => {}
    }

    editor_error_response(404, "编辑器资源不存在")
}
