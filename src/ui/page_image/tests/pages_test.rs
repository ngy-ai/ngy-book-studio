//! Tests for persisted pages, locators and repagination.

use super::super::*;
use super::*;

#[test]
fn persisted_pages_accept_exact_and_rendered_office_locators() {
    let slide_pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![slide_page("book", "unit-1", 1)],
    )
    .expect("valid persisted slide page");
    assert_eq!(slide_pages.len(), 1);
    assert_eq!(slide_pages[0].locator.unit_id, "unit-1");
    assert_eq!(slide_pages[0].page_number, 1);
    assert!(!page_subtitle(&slide_pages[0], VisualPageSourceKind::OfficeEnhanced).contains("临时"));

    let rendered_pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![
            rendered_page("book", "unit-1", 1),
            rendered_page("book", "unit-1", 2),
        ],
    )
    .expect("one Word or Excel unit may span multiple pages");
    assert_eq!(rendered_pages.len(), 2);

    let section_pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![section_page("book", "unit-1", 1, 7)],
    )
    .expect("one-to-one Word page keeps its exact section locator");
    assert_eq!(section_pages[0].page_number, 1);
    assert_eq!(
        section_pages[0].locator.source,
        Some(SourceLocator::office_section(7))
    );

    let worksheet_pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![worksheet_page("book", "unit-1", 1, "汇总", "B2:G18")],
    )
    .expect("one-to-one Excel page keeps its exact worksheet locator");
    assert_eq!(worksheet_pages[0].page_number, 1);
    assert_eq!(
        worksheet_pages[0].locator.source,
        Some(SourceLocator::worksheet("汇总", Some("B2:G18".to_string())))
    );

    assert!(
        pages_from_persisted_pages(
            "other",
            VisualPageSourceKind::OfficeEnhanced,
            vec![slide_page("book", "unit-1", 1)],
        )
        .is_err()
    );

    let mut mismatched = slide_page("book", "unit-1", 1);
    mismatched.content_unit_id = Some("unit-2".to_string());
    assert!(
        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![mismatched],
        )
        .is_err()
    );

    let unsupported = page("book", "unit-1", 1, SourceLocator::pdf_page(1));
    assert!(
        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![unsupported],
        )
        .is_err()
    );
}

#[test]
fn djvu_pages_require_a_one_to_one_ordered_unit_mapping() {
    let pages = pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::Djvu,
        vec![
            djvu_page("book", "unit-1", 1),
            djvu_page("book", "unit-2", 2),
        ],
    )
    .expect("valid persisted DjVu pages");
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1].locator.source, Some(SourceLocator::djvu_page(2)));
    assert!(page_subtitle(&pages[0], VisualPageSourceKind::Djvu).contains("DjVu"));
    assert_eq!(
        page_reference_hints("book", &pages, 1)
            .iter()
            .map(|hint| hint.label.clone())
            .collect::<Vec<_>>(),
        vec!["当前页面 · 第 2 页".to_string(), "第 1 页".to_string()]
    );

    let mut out_of_order = djvu_page("book", "unit-2", 2);
    out_of_order.locator =
        DocumentLocator::unit("book", "unit-2").with_source(SourceLocator::djvu_page(3));
    assert!(
        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::Djvu,
            vec![djvu_page("book", "unit-1", 1), out_of_order],
        )
        .is_err()
    );

    let mut missing_unit = djvu_page("book", "unit-1", 1);
    missing_unit.content_unit_id = None;
    assert!(
        pages_from_persisted_pages("book", VisualPageSourceKind::Djvu, vec![missing_unit]).is_err()
    );

    let wrong_source = page("book", "unit-1", 1, SourceLocator::pdf_page(1));
    assert!(
        pages_from_persisted_pages("book", VisualPageSourceKind::Djvu, vec![wrong_source]).is_err()
    );
}

#[test]
fn persisted_page_locators_and_file_names_must_be_unique_and_ordered() {
    assert!(
        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![
                rendered_page("book", "unit-1", 1),
                rendered_page("book", "unit-1", 1),
            ],
        )
        .is_err()
    );

    let mut duplicate_file = slide_page("book", "unit-2", 2);
    duplicate_file.file_name = "Page00001.png".to_string();
    assert!(
        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![slide_page("book", "unit-1", 1), duplicate_file],
        )
        .is_err()
    );

    let mut first = slide_page("book", "unit-1", 1);
    first.file_name = "Page00002.png".to_string();
    let mut second = slide_page("book", "unit-2", 2);
    second.file_name = "Page00001.png".to_string();
    assert!(
        pages_from_persisted_pages(
            "book",
            VisualPageSourceKind::OfficeEnhanced,
            vec![first, second],
        )
        .is_err()
    );

    pages_from_persisted_pages(
        "book",
        VisualPageSourceKind::OfficeEnhanced,
        vec![
            section_page("book", "unit-1", 1, 1),
            rendered_page("book", "unit-2", 2),
        ],
    )
    .expect("the persisted order does not depend on a homogeneous source locator kind");
}
