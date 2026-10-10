//! Tests for references, zoom and page text escaping.

use super::super::*;
use super::*;

#[test]
fn references_and_source_links_keep_exact_pages_for_repeated_units() {
    let pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![
            section_page("book", "unit-1", 1, 1),
            section_page("book", "unit-1", 2, 2),
            worksheet_page("book", "unit-2", 3, "汇总", "A1:C8"),
        ],
    )
    .expect("valid persisted pages");
    let references = page_reference_hints("book", &pages, 1);
    assert_eq!(references.len(), 3);
    assert_eq!(references[0].unit_id, pages[1].locator.unit_id);
    assert_eq!(references[0].label, "当前页面 · 第 2 页");
    assert_eq!(references[0].locator.as_ref(), Some(&pages[1].locator));
    assert_eq!(references[1].unit_id, pages[0].locator.unit_id);
    assert_eq!(references[1].locator.as_ref(), Some(&pages[0].locator));
    assert_eq!(references[2].unit_id, pages[2].locator.unit_id);
    assert!(references.iter().all(|reference| {
        reference.book_id == "book"
            && reference.unit_index.is_none()
            && reference.frozen_text.is_none()
    }));

    let exact_source = AiSourceLink {
        citation_id: "citation-1".to_string(),
        book_id: "book".to_string(),
        unit_id: "unit-1".to_string(),
        unit_index: None,
        document_revision: ngy_book_studio::document::Revision::new(1),
        unit_revision: ngy_book_studio::document::Revision::new(1),
        locator: Some(pages[0].locator.clone()),
        label: "第一页".to_string(),
        quote: None,
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    assert_eq!(
        page_index_for_source(
            &pages,
            VisualPageSourceKind::OfficeEnhanced,
            1,
            &exact_source
        ),
        Some(0),
        "an exact locator must win over the current page with the same unit ID"
    );

    let exact_source_coordinate = pages[0].locator.source.clone().unwrap();
    let differing_locators = [
        DocumentLocator::block("book", "unit-1", "block-1")
            .with_source(exact_source_coordinate.clone()),
        DocumentLocator::text("book", "unit-1", "block-1", 0, 1)
            .with_source(exact_source_coordinate.clone()),
        pages[0]
            .locator
            .clone()
            .with_region(ngy_book_studio::document::NormalizedRect::new(
                0, 0, 100, 100,
            )),
    ];
    for locator in differing_locators {
        let differing_source = AiSourceLink {
            locator: Some(locator),
            ..exact_source.clone()
        };
        assert_eq!(
            page_index_for_source(
                &pages,
                VisualPageSourceKind::OfficeEnhanced,
                1,
                &differing_source
            ),
            None,
            "an exact source coordinate must not ignore block, text range, or region"
        );
    }

    let ambiguous_pages = vec![
        PageImage {
            file_name: "Page00001.png".to_string(),
            page_number: 1,
            width: 800,
            height: 1_200,
            image: Arc::clone(&pages[0].image),
            locator: pages[0].locator.clone(),
        },
        PageImage {
            file_name: "Page00002.png".to_string(),
            page_number: 2,
            width: 800,
            height: 1_200,
            image: Arc::clone(&pages[0].image),
            locator: pages[0].locator.clone(),
        },
    ];
    assert_eq!(
        page_index_for_source(
            &ambiguous_pages,
            VisualPageSourceKind::OfficeEnhanced,
            0,
            &exact_source
        ),
        None,
        "an ambiguous exact locator must be rejected"
    );

    let wrong_source_type = AiSourceLink {
        locator: Some(
            DocumentLocator::unit("book", "unit-1").with_source(SourceLocator::pdf_page(1)),
        ),
        ..exact_source.clone()
    };
    assert_eq!(
        page_index_for_source(
            &pages,
            VisualPageSourceKind::OfficeEnhanced,
            1,
            &wrong_source_type
        ),
        None,
        "a supplied non-Office source locator must not use the unit fallback"
    );

    let unit_only_source = AiSourceLink {
        locator: None,
        ..exact_source
    };
    assert_eq!(
        page_index_for_source(
            &pages,
            VisualPageSourceKind::OfficeEnhanced,
            1,
            &unit_only_source
        ),
        Some(1),
        "unit-only citations retain the previous current-page behavior"
    );

    let block_source = AiSourceLink {
        locator: Some(DocumentLocator::block("book", "unit-1", "block-1")),
        ..unit_only_source.clone()
    };
    assert_eq!(
        page_index_for_source(
            &pages,
            VisualPageSourceKind::OfficeEnhanced,
            1,
            &block_source
        ),
        Some(1),
        "a block locator without a visual source coordinate remains a unit-level target"
    );

    let stale_source = AiSourceLink {
        locator: Some(
            DocumentLocator::unit("book", "unit-1")
                .with_source(SourceLocator::office_rendered_page(99)),
        ),
        ..unit_only_source
    };
    assert_eq!(
        page_index_for_source(
            &pages,
            VisualPageSourceKind::OfficeEnhanced,
            1,
            &stale_source
        ),
        None,
        "a preview-only locator must not silently fall back to a guessed unit page"
    );
}

#[test]
fn repaginated_office_pages_are_preview_only_and_not_ai_references() {
    let pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![
            rendered_page("book", "unit-1", 1),
            rendered_page("book", "unit-1", 2),
            rendered_page("book", "unit-2", 3),
        ],
    )
    .expect("repaginated Word or Excel pages remain viewable");

    assert!(page_reference_hints("book", &pages, 1).is_empty());
    let subtitle = page_subtitle(&pages[1], VisualPageSourceKind::OfficeEnhanced);
    assert!(subtitle.contains("仅供预览"));
    assert!(subtitle.contains("不用于搜索或 AI 引用"));

    let guessed_source = AiSourceLink {
        citation_id: "citation-preview-only".to_string(),
        book_id: "book".to_string(),
        unit_id: "unit-1".to_string(),
        unit_index: None,
        document_revision: ngy_book_studio::document::Revision::new(1),
        unit_revision: ngy_book_studio::document::Revision::new(1),
        locator: Some(pages[0].locator.clone()),
        label: "增强预览第一页".to_string(),
        quote: None,
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    assert_eq!(
        page_index_for_source(
            &pages,
            VisualPageSourceKind::OfficeEnhanced,
            1,
            &guessed_source
        ),
        None,
        "a page without an exact unit mapping cannot be an AI citation target"
    );
}

#[test]
fn djvu_page_text_is_escaped_and_zoom_is_clamped() {
    let html = page_text_html("First line & <tag>\n\nSecond line", true, None);
    assert_eq!(
        html,
        "<p>First line &amp; &lt;tag&gt;</p><p>Second line</p>"
    );
    assert!(page_text_html("", false, None).contains("没有文字层"));

    // 朗读中的段落包一个 <mark>，其它段落不受影响；面板的段落索引与朗读块 id
    // 是同一份切分。
    let highlighted = page_text_html("First line & <tag>\n\nSecond line", true, Some(1));
    assert_eq!(
        highlighted,
        "<p>First line &amp; &lt;tag&gt;</p><p><mark>Second line</mark></p>"
    );
    assert_eq!(page_text_paragraphs("First line\n\nSecond line").len(), 2);

    assert_eq!(zoomed_size(800, 1_200, 500), (400.0, 600.0));
    assert_eq!(zoomed_size(1, 1, 250), (1.0, 1.0));
}

#[test]
fn a_long_djvu_line_splits_into_speakable_paragraphs() {
    // 文字层把一整页塞成一行时，段落按句末标点切开：每段都短于上限，且拼接
    // 回来的文字与原文逐字相同（切分只去掉了分段处的空白）。
    let page = "第一句。第二句！第三句？".repeat(120);
    let paragraphs = page_text_paragraphs(&page);
    assert!(paragraphs.len() > 1, "{}", paragraphs.len());
    assert!(
        paragraphs
            .iter()
            .all(|paragraph| paragraph.chars().count() <= MAX_PAGE_TEXT_PARAGRAPH_CHARS)
    );
    assert_eq!(paragraphs.concat(), page);

    // 没有句末标点的长行也能切：按上限硬切，不留空段。
    let punctuation_free = "字".repeat(MAX_PAGE_TEXT_PARAGRAPH_CHARS * 2 + 7);
    let paragraphs = page_text_paragraphs(&punctuation_free);
    assert_eq!(paragraphs.len(), 3);
    assert_eq!(paragraphs.concat(), punctuation_free);
}

#[test]
fn wheel_zoom_steps_within_range_and_returns_to_fit() {
    // Fit is the floor: a page can be enlarged out of it, never shrunk into
    // empty margin.
    assert_eq!(zoom_after_wheel(ZOOM_FIT, false), ZOOM_FIT);
    assert_eq!(zoom_after_wheel(ZOOM_FIT, true), 1_000);

    assert_eq!(zoom_after_wheel(1_000, true), 1_250);
    assert_eq!(zoom_after_wheel(1_000, false), 750);
    assert_eq!(zoom_after_wheel(ZOOM_MAX_MILLI, true), ZOOM_MAX_MILLI);
    // One notch below the smallest explicit size falls back to fit instead
    // of leaving a size the fit button could never reach again.
    assert_eq!(zoom_after_wheel(ZOOM_MIN_MILLI, false), ZOOM_FIT);
    assert_eq!(zoom_after_wheel(ZOOM_FIT + 1, false), ZOOM_FIT);
}
