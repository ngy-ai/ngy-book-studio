//! Tests for the navigation URL and the XHTML body snapshots.

use super::*;

#[test]
fn parses_editor_mode_revision_and_real_chapter_path() {
    let mut valid_uris = vec![
        "epubeditor://shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=7",
    ];
    #[cfg(target_os = "windows")]
        valid_uris.push(
            "http://epubeditor.shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=7",
        );
    for uri in valid_uris {
        let uri = uri.parse().expect("valid editor URI");
        assert_eq!(
            editor_document_request(&uri),
            Some(EditorDocumentRequest {
                kind: EditorDocumentKind::TrustedShell,
                session_id: "session-a".to_string(),
                chapter_id: "unit-a".to_string(),
                revision: 7,
                href: "EPUB/Text/ch.xhtml".to_string(),
            })
        );
    }

    let preview =
        "epubeditor://content/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid preview URI");
    assert_eq!(
        editor_document_request(&preview).unwrap().kind,
        EditorDocumentKind::Preview
    );
    let source =
        "epubeditor://content/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid rich source URI");
    assert_eq!(
        editor_document_request(&source).unwrap().kind,
        EditorDocumentKind::RichSource
    );
    let forged =
        "http://epubeditor.evil/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid forged URI");
    assert!(editor_document_request(&forged).is_none());
    let forged_https =
        "https://epubeditor.shell/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid forged HTTPS URI");
    assert!(editor_document_request(&forged_https).is_none());
    let legacy =
        "epubeditor://book/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid legacy-origin URI");
    assert!(editor_document_request(&legacy).is_none());
    let duplicate = "epubeditor://shell/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&chapter_id=unit-b&rev=8"
            .parse()
            .expect("valid duplicate-query URI");
    assert!(editor_document_request(&duplicate).is_none());
    for mismatched_role in [
        "epubeditor://shell/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=8",
        "epubeditor://shell/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=8",
        "epubeditor://content/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8",
    ] {
        assert!(
            editor_document_request(&mismatched_role.parse().unwrap()).is_none(),
            "{mismatched_role}"
        );
    }
}

#[test]
fn builds_platform_editor_navigation_url() {
    let url = editor_navigation_url(
        EditorTab::RichText,
        "session-a",
        "unit-a",
        9,
        "EPUB/Text/ch.xhtml",
    );
    #[cfg(target_os = "windows")]
    assert_eq!(
        url,
        "http://epubeditor.shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=9"
    );
    #[cfg(not(target_os = "windows"))]
    assert_eq!(
        url,
        "epubeditor://shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=9"
    );

    let preview = editor_navigation_url(
        EditorTab::Preview,
        "session-a",
        "unit-a",
        10,
        "EPUB/Text/ch.xhtml",
    );
    #[cfg(target_os = "windows")]
    assert_eq!(
        preview,
        "http://epubeditor.content/EPUB/Text/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=10"
    );
    #[cfg(not(target_os = "windows"))]
    assert_eq!(
        preview,
        "epubeditor://content/EPUB/Text/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=10"
    );
    assert!(is_editor_navigation_url(&url));
    assert!(is_editor_navigation_url(&preview));
    assert!(!is_editor_navigation_url(
        "epubeditor://content/EPUB/Text/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=10"
    ));
    assert!(!is_editor_navigation_url(
        "epubeditor://book/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=10"
    ));
}

#[test]
fn body_snapshot_preserves_the_xhtml_document_shell() {
    let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>T</title><!-- <body>fake</body> --><script>keep()</script></head><xhtml:body class="book"><p>old</p></xhtml:body></html>"#;
    let replacement = r#"<body xmlns="http://www.w3.org/1999/xhtml" class="book"><p><strong>new</strong></p></body>"#;
    let updated = replace_body_element(original, replacement).expect("body is replaceable");
    assert!(updated.starts_with("<?xml version="));
    assert!(updated.contains("<!DOCTYPE html>"));
    assert!(updated.contains("<head><title>T</title>"));
    assert!(updated.contains("<script>keep()</script></head>"));
    assert!(updated.contains("<!-- <body>fake</body> -->"));
    assert!(updated.contains("<strong>new</strong>"));
    assert!(!updated.contains("<p>old</p>"));
}

#[test]
fn rich_text_snapshot_does_not_promote_head_title_into_body() {
    let snapshot = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>HEAD_ONLY_TOKEN</title></head>
  <body xmlns="http://www.w3.org/1999/xhtml">
    <h1>第一章</h1><p>BODY_ONLY_TOKEN</p>
  </body>
</html>"#;

    let parsed = parse_rich_text_snapshot(snapshot, "unit-1").expect("valid snapshot");
    let text = parsed.document.plain_text();
    assert!(!text.contains("HEAD_ONLY_TOKEN"));
    assert!(text.contains("第一章"));
    assert!(text.contains("BODY_ONLY_TOKEN"));
    assert!(!parsed.canonical_source.contains("HEAD_ONLY_TOKEN"));
}

#[test]
fn xml_scanner_recognizes_self_closing_body_elements() {
    for body in [
        "<body/>",
        "<body />",
        "<xhtml:body/>",
        "<xhtml:body data-note=\"a > b\"\n\t />",
    ] {
        assert_eq!(
            xml_body_element_range(body),
            Some((0, body.len())),
            "{body}"
        );
    }
}

#[test]
fn body_snapshot_replaces_a_self_closing_prefixed_body() {
    let original = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head/><xhtml:body class="empty"
 /></html>"#;
    let replacement = r#"<body xmlns="http://www.w3.org/1999/xhtml"><p>new</p></body>"#;

    assert_eq!(
            replace_body_element(original, replacement),
            Some(r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head/><body xmlns="http://www.w3.org/1999/xhtml"><p>new</p></body></html>"#.to_string())
        );
}

#[test]
fn body_snapshot_accepts_a_self_closing_replacement() {
    let original = r#"<html><head/><body><p>old</p></body></html>"#;
    let replacement = "<xhtml:body class=\"empty\"\n />";

    assert_eq!(
        replace_body_element(original, replacement),
        Some(
            r#"<html><head/><xhtml:body class="empty"
 /></html>"#
                .to_string()
        )
    );
}
