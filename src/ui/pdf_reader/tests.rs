//! Tests for the PDF reader.

use super::*;

#[test]
pub(crate) fn protocol_only_exposes_the_document_and_checked_in_assets() {
    let pdf = b"%PDF-test";
    let (mime, bytes) = pdf_resource(pdf, "/document.pdf").expect("document route");
    assert_eq!(mime, "application/pdf");
    assert_eq!(bytes.as_ref(), pdf);
    assert!(pdf_resource(pdf, "/viewer.html").is_some());
    assert!(pdf_resource(pdf, "/../document.pdf").is_none());
    assert!(pdf_resource(pdf, "/%2e%2e/pdf.mjs").is_none());
    assert!(pdf_resource(pdf, "/https://example.com/file").is_none());
    assert!(pdf_resource(pdf, "/wasm/quickjs-eval.js").is_none());
    assert!(pdf_resource(pdf, "/wasm/quickjs-eval.wasm").is_none());
}

#[test]
pub(crate) fn protocol_navigation_stays_on_the_private_origin() {
    assert!(is_pdf_navigation_url("ngypdf://viewer/viewer.html"));
    assert!(is_pdf_navigation_url("http://ngypdf.viewer/pdf.mjs"));
    assert!(!is_pdf_navigation_url("https://example.com/document.pdf"));
    assert!(!is_pdf_navigation_url("ngypdf://attacker/viewer.html"));
}

#[test]
pub(crate) fn compact_reading_only_changes_the_viewer_url_when_enabled() {
    assert_eq!(
        pdf_viewer_url(7, false),
        "ngypdf://viewer/viewer.html?startPage=7"
    );
    assert_eq!(
        pdf_viewer_url(7, true),
        "ngypdf://viewer/viewer.html?startPage=7&compact=1"
    );
    // A parameter must never leave the private origin or its viewer path.
    assert!(is_pdf_navigation_url(&pdf_viewer_url(7, true)));
}

/// The URL parameter and the live update must name the same attribute, or an
/// already open reader would show a different gap than a freshly opened one.
#[test]
pub(crate) fn compact_reading_script_toggles_the_same_document_attribute() {
    let enabled = pdf_compact_reading_script(true);
    assert!(enabled.contains("setAttribute(\"data-pdf-compact\", \"1\")"));
    assert!(enabled.contains("document.documentElement"));
    let disabled = pdf_compact_reading_script(false);
    assert!(disabled.contains("removeAttribute(\"data-pdf-compact\")"));
    assert!(disabled.contains("document.documentElement"));
}

#[test]
pub(crate) fn ipc_page_change_is_typed() {
    let message = serde_json::from_str::<PdfIpcMessage>(
            r#"{"type":"ngy-pdf-page-changed","requestId":7,"pageNumber":3,"pageCount":9,"pdfjsVersion":"5.7.284"}"#,
        )
        .expect("typed page message");
    assert!(matches!(
        message,
        PdfIpcMessage::PageChanged {
            request_id: 7,
            page_number: 3,
            page_count: 9
        }
    ));
}

/// The bundled viewer steps and clamps the same page scale the host stores,
/// and reports it under the message the host parses. Both are copies of one
/// decision, so a test has to keep them together.
#[test]
pub(crate) fn the_bundled_viewer_zooms_in_the_host_range() {
    let viewer = ngy_book_studio::preview::PDFJS_VIEWER_SCRIPT;
    for expected in [
        format!("PAGE_ZOOM_DEFAULT_MILLI = {PDF_ZOOM_DEFAULT_MILLI};"),
        format!("PAGE_ZOOM_MIN_MILLI = {PDF_ZOOM_MIN_MILLI};"),
        format!("PAGE_ZOOM_MAX_MILLI = {PDF_ZOOM_MAX_MILLI};"),
        "\"ngy-pdf-zoom-changed\"".to_string(),
    ] {
        assert!(
            viewer.contains(&expected),
            "the bundled viewer is missing {expected}"
        );
    }
    // The URL the host opens is the only scale the viewer starts from, and
    // the scale it reports back is a number the host can clamp.
    let script = pdf_zoom_script(PDF_ZOOM_DEFAULT_MILLI);
    assert!(script.contains("window.postMessage"));
    assert!(script.contains(&format!("\"zoomMilli\":{PDF_ZOOM_DEFAULT_MILLI}")));
    assert!(
        script.contains("\"type\":\"ngy-pdf-zoom\""),
        "the viewer must receive the same message type it waits for"
    );

    assert_eq!(pdf_zoom_milli(0), PDF_ZOOM_MIN_MILLI);
    assert_eq!(pdf_zoom_milli(u32::MAX), PDF_ZOOM_MAX_MILLI);
    assert_eq!(
        pdf_zoom_milli(PDF_ZOOM_DEFAULT_MILLI),
        PDF_ZOOM_DEFAULT_MILLI
    );

    let changed = serde_json::from_str::<PdfIpcMessage>(
        r#"{"type":"ngy-pdf-zoom-changed","zoomMilli":1750,"pdfjsVersion":"5.7.284"}"#,
    )
    .expect("typed page scale message");
    assert!(matches!(
        changed,
        PdfIpcMessage::ZoomChanged { zoom_milli: 1_750 }
    ));
}

#[test]
pub(crate) fn pdf_progress_requires_the_exact_canonical_unit_locator() {
    let canonical = PdfReaderPage {
        unit_id: Some("unit-3".to_string()),
        unit_index: Some(7),
        unit_revision: 1,
        title: "Three".to_string(),
        page_number: 3,
    };
    assert_eq!(
        pdf_progress_write(3, Some(&canonical)),
        Ok(ReadingProgressWrite::Unit {
            spine_index: 7,
            unit_id: "unit-3".to_string(),
        })
    );
    assert!(pdf_progress_write(2, Some(&canonical)).is_err());
    assert!(pdf_progress_write(3, None).is_err());

    let missing_unit = PdfReaderPage {
        unit_id: None,
        ..canonical.clone()
    };
    assert!(pdf_progress_write(3, Some(&missing_unit)).is_err());

    let missing_index = PdfReaderPage {
        unit_index: None,
        ..canonical
    };
    assert!(pdf_progress_write(3, Some(&missing_index)).is_err());
}

#[test]
pub(crate) fn ipc_selection_change_is_typed_and_bounded() {
    let message = serde_json::from_str::<PdfIpcMessage>(
            r#"{"type":"ngy-pdf-selection-changed","requestId":7,"pageNumber":3,"selectedText":"  first\n\tsecond  ","pdfjsVersion":"5.7.284"}"#,
        )
        .expect("typed selection message");
    let PdfIpcMessage::SelectionChanged {
        request_id,
        page_number,
        selected_text,
    } = message
    else {
        panic!("expected selection message");
    };
    let selection = validated_pdf_selection(7, 9, request_id, page_number, &selected_text)
        .expect("current request and page")
        .expect("non-empty selection");
    assert_eq!(selection.text, "first second");
    assert_eq!(selection.page_number, 3);

    // A selection on another page of the same document keeps its own page.
    let other = validated_pdf_selection(7, 9, 7, 4, "another page")
        .expect("page inside the document")
        .expect("non-empty selection");
    assert_eq!(other.page_number, 4);

    assert!(validated_pdf_selection(8, 9, request_id, page_number, &selected_text).is_none());
    assert!(validated_pdf_selection(7, 9, 7, 0, "selected").is_none());
    assert!(validated_pdf_selection(7, 9, 7, 10, "selected").is_none());
    assert!(
        validated_pdf_selection(7, 9, 7, 3, &"x".repeat(MAX_PDF_SELECTION_BYTES + 1)).is_none()
    );
    assert_eq!(validated_pdf_selection(7, 9, 7, 3, " \n\t "), Some(None));
}

#[test]
pub(crate) fn ai_reference_options_include_current_then_all_canonical_pages() {
    let pages = vec![
        PdfReaderPage {
            unit_id: Some("unit-1".to_string()),
            unit_index: Some(0),
            unit_revision: 1,
            title: "One".to_string(),
            page_number: 1,
        },
        PdfReaderPage {
            unit_id: None,
            unit_index: None,
            unit_revision: 0,
            title: "Ephemeral".to_string(),
            page_number: 2,
        },
        PdfReaderPage {
            unit_id: Some("unit-3".to_string()),
            unit_index: Some(2),
            unit_revision: 3,
            title: "Three".to_string(),
            page_number: 3,
        },
    ];

    let references = pdf_reference_hints("book-a", &pages, 3, 0, None);

    assert_eq!(
        references
            .iter()
            .map(|reference| reference.unit_id.as_str())
            .collect::<Vec<_>>(),
        vec!["unit-3", "unit-1"]
    );
    assert!(references[0].label.starts_with("当前页"));
    assert_eq!(
        references[0].locator,
        Some(DocumentLocator::unit("book-a", "unit-3").with_source(SourceLocator::pdf_page(3)))
    );

    let source = AiSourceLink {
        citation_id: "source-3".to_string(),
        book_id: "book-a".to_string(),
        unit_id: "unit-3".to_string(),
        unit_index: None,
        document_revision: ngy_book_studio::document::Revision::new(1),
        unit_revision: ngy_book_studio::document::Revision::new(1),
        locator: references[0].locator.clone(),
        label: "Page 3".to_string(),
        quote: None,
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    assert_eq!(pdf_page_for_source(&pages, &source), Some(3));

    let stale = AiSourceLink {
        locator: Some(
            DocumentLocator::unit("book-a", "unit-3").with_source(SourceLocator::pdf_page(99)),
        ),
        ..source.clone()
    };
    assert_eq!(
        pdf_page_for_source(&pages, &stale),
        None,
        "a stale exact PDF locator must not degrade to the unit's first page"
    );

    let wrong_source_type = AiSourceLink {
        locator: Some(
            DocumentLocator::unit("book-a", "unit-3").with_source(SourceLocator::slide(3)),
        ),
        ..source
    };
    assert_eq!(pdf_page_for_source(&pages, &wrong_source_type), None);
    assert!(
        pdf_source_requires_exact_page(&wrong_source_type),
        "a supplied non-PDF source locator must be rejected instead of using the unit fallback"
    );
}

#[test]
pub(crate) fn current_pdf_selection_freezes_only_the_current_page_reference() {
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
            unit_revision: 2,
            title: "Two".to_string(),
            page_number: 2,
        },
    ];
    let selection = PdfTextSelection {
        request_id: 11,
        page_number: 2,
        text: "exact highlighted text".to_string(),
    };

    let references = pdf_reference_hints("book-a", &pages, 2, 11, Some(&selection));

    assert_eq!(references[0].unit_id, "unit-2");
    assert!(references[0].label.starts_with("选中高亮"));
    assert_eq!(
        references[0].frozen_text.as_deref(),
        Some("exact highlighted text")
    );
    assert_eq!(references[1].frozen_text, None);

    let stale_references = pdf_reference_hints("book-a", &pages, 2, 12, Some(&selection));
    assert_eq!(stale_references[0].frozen_text, None);
    assert!(stale_references[0].label.starts_with("当前页 ·"));
}
