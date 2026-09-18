//! Tests for the PDF annotation flow.

use super::super::*;
use super::*;

pub(crate) fn action_body(action: &str, extra: &str) -> String {
    format!(
        r#"{{"type":"annotation_action","action":"{action}","session":"1","revision":3,"request_id":1,"page":7{extra}}}"#
    )
}

pub(crate) fn parse(body: &str) -> Option<PdfAnnotationAction> {
    match serde_json::from_str::<PdfIpcMessage>(body).ok()? {
        PdfIpcMessage::AnnotationAction { action } => action.valid().then_some(action),
        _ => None,
    }
}

/// An imported book that was never edited carries `Revision::INITIAL` (`0`)
/// for both its document and its pages. Reading that as "no canonical
/// identity" silently hid the selection toolbar on every such book.
#[test]
pub(crate) fn any_canonical_page_owns_notes_including_the_first_imported_revision() {
    let imported = PdfReaderPage {
        unit_id: Some("unit-1".to_string()),
        unit_index: Some(0),
        unit_revision: 0,
        title: "Page 1".to_string(),
        page_number: 1,
    };
    assert_eq!(
        pdf_note_position(&[imported.clone()], 1),
        Some(PdfNotePosition {
            unit_id: "unit-1".to_string(),
            unit_revision: 0,
            page_number: 1,
        })
    );
    assert_eq!(pdf_note_position(&[imported.clone()], 2), None);
    // The canonical unit ID alone decides ownership; a page number without
    // one stays a preview, and the spine index is not notes metadata.
    for unit_id in [None, Some("   ".to_string())] {
        assert!(
            pdf_note_position(
                &[PdfReaderPage {
                    unit_id,
                    ..imported.clone()
                }],
                1
            )
            .is_none(),
            "a page without canonical identity cannot own notes"
        );
    }
    assert_eq!(
        pdf_note_position(
            &[PdfReaderPage {
                unit_index: None,
                ..imported
            }],
            1
        )
        .map(|position| position.unit_id),
        Some("unit-1".to_string()),
        "a unit index is not required to own notes"
    );
}

#[test]
pub(crate) fn typed_note_ipc_bounds_untrusted_content_and_forbids_ai_authorship() {
    let body = action_body(
        "human_comment",
        r#","anchor":{"quote":"选区","start":0,"end":2},"content":"人工想法""#,
    );
    let action = parse(&body).expect("valid human thought action");
    assert_eq!(action.action, PdfNoteAction::HumanComment);
    assert_eq!(action.page, 7);
    assert_eq!(action.revision, 3);

    for invalid in [
        body.replace("human_comment", "ai_comment"),
        body.replace(r#""request_id":1"#, r#""request_id":0"#),
        body.replace(r#""page":7"#, r#""page":0"#),
        body.replace(r#","page":7"#, ""),
        body.replace("人工想法", &"x".repeat(64 * 1024 + 1)),
        body.replace(r#""end":2"#, r#""end":0"#),
        body.replace(r#""quote":"选区""#, r#""quote":"  ""#),
        body.replace(
            r#""quote":"选区""#,
            &format!(r#""quote":"{}""#, "x".repeat(MAX_PDF_SELECTION_BYTES + 1)),
        ),
    ] {
        assert!(parse(&invalid).is_none(), "{invalid}");
    }
}

#[test]
pub(crate) fn remove_mark_ipc_requires_a_range_and_cannot_target_a_thought_by_id() {
    let body = action_body(
        "remove_mark",
        r#","anchor":{"quote":"选区","start":0,"end":2}"#,
    );
    assert_eq!(
        parse(&body).expect("valid mark removal").action,
        PdfNoteAction::RemoveMark
    );
    for invalid in [
        action_body(
            "strikethrough",
            r#","anchor":{"quote":"选区","start":0,"end":2}"#,
        ),
        action_body("remove_mark", ""),
        body.replace(r#""page":7"#, r#""page":7,"id":"human-note""#),
        body.replace(r#""page":7"#, r#""page":7,"content":"想法""#),
    ] {
        assert!(parse(&invalid).is_none(), "{invalid}");
    }
}

/// The injected bridge is the only page-side author of note requests, so
/// its contract with this host is pinned here: private origin, page-local
/// PDF.js text layer scope, a declared reading window and a page number on
/// every request.
#[test]
pub(crate) fn trusted_pdf_bridge_binds_every_request_to_a_rendered_page() {
    let bridge = include_str!("../annotations.js");
    for contract in [
        "window.top === window",
        "ngypdf.viewer",
        "\"ngypdf:\"",
        "document.querySelector(`.pdf-page[data-page=\"${page}\"]`)",
        "querySelector(\".textLayer\")",
        "attachShadow({ mode: \"closed\" })",
        "type: \"annotation_action\"",
        "page, request_id: requestId, ...fields",
        "postSilent(\"pages_rendered\", { pages: declaredPages })",
        "setPages(pages)",
        "lockedPage()",
        "disable()",
    ] {
        assert!(
            bridge.contains(contract),
            "missing bridge contract: {contract}"
        );
    }
    // The page may only ask for an explanation; the AI thought itself is
    // written by this host after the answer is saved.
    assert!(!bridge.contains(r#"post("ai_comment""#));
    assert!(!bridge.contains(r#"choose("ai_comment")"#));
}

/// The continuous viewer declares the pages it has mounted; the host serves
/// a page request only while it is inside that window.
#[test]
pub(crate) fn rendered_window_ipc_is_bounded_and_requires_the_declared_pages() {
    let body = r#"{"type":"annotation_action","action":"pages_rendered","session":"1","revision":0,"request_id":1,"page":7,"pages":[3,4,5]}"#;
    let action = parse(body).expect("valid rendered-window declaration");
    assert_eq!(action.action, PdfNoteAction::PagesRendered);
    assert_eq!(action.pages, vec![3, 4, 5]);

    for invalid in [
        body.replace(r#""pages":[3,4,5]"#, r#""pages":[]"#),
        body.replace(r#""pages":[3,4,5]"#, r#""pages":[0]"#),
        body.replace(
            r#""pages":[3,4,5]"#,
            &format!(r#""pages":[{}0]"#, "1,".repeat(MAX_ANNOTATION_PAGES)),
        ),
    ] {
        assert!(parse(&invalid).is_none(), "{invalid}");
    }

    let list = r#"{"type":"annotation_action","action":"list","session":"1","revision":0,"request_id":2,"page":3,"pages":[3,4]}"#;
    let action = parse(list).expect("valid multi-page listing");
    assert_eq!(action.action, PdfNoteAction::List);
    assert_eq!(action.pages, vec![3, 4]);
}

#[test]
pub(crate) fn thoughts_render_gfm_but_preserve_both_authors_markdown_source() {
    let source = "## 标题\n\n**重点**\n\n- 一项\n\n> 引用\n\n```rust\nlet value = 1;\n```\n\n| 类别 | 内容 |\n| --- | --- |\n| 人工 | 想法 |";
    let records = [AnnotationKind::HumanComment, AnnotationKind::AiComment]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| Annotation {
            id: format!("note-{index}"),
            book_id: "book".into(),
            content_unit_id: "unit".into(),
            document_revision: 1,
            unit_revision: 1,
            anchor: TextAnchor {
                quote: "引文".into(),
                start: 0,
                end: 2,
            },
            kind,
            comment: Some(source.into()),
            created_at: 1,
            updated_at: 1,
            stale: false,
        })
        .collect();
    let payload = notes_json(records);
    for entry in payload.as_array().unwrap() {
        assert_eq!(entry["content"], source);
        let html = entry["content_html"].as_str().unwrap();
        for expected in [
            "<h2>标题</h2>",
            "<strong>重点</strong>",
            "<ul>",
            "<blockquote>",
            "<pre><code",
            "<table>",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
    }
    assert_eq!(payload[0]["content_html"], payload[1]["content_html"]);
}

#[test]
pub(crate) fn thought_html_never_activates_model_links_images_or_raw_html() {
    let html = thought_display_html(
        "[链接](https://example.invalid/x)\n\n![图片说明](https://example.invalid/image.png)\n\n<img src=x onerror=alert(1)>\n\n<script>alert(2)</script>",
    );
    for forbidden in ["<a ", "<img", "<script", "<iframe", "href=", "src=\"https:"] {
        assert!(!html.contains(forbidden), "{forbidden}: {html}");
    }
    assert!(html.contains("链接"));
    assert!(html.contains("图片说明"));
    assert!(html.contains("&lt;script"));
}

#[test]
pub(crate) fn explanation_reference_freezes_the_selection_on_the_current_page_only() {
    let pages = vec![
        PdfReaderPage {
            unit_id: Some("unit-1".to_string()),
            unit_index: Some(0),
            unit_revision: 1,
            title: "One".to_string(),
            page_number: 1,
        },
        PdfReaderPage {
            unit_id: Some("unit-2".to_string()),
            unit_index: Some(1),
            unit_revision: 4,
            title: "Two".to_string(),
            page_number: 2,
        },
        PdfReaderPage {
            unit_id: None,
            unit_index: None,
            unit_revision: 0,
            title: "Ephemeral".to_string(),
            page_number: 3,
        },
    ];

    let reference =
        pdf_explanation_reference("book-a", &pages, 2, "  所选\n\t文本  ").expect("reference");
    assert_eq!(reference.unit_id, "unit-2");
    assert_eq!(reference.frozen_text.as_deref(), Some("所选 文本"));
    assert_eq!(
        reference.locator,
        Some(DocumentLocator::unit("book-a", "unit-2").with_source(SourceLocator::pdf_page(2)))
    );

    assert!(pdf_explanation_reference("book-a", &pages, 2, " \n\t ").is_none());
    assert!(
        pdf_explanation_reference("book-a", &pages, 3, "临时页面").is_none(),
        "a page without canonical identity cannot become an AI reference"
    );
    assert!(pdf_explanation_reference("book-a", &pages, 9, "缺失页面").is_none());
}
