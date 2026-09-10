use super::is_reader_document_uri;
use anyhow::Context as _;
use gpui_component::wry::{WebView, WebViewExtWindows, http::Uri};
use std::sync::Arc;
use webview2_com::{
    CoTaskMemPWSTR, ContextMenuRequestedEventHandler, CustomItemSelectedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND, ICoreWebView2_11,
        ICoreWebView2ContextMenuRequestedEventArgs, ICoreWebView2Environment9,
    },
};
use windows::core::{BOOL, Interface, PWSTR, w};

const MAX_MENU_DOCUMENT_URI_BYTES: usize = 16 * 1024;

/// The WebView owns its context-menu handler; each menu owns its click handler.
/// Neither callback retains the WebView/controller or a GPUI entity, so teardown
/// releases the callbacks without a COM reference cycle. The shared gate also
/// rejects a click from a menu that was opened before shutdown. The private
/// document check and the event payload are supplied by the caller so the same
/// menu serves both the EPUB reader and the PDF reader.
pub(crate) fn install<E: Send + 'static>(
    raw_webview: &WebView,
    event_sender: async_channel::Sender<E>,
    gate: Arc<dyn Fn() -> bool + Send + Sync>,
    document_uri: fn(&str) -> Option<Uri>,
    explain: fn(String, String) -> E,
    max_selection_bytes: usize,
) -> anyhow::Result<()> {
    let webview: ICoreWebView2_11 = raw_webview
        .webview()
        .cast()
        .context("当前 WebView2 不支持阅读选区右键菜单，请更新 WebView2 Runtime")?;
    let environment: ICoreWebView2Environment9 = raw_webview
        .environment()
        .cast()
        .context("当前 WebView2 不支持 AI 解释菜单，请更新 WebView2 Runtime")?;
    let handler = ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
        if !gate() {
            tracing::debug!(
                target: "moye_ai",
                stage = "context_menu_closed",
                "AI explanation menu ignored after shutdown"
            );
            return Ok(());
        }
        let Some(args) = args else {
            tracing::debug!(
                target: "moye_ai",
                stage = "context_menu_missing_args",
                "AI explanation menu received no target"
            );
            return Ok(());
        };
        tracing::debug!(
            target: "moye_ai",
            stage = "context_menu_requested",
            "AI explanation menu requested"
        );
        let result = append_explain_item(
            &environment,
            &args,
            &event_sender,
            &gate,
            document_uri,
            explain,
            max_selection_bytes,
        );
        if let Err(error) = &result {
            tracing::warn!(
                target: "moye_ai",
                stage = "context_menu_failed",
                hresult = error.code().0,
                "AI explanation menu could not be added"
            );
        }
        result
    }));
    let mut token = 0;
    // Registration lasts exactly as long as this WebView. The handler does not
    // need to be removed separately because it never owns its event source.
    unsafe { webview.add_ContextMenuRequested(&handler, &mut token) }
        .context("注册阅读选区 AI 解释菜单失败")?;
    tracing::debug!(
        target: "moye_ai",
        stage = "context_menu_registered",
        "AI explanation menu registered"
    );
    Ok(())
}

fn append_explain_item<E: Send + 'static>(
    environment: &ICoreWebView2Environment9,
    args: &ICoreWebView2ContextMenuRequestedEventArgs,
    event_sender: &async_channel::Sender<E>,
    gate: &Arc<dyn Fn() -> bool + Send + Sync>,
    document_uri: fn(&str) -> Option<Uri>,
    explain: fn(String, String) -> E,
    max_selection_bytes: usize,
) -> windows::core::Result<()> {
    // WebView2 snapshots both the selection and its page for this menu request.
    // Never use the debounced JavaScript selection cache here: it can describe
    // the previous selection when the user opens the menu immediately.
    let target = unsafe { args.ContextMenuTarget()? };
    let mut has_selection = BOOL::default();
    let mut is_editable = BOOL::default();
    let mut is_main_frame = BOOL::default();
    unsafe {
        target.HasSelection(&mut has_selection)?;
        target.IsEditable(&mut is_editable)?;
        target.IsRequestedForMainFrame(&mut is_main_frame)?;
    }
    tracing::debug!(
        target: "moye_ai",
        stage = "context_menu_target",
        has_selection = has_selection.as_bool(),
        is_editable = is_editable.as_bool(),
        is_main_frame = is_main_frame.as_bool(),
        "AI explanation menu target inspected"
    );
    if !has_selection.as_bool() || is_editable.as_bool() {
        return Ok(());
    }

    let mut page_uri = PWSTR::null();
    unsafe { target.PageUri(&mut page_uri)? };
    let url = CoTaskMemPWSTR::from(page_uri).to_string();
    let mut frame_uri = PWSTR::null();
    unsafe { target.FrameUri(&mut frame_uri)? };
    let frame_url = CoTaskMemPWSTR::from(frame_uri).to_string();
    // Real Windows child-WebView requests can report is_main_frame=false for
    // the visible reader document. Validate the native page and frame URLs
    // independently and require the same private document. The CSP forbids
    // child frames, so a different frame must never become a selection source.
    let page_document = document_uri(&url);
    let frame_document = document_uri(&frame_url);
    let origin_accepted = matching_documents(page_document.as_ref(), frame_document.as_ref());
    tracing::debug!(
        target: "moye_ai",
        stage = "context_menu_origin",
        page_private = page_document.is_some(),
        frame_private = frame_document.is_some(),
        origin_accepted,
        "AI explanation menu origin checked"
    );
    if !origin_accepted {
        return Ok(());
    }
    let mut selection = PWSTR::null();
    unsafe { target.SelectionText(&mut selection)? };
    let selection = CoTaskMemPWSTR::from(selection).to_string();
    tracing::debug!(
        target: "moye_ai",
        stage = "context_menu_selection",
        selection_bytes = selection.len(),
        "AI explanation menu selection captured"
    );
    let Some(selected_text) = bounded_selection(&selection, max_selection_bytes) else {
        return Ok(());
    };

    let item = unsafe {
        environment.CreateContextMenuItem(
            w!("AI解释"),
            None,
            COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
        )?
    };
    let event_sender = event_sender.clone();
    let gate = gate.clone();
    let handler = CustomItemSelectedEventHandler::create(Box::new(move |_, _| {
        if gate() {
            tracing::debug!(
                target: "moye_ai",
                stage = "context_menu_selected",
                "AI explanation menu command selected"
            );
            // A closed receiver means that the reader has already been released.
            let _ = event_sender.try_send(explain(url.clone(), selected_text.clone()));
        }
        Ok(())
    }));
    let mut token = 0;
    unsafe {
        item.add_CustomItemSelected(&handler, &mut token)?;
        let items = args.MenuItems()?;
        let mut count = 0;
        items.Count(&mut count)?;
        items.InsertValueAtIndex(count, &item)?;
    }
    tracing::debug!(
        target: "moye_ai",
        stage = "context_menu_added",
        "AI explanation menu command added"
    );
    Ok(())
}

/// The EPUB reader's private document check, exposed so `reader.rs` can hand it
/// to the shared menu installer as a plain function pointer.
pub(super) fn reader_document_uri(value: &str) -> Option<Uri> {
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
    is_reader_document_uri(&uri).then_some(uri)
}

fn matching_documents(page: Option<&Uri>, frame: Option<&Uri>) -> bool {
    // http::Uri normalizes scheme/host case and omits fragments. Path, query,
    // port and origin must still match; different private documents do not.
    page.zip(frame).is_some_and(|(page, frame)| page == frame)
}

fn bounded_selection(value: &str, max_bytes: usize) -> Option<String> {
    let mut normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() > max_bytes {
        let mut end = max_bytes;
        while !normalized.is_char_boundary(end) {
            end -= 1;
        }
        normalized.truncate(end);
    }
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::super::MAX_READER_SELECTION_BYTES;
    use super::*;

    #[test]
    fn menu_selection_normalizes_whitespace_and_omits_empty_text() {
        assert_eq!(
            bounded_selection("  一段\n\t文本  ", MAX_READER_SELECTION_BYTES),
            Some("一段 文本".into())
        );
        assert_eq!(
            bounded_selection(" \t\n\u{3000}", MAX_READER_SELECTION_BYTES),
            None
        );
    }

    #[test]
    fn menu_selection_keeps_a_valid_utf8_prefix_at_the_selection_limit() {
        let selection = "中".repeat(MAX_READER_SELECTION_BYTES);
        let result = bounded_selection(&selection, MAX_READER_SELECTION_BYTES).unwrap();
        assert!(result.len() <= MAX_READER_SELECTION_BYTES);
        assert_eq!(result, "中".repeat(MAX_READER_SELECTION_BYTES / 3));
    }

    #[test]
    fn menu_target_requires_the_same_private_page_and_frame_document() {
        for (page, frame) in [
            (
                "epubreader://book/Text/one.xhtml",
                "epubreader://book/Text/one.xhtml",
            ),
            (
                "http://epubreader.book/Text/one.xhtml#one",
                "http://epubreader.book/Text/one.xhtml#two",
            ),
            (
                "https://epubreader.book/Text/one.xhtml",
                "https://EPUBREADER.BOOK/Text/one.xhtml",
            ),
        ] {
            assert!(matching_documents(
                reader_document_uri(page).as_ref(),
                reader_document_uri(frame).as_ref()
            ));
        }
    }

    #[test]
    fn menu_target_rejects_other_documents_external_frames_and_oversized_uris() {
        let page = reader_document_uri("http://epubreader.book/Text/one.xhtml");
        for frame in [
            "http://epubreader.book/Text/two.xhtml",
            "http://epubreader.book/Text/one.xhtml?other=1",
            "https://epubreader.book/Text/one.xhtml",
            "http://epubreader.book:1234/Text/one.xhtml",
            "https://example.com/Text/one.xhtml",
            "http://epubreader.book.evil/Text/one.xhtml",
            "about:blank",
            "",
        ] {
            let frame = reader_document_uri(frame);
            assert!(!matching_documents(page.as_ref(), frame.as_ref()));
            assert!(!matching_documents(frame.as_ref(), page.as_ref()));
        }
        let oversized = format!(
            "http://epubreader.book/{}",
            "a".repeat(MAX_MENU_DOCUMENT_URI_BYTES)
        );
        assert!(reader_document_uri(&oversized).is_none());
        assert!(!matching_documents(None, None));
    }
}
