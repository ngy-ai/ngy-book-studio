//! Tests for selection limits, IPC and the protocol gate.

use super::*;

#[test]
pub(crate) fn reader_selection_is_bounded_normalized_and_frozen_on_the_current_chapter() {
    let spine = vec![ngy_book_studio::reader::SpineItem {
        href: "one.xhtml".to_string(),
        title: "One".to_string(),
    }];
    let locators = vec![DocumentLocator::unit("book-a", "unit-1")];
    let references = reader_reference_hints(
        "book-a",
        &spine,
        &locators,
        0,
        normalize_reader_selection("  selected\n\twords  ")
            .unwrap()
            .as_deref(),
    );

    assert_eq!(references[0].frozen_text.as_deref(), Some("selected words"));
    assert!(references[0].label.starts_with("当前章节高亮"));
    assert!(normalize_reader_selection(&"x".repeat(MAX_READER_SELECTION_BYTES + 1)).is_none());
}

#[test]
pub(crate) fn reader_ipc_accepts_only_typed_messages_from_the_private_origin() {
    let own_uri = "epubreader://book/Text/one.xhtml".parse().unwrap();
    let event = reader_ipc_event(
        &own_uri,
        r#"{"type":"selection_changed","selected_text":"  one\n two  "}"#,
    );
    assert_eq!(
        event,
        Some(ReaderWebEvent::SelectionChanged {
            url: "epubreader://book/Text/one.xhtml".to_string(),
            selected_text: Some("one two".to_string()),
        })
    );

    let forged_uri = "https://example.com/Text/one.xhtml".parse().unwrap();
    assert!(
        reader_ipc_event(
            &forged_uri,
            r#"{"type":"selection_changed","selected_text":"one"}"#
        )
        .is_none()
    );
    assert!(reader_ipc_event(&own_uri, r#"{"type":"unknown","selected_text":"one"}"#).is_none());
    assert_eq!(
        reader_ipc_event(
            &own_uri,
            r#"{"type":"citation_navigation_result","request_id":7,"found":false,"reason":"ambiguous"}"#,
        ),
        Some(ReaderWebEvent::CitationNavigationResult {
            url: "epubreader://book/Text/one.xhtml".to_string(),
            request_id: 7,
            found: false,
            reason: "ambiguous".to_string(),
        })
    );
}

#[test]
pub(crate) fn trusted_reader_bridge_only_reports_bounded_selection_changes() {
    for marker in [
        "selectionchange",
        "selection_changed",
        "MAX_SELECTION_BYTES = 32768",
        "window.top === window",
        "epubreader.book",
    ] {
        assert!(
            READER_INITIALIZATION_SCRIPT.contains(marker),
            "missing {marker}"
        );
    }
}

/// The chapter runtime clamps the same text-size range the host does, and
/// reports it under the message the host parses. Both are copies of one
/// decision, so a test has to keep them together.
#[test]
pub(crate) fn the_chapter_runtime_shares_the_host_text_size_range() {
    for marker in [
        format!("MIN_TEXT_SIZE = {READER_FONT_SIZE_MIN};"),
        format!("MAX_TEXT_SIZE = {READER_FONT_SIZE_MAX};"),
        "reader_text_size".to_string(),
        "event.preventDefault()".to_string(),
        "{ passive: false }".to_string(),
    ] {
        assert!(
            READER_INITIALIZATION_SCRIPT.contains(&marker),
            "missing {marker}"
        );
    }

    // The runtime's own step is the host's step: a notch that moved the text
    // by a different amount than the stored one would drift on every wheel.
    assert!(
        READER_INITIALIZATION_SCRIPT.contains(&format!(
            "TEXT_SIZE_STEP = {};",
            ngy_book_studio::reader::READER_FONT_SIZE_STEP
        )),
        "the chapter runtime must step by the host's text size step"
    );

    // An out-of-range report is not the chapter runtime's message, so it is
    // dropped instead of being clamped into a size the reader never chose.
    let own_uri = "epubreader://book/Text/one.xhtml".parse().unwrap();
    assert_eq!(
        reader_ipc_event(&own_uri, r#"{"type":"reader_text_size","font_size":22}"#),
        Some(ReaderWebEvent::TextSize {
            url: "epubreader://book/Text/one.xhtml".to_string(),
            font_size: 22,
        })
    );
    for rejected in ["9", "31", "-1"] {
        assert!(
            reader_ipc_event(
                &own_uri,
                &format!(r#"{{"type":"reader_text_size","font_size":{rejected}}}"#)
            )
            .is_none(),
            "font_size {rejected} must not reach the window"
        );
    }
}

#[test]
pub(crate) fn reader_http_response_preserves_range_contract_and_security_headers() {
    let response = reader_resource_response(ResourceResponse {
        status: 206,
        bytes: b"2345".to_vec(),
        mime: "audio/mpeg".to_string(),
        accept_ranges: Some("bytes"),
        content_length: 4,
        content_range: Some("bytes 2-5/10".to_string()),
    });

    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["Content-Type"], "audio/mpeg");
    assert_eq!(response.headers()["Accept-Ranges"], "bytes");
    assert_eq!(response.headers()["Content-Length"], "4");
    assert_eq!(response.headers()["Content-Range"], "bytes 2-5/10");
    assert_eq!(response.headers()["X-Content-Type-Options"], "nosniff");
    assert_eq!(response.headers()["Content-Security-Policy"], READER_CSP);
    assert_eq!(response.body().as_ref(), b"2345");
}

#[test]
pub(crate) fn reader_protocol_gate_revokes_late_responses() {
    let gate = ReaderProtocolGate::new();
    let callback = gate.clone();
    assert!(callback.is_open());
    gate.close();
    assert!(!callback.is_open());
}
