//! Building the PDF webview and serving its document.

use super::*;

pub(crate) async fn build_pdf_reader_webview(
    pdf_bytes: Arc<Vec<u8>>,
    initial_page: u32,
    compact_reading: bool,
    parent: &ParentWindowHandle,
    appearance: Appearance,
) -> Result<(wry::WebView, async_channel::Receiver<PdfIpcMessage>)> {
    let (sender, receiver) = async_channel::unbounded();
    let ipc_sender = sender.clone();
    let protocol_pdf = pdf_bytes;
    let raw_webview = wry::WebViewBuilder::new()
        .with_ipc_handler(move |request| {
            if request.body().len() > MAX_PDF_IPC_BYTES
                || !is_pdf_navigation_url(&request.uri().to_string())
            {
                return;
            }
            let Ok(message) = serde_json::from_str::<PdfIpcMessage>(request.body()) else {
                return;
            };
            let _ = ipc_sender.try_send(message);
        })
        .with_custom_protocol("ngypdf".to_string(), move |_, request| {
            let Some((mime, body)) = pdf_resource(protocol_pdf.as_slice(), request.uri().path())
            else {
                return wry::http::Response::builder()
                    .status(404)
                    .header("Content-Type", "text/plain; charset=utf-8")
                    .header("Content-Security-Policy", "default-src 'none'")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Cow::Owned(b"PDF resource not found".to_vec()))
                    .expect("valid PDF error response");
            };
            let content_length = body.len();
            wry::http::Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("Content-Length", content_length.to_string())
                .header("Content-Security-Policy", PDF_PROTOCOL_CSP)
                .header(
                    "Permissions-Policy",
                    "camera=(), microphone=(), geolocation=()",
                )
                .header("Referrer-Policy", "no-referrer")
                .header("X-Content-Type-Options", "nosniff")
                .header("Cache-Control", "no-store")
                .body(body)
                .expect("valid local PDF response")
        })
        .with_navigation_handler(|url| is_pdf_navigation_url(&url))
        .with_initialization_script(include_str!("annotations.js"))
        .with_initialization_script(include_str!("read_aloud.js"))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        // 首页渲染出来之前显示的是这个底色，所以它要和 viewer 的外壳同色。
        .with_background_color(Palette::bytes(Palette::for_appearance(appearance).paper))
        .with_hotkeys_zoom(false)
        .with_incognito(true)
        .with_url(pdf_viewer_url(initial_page, compact_reading))
        .build_as_child_async(parent)
        .await
        .context("无法创建 PDF 视图")?;

    // The toolbar keeps working without the native menu, so never fail the
    // whole reader because one WebView2 interface is missing.
    #[cfg(target_os = "windows")]
    if let Err(error) = install_pdf_selection_menu(&raw_webview, sender) {
        tracing::warn!(%error, "PDF AI explanation menu unavailable");
    }

    Ok((raw_webview, receiver))
}

/// Installs the native "AI解释" context-menu command for PDF text selections.
///
/// WebView2 snapshots the selection with its page, so the command carries the
/// real selected text instead of a debounced JavaScript cache. The bridge then
/// re-reads the live range before any note is written. A click that arrives
/// after teardown only fails its send: `handle_ipc` also ignores every message
/// once the window is closing, and PDF resources are served synchronously by
/// the WebView's own protocol handler.
#[cfg(target_os = "windows")]
pub(crate) fn install_pdf_selection_menu(
    raw_webview: &wry::WebView,
    sender: async_channel::Sender<PdfIpcMessage>,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    use super::reader::selection_menu;

    selection_menu::install(
        raw_webview,
        sender,
        Arc::new(|| true),
        pdf_document_uri,
        |url, selected_text| PdfIpcMessage::ExplainSelection { selected_text, url },
        MAX_PDF_SELECTION_BYTES,
    )
    .context("无法添加 PDF 阅读器 AI 解释菜单")
}

/// Mirrors the reader's menu origin check for the private PDF viewer origin.
#[cfg(target_os = "windows")]
pub(crate) fn pdf_document_uri(value: &str) -> Option<wry::http::Uri> {
    use wry::http::Uri;

    pub(crate) const MAX_MENU_DOCUMENT_URI_BYTES: usize = 16 * 1024;
    if value.len() > MAX_MENU_DOCUMENT_URI_BYTES {
        return None;
    }
    let uri: Uri = value.parse().ok()?;
    let mut parts = uri.into_parts();
    parts.authority = Some(
        parts
            .authority?
            .as_str()
            .to_ascii_lowercase()
            .parse()
            .ok()?,
    );
    let uri = Uri::from_parts(parts).ok()?;
    is_pdf_navigation_url(&uri.to_string()).then_some(uri)
}
