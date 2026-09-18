//! Serving reader resources, media and IPC requests.

use super::*;

pub(crate) fn spawn_reader_media(
    services: &Arc<AppServices>,
    book_id: String,
    asset: AuthorizedReaderAsset,
    range_header: Option<String>,
) -> tokio::task::JoinHandle<Result<ResourceResponse>> {
    services.spawn_library_read(move |library| {
        let (media_type, byte_len) = library.asset_metadata(&book_id, &asset.asset_id)?;
        ensure!(
            byte_len == asset.byte_len && same_base_media_type(&media_type, &asset.media_type),
            "EPUB 媒体与阅读器授权快照不匹配"
        );
        let response = MediaService::new((*library).clone()).serve(
            &book_id,
            &asset.asset_id,
            range_header.as_deref(),
        )?;
        Ok(response.into())
    })
}

pub(crate) fn same_base_media_type(left: &str, right: &str) -> bool {
    let base = |value: &str| {
        value
            .split(';')
            .next()
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase()
    };
    base(left) == base(right)
}

pub(crate) fn reader_resource_response(
    response: ResourceResponse,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    let mut builder = gpui_component::wry::http::Response::builder()
        .status(response.status)
        .header("Content-Type", response.mime)
        .header("Content-Length", response.content_length.to_string())
        .header("Content-Security-Policy", READER_CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store");
    if let Some(accept_ranges) = response.accept_ranges {
        builder = builder.header("Accept-Ranges", accept_ranges);
    }
    if let Some(content_range) = response.content_range {
        builder = builder.header("Content-Range", content_range);
    }
    builder
        .body(Cow::Owned(response.bytes))
        .expect("valid EPUB resource response")
}

pub(crate) fn reader_error_response(
    status: u16,
    message: impl Into<String>,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    gpui_component::wry::http::Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Security-Policy", "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(Cow::Owned(message.into().into_bytes()))
        .expect("valid EPUB error response")
}

pub(crate) fn reader_owned_response(
    response: gpui_component::wry::http::Response<Cow<'static, [u8]>>,
) -> gpui_component::wry::http::Response<Vec<u8>> {
    let (parts, body) = response.into_parts();
    gpui_component::wry::http::Response::from_parts(parts, body.into_owned())
}

pub(crate) fn reader_ipc_event(
    uri: &gpui_component::wry::http::Uri,
    body: &str,
) -> Option<ReaderWebEvent> {
    if body.len() > 128 * 1024 || !is_reader_document_uri(uri) {
        return None;
    }
    match serde_json::from_str::<ReaderIpcMessage>(body).ok()? {
        ReaderIpcMessage::AnnotationAction { action } => {
            action.valid().then(|| ReaderWebEvent::AnnotationAction {
                url: uri.to_string(),
                action,
            })
        }
        ReaderIpcMessage::ManualTranslation { request } => {
            request.valid().then(|| ReaderWebEvent::ManualTranslation {
                url: uri.to_string(),
                request,
            })
        }
        ReaderIpcMessage::SelectionChanged { selected_text } => {
            let selected_text = normalize_reader_selection(&selected_text)?;
            Some(ReaderWebEvent::SelectionChanged {
                url: uri.to_string(),
                selected_text,
            })
        }
        ReaderIpcMessage::CitationNavigationResult {
            request_id,
            found,
            reason,
        } if (1..=(1_u64 << 53) - 1).contains(&request_id) && reason.len() <= 64 => {
            Some(ReaderWebEvent::CitationNavigationResult {
                url: uri.to_string(),
                request_id,
                found,
                reason,
            })
        }
        ReaderIpcMessage::CitationNavigationResult { .. } => None,
        // The chapter runtime clamps the same range; a size outside it means the
        // message did not come from that runtime, so it is rejected rather than
        // clamped into a value the reader never chose.
        ReaderIpcMessage::ReaderTextSize { font_size }
            if (u32::from(READER_FONT_SIZE_MIN)..=u32::from(READER_FONT_SIZE_MAX))
                .contains(&font_size) =>
        {
            Some(ReaderWebEvent::TextSize {
                url: uri.to_string(),
                font_size: font_size as u8,
            })
        }
        ReaderIpcMessage::ReaderTextSize { .. } => None,
    }
}

pub(crate) fn is_reader_document_uri(uri: &gpui_component::wry::http::Uri) -> bool {
    matches!(
        (uri.scheme_str(), uri.host()),
        (Some("epubreader"), Some("book")) | (Some("http" | "https"), Some("epubreader.book"))
    )
}

pub(crate) fn normalize_reader_selection(value: &str) -> Option<Option<String>> {
    if value.len() > MAX_READER_SELECTION_BYTES {
        return None;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    Some((!normalized.is_empty()).then_some(normalized))
}
