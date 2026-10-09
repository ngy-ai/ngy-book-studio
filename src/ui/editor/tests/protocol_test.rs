//! Tests for the custom protocol: shell isolation, resource rewriting and
//! media responses.

use super::*;

#[test]
fn trusted_bridge_supplies_prosemirror_editor_contract() {
    assert!(
        EDITOR_INITIALIZATION_SCRIPT.starts_with("(()=>{const __ngyEditorTrustedShell=()=>{"),
        "the complete ProseMirror bundle must wait for an XHTML document element"
    );
    assert!(
        EDITOR_INITIALIZATION_SCRIPT.contains("DOMContentLoaded\",__ngyEditorStart,{once:true}")
    );
    for marker in [
        "contenteditable",
        "ProseMirror",
        "__ngyProseMirror",
        "bullet_list",
        "ordered_list",
        "table_cell",
        "restricted_html",
        "insertResource",
        "replaceSelectedResource",
        "XMLSerializer",
        "createElementNS",
        "session_id",
        "chapter_id",
        "request_id",
        "selected_text",
        "too_large",
        "epubeditor.shell",
        "epubeditor.content",
        "source",
        "credentials",
        "omit",
        "redirect",
        "error",
    ] {
        assert!(
            EDITOR_INITIALIZATION_SCRIPT.contains(marker),
            "missing {marker}"
        );
    }
    let send_installed = EDITOR_INITIALIZATION_SCRIPT
        .find("window.__ngyEditorSend=")
        .expect("snapshot bridge installation");
    let ready_sent = EDITOR_INITIALIZATION_SCRIPT
        .find("ready:!0")
        .expect("ready handshake");
    assert!(send_installed < ready_sent);
    assert!(!EDITOR_SHELL_CSP.contains("script-src 'unsafe-inline'"));
    assert!(EDITOR_SHELL_CSP.contains("connect-src http://epubeditor.content"));
    assert!(EDITOR_CONTENT_CSP.contains("connect-src 'none'"));
    assert!(EDITOR_CONTENT_CSP.contains("script-src 'none'"));
}

#[test]
fn editor_protocol_separates_trusted_shell_from_canonical_content() {
    let href = "chapter-1.xhtml".to_string();
    let html = editor_document_shell("规范化章节", "<h1>规范化正文</h1>");
    let state = EditorWebState::new("book-a".to_string(), href.clone(), html);
    let (session_id, chapter_id) = editor_ipc_identity(&state, 0);

    let document_request = wry::http::Request::builder()
        .uri(editor_custom_url(
            EditorTab::RichText,
            &session_id,
            &chapter_id,
            0,
            &href,
        ))
        .body(())
        .unwrap();
    let response = editor_protocol_response(&state, &document_request);
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/xhtml+xml; charset=utf-8"
    );
    let shell = String::from_utf8_lossy(response.body());
    assert_eq!(shell, EDITOR_TRUSTED_SHELL);
    assert!(!shell.contains("规范化正文"));
    assert_eq!(
        response.headers()["content-security-policy"],
        EDITOR_SHELL_CSP
    );
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );

    let source_url = format!(
        "epubeditor://content/{href}?mode=source&session_id={}&chapter_id={chapter_id}&rev=0",
        urlencoding::encode(&session_id)
    );
    let source_request = wry::http::Request::builder()
        .uri(&source_url)
        .header("Origin", EDITOR_SHELL_ORIGIN)
        .body(())
        .unwrap();
    let source_response = editor_protocol_response(&state, &source_request);
    assert_eq!(source_response.status(), 200);
    assert!(String::from_utf8_lossy(source_response.body()).contains("规范化正文"));
    assert_eq!(
        source_response.headers()["content-security-policy"],
        EDITOR_CONTENT_CSP
    );
    assert_eq!(
        source_response.headers()["access-control-allow-origin"],
        EDITOR_SHELL_ORIGIN
    );
    assert_eq!(
        source_response.headers()["cross-origin-resource-policy"],
        "cross-origin"
    );
    let source_without_origin =
        editor_protocol_response(&state, &protocol_request(&source_url, None));
    assert_eq!(source_without_origin.status(), 403);
    let forged_source = wry::http::Request::builder()
        .uri(&source_url)
        .header("Origin", "http://epubeditor.evil")
        .body(())
        .unwrap();
    assert_eq!(
        editor_protocol_response(&state, &forged_source).status(),
        403
    );

    let preview_request = protocol_request(
        editor_custom_url(EditorTab::Preview, &session_id, &chapter_id, 0, &href),
        None,
    );
    let preview_response = editor_protocol_response(&state, &preview_request);
    assert_eq!(preview_response.status(), 200);
    assert!(String::from_utf8_lossy(preview_response.body()).contains("规范化正文"));
    assert!(
        preview_response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );

    let stale_session = editor_protocol_response(
        &state,
        &protocol_request(
            editor_custom_url(
                EditorTab::RichText,
                "another-session",
                &chapter_id,
                0,
                &href,
            ),
            None,
        ),
    );
    assert_eq!(stale_session.status(), 404);
    let stale_chapter = editor_protocol_response(
        &state,
        &protocol_request(
            editor_custom_url(EditorTab::RichText, &session_id, "another-unit", 0, &href),
            None,
        ),
    );
    assert_eq!(stale_chapter.status(), 404);

    let legacy_origin = editor_protocol_response(
        &state,
        &protocol_request(
            format!(
                "epubeditor://book/{href}?mode=edit&session_id={session_id}&chapter_id={chapter_id}&rev=0"
            ),
            None,
        ),
    );
    assert_eq!(legacy_origin.status(), 403);

    for uri in [
        "epubeditor://content/EPUB/Styles/book.css",
        "epubeditor://content/EPUB/Images/pixel.png",
        "epubeditor://content/EPUB/Fonts/book.woff2",
    ] {
        let request = wry::http::Request::builder().uri(uri).body(()).unwrap();
        let response = editor_protocol_response(&state, &request);
        assert_eq!(response.status(), 404, "{uri}");
    }

    let missing_request = wry::http::Request::builder()
        .uri("epubeditor://content/EPUB/Images/missing.png")
        .body(())
        .unwrap();
    let missing = editor_protocol_response(&state, &missing_request);
    assert_eq!(missing.status(), 404);
}

#[test]
fn editor_protocol_rewrites_and_serves_new_media_from_the_private_origin() {
    let bytes = Arc::new(b"new-audio-bytes".to_vec());
    let asset = AssetRef::from_bytes(
        "book-a",
        AssetRole::Audio,
        "audio/mpeg",
        Some("sample.mp3".to_string()),
        bytes.as_slice(),
    );
    let document = document_with_audio("book-a", asset.clone());
    let href = "EPUB/Text/ch.xhtml".to_string();
    let html = format!(
        "<html><body><audio SRC = 'ngy-asset:{}'></audio><p>src=\"ngy-asset:{}\"</p><audio src=\"ngy-asset:unknown\"></audio></body></html>",
        asset.id, asset.id
    );
    let state = EditorWebState::new("book-a".to_string(), href.clone(), html);
    let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
    state
        .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
        .unwrap();

    let page = editor_protocol_response(
        &state,
        &protocol_request(
            editor_custom_url(EditorTab::Preview, &session_id, &chapter_id, 0, &href),
            None,
        ),
    );
    let page = String::from_utf8(page.body().to_vec()).unwrap();
    let private_path = editor_asset_path(&asset.id);
    assert!(page.contains(&format!("SRC = '{private_path}'")));
    assert!(page.contains(&format!("<p>src=\"ngy-asset:{}\"</p>", asset.id)));
    assert!(page.contains("src=\"ngy-asset:unknown\""));

    let media = editor_protocol_response(
        &state,
        &protocol_request(
            format!("epubeditor://content{}", editor_asset_path(&asset.id)),
            None,
        ),
    );
    assert_eq!(media.status(), 200);
    assert_eq!(media.headers()["content-type"], "audio/mpeg");
    assert_eq!(media.headers()["accept-ranges"], "bytes");
    assert_eq!(media.body().as_ref(), bytes.as_slice());
    let shell_media = editor_protocol_response(
        &state,
        &protocol_request(
            format!("epubeditor://shell{}", editor_asset_path(&asset.id)),
            None,
        ),
    );
    assert_eq!(shell_media.status(), 404);

    let canonical = state
        .apply_message(EditorIpcMessage {
            session_id,
            chapter_id,
            href,
            revision: 0,
            request_id: Some(7),
            body: Some(format!(
                "<body><audio src=\"{}\"></audio></body>",
                editor_asset_path(&asset.id)
            )),
            selected_text: String::new(),
            too_large: false,
            ready: false,
        })
        .unwrap();
    assert!(canonical.html.contains(&format!("ngy-asset:{}", asset.id)));
    assert!(!canonical.html.contains(EDITOR_ASSET_PATH_PREFIX));

    let canonical_absolute = state
        .apply_message(EditorIpcMessage {
            session_id: canonical.session_id,
            chapter_id: canonical.chapter_id,
            href: canonical.href,
            revision: canonical.revision,
            request_id: Some(8),
            body: Some(format!(
                "<body><audio src=\"epubeditor://content{}\"></audio></body>",
                editor_asset_path(&asset.id)
            )),
            selected_text: String::new(),
            too_large: false,
            ready: false,
        })
        .unwrap();
    assert!(
        canonical_absolute
            .html
            .contains(&format!("ngy-asset:{}", asset.id))
    );
    #[cfg(target_os = "windows")]
    {
        let canonical_windows = state
            .apply_message(EditorIpcMessage {
                session_id: canonical_absolute.session_id,
                chapter_id: canonical_absolute.chapter_id,
                href: canonical_absolute.href,
                revision: canonical_absolute.revision,
                request_id: Some(9),
                body: Some(format!(
                    "<body><audio src=\"http://epubeditor.content{}\"></audio></body>",
                    editor_asset_path(&asset.id)
                )),
                selected_text: String::new(),
                too_large: false,
                ready: false,
            })
            .unwrap();
        assert!(
            canonical_windows
                .html
                .contains(&format!("ngy-asset:{}", asset.id))
        );
    }
}

#[test]
fn editor_media_protocol_supports_seek_ranges_and_unsatisfiable_ranges() {
    let bytes = Arc::new(b"0123456789".to_vec());
    let asset = AssetRef::from_bytes(
        "book-a",
        AssetRole::Audio,
        "audio/ogg",
        None,
        bytes.as_slice(),
    );
    let document = document_with_audio("book-a", asset.clone());
    let state = EditorWebState::new(
        "book-a".to_string(),
        "EPUB/Text/ch.xhtml".to_string(),
        "<html><body></body></html>".to_string(),
    );
    state
        .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
        .unwrap();
    let uri = format!("epubeditor://content{}", editor_asset_path(&asset.id));

    let partial = editor_protocol_response(&state, &protocol_request(&uri, Some("bytes=2-5")));
    assert_eq!(partial.status(), 206);
    assert_eq!(partial.headers()["content-range"], "bytes 2-5/10");
    assert_eq!(partial.headers()["content-length"], "4");
    assert_eq!(partial.body().as_ref(), b"2345");

    let invalid = editor_protocol_response(&state, &protocol_request(&uri, Some("bytes=20-30")));
    assert_eq!(invalid.status(), 416);
    assert_eq!(invalid.headers()["content-range"], "bytes */10");
    assert_eq!(invalid.headers()["content-length"], "0");
    assert!(invalid.body().is_empty());
}

#[test]
fn editor_media_files_are_size_bounded_before_full_allocation() {
    let data_dir = tempdir().unwrap();
    let small = data_dir.path().join("small.bin");
    std::fs::write(&small, b"small-media").unwrap();
    assert_eq!(
        read_editor_media_file(&small, MediaKind::Image).unwrap(),
        b"small-media"
    );

    let oversized = data_dir.path().join("oversized.bin");
    let file = std::fs::File::create(&oversized).unwrap();
    file.set_len(MediaKind::Image.max_bytes() + 1).unwrap();
    drop(file);
    let error = read_editor_media_file(&oversized, MediaKind::Image).unwrap_err();
    assert!(error.to_string().contains("安全上限"));
    close_test_data_dir(data_dir);
}
