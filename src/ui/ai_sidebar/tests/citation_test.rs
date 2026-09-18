//! Tests for citation navigation and DOM highlighting.

use super::*;

fn cited_document() -> (BookDocument, AiSourceLink) {
    let text = "前文 引用目标 后文";
    let start = text.find("引用目标").unwrap();
    let mut unit = ContentUnit::new(
        "unit-1",
        ContentUnitKind::Chapter,
        "Chapter",
        "<p>前文 引用目标 后文</p>",
        BlockDocument::new(vec![Block::paragraph("block-1", text)]),
    )
    .with_source_locator(SourceLocator::epub("Text/chapter.xhtml#source-anchor"));
    unit.revision = Revision::new(7);
    let mut document = BookDocument::created("book", "Book");
    document.revision = Revision::new(3);
    document.units.push(unit);
    let locator = DocumentLocator::text(
        "book",
        "unit-1",
        "block-1",
        start as u64,
        (start + "引用目标".len()) as u64,
    )
    .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor"));
    let source = AiSourceLink {
        citation_id: "passage:one".to_string(),
        book_id: "book".to_string(),
        unit_id: "unit-1".to_string(),
        unit_index: None,
        document_revision: Revision::new(3),
        unit_revision: Revision::new(7),
        locator: Some(locator),
        label: "Book · Chapter".to_string(),
        quote: Some("引用目标".to_string()),
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    (document, source)
}

#[test]
fn citation_navigation_rederives_the_exact_text_from_the_current_ast() {
    let (document, source) = cited_document();
    let target = source.current_navigation_target(&document).unwrap();
    assert_eq!(target.unit_index, 0);
    assert_eq!(
        target.source,
        Some(SourceLocator::epub("Text/chapter.xhtml#source-anchor"))
    );
    assert_eq!(target.focus.as_ref().unwrap().text, "引用目标");
    assert_eq!(target.focus.as_ref().unwrap().before, "前文 ");
    assert_eq!(target.focus.as_ref().unwrap().after, " 后文");
}

#[test]
fn citation_navigation_rejects_stale_versions_and_mismatched_sources() {
    let (document, mut source) = cited_document();
    source.document_revision = Revision::new(2);
    assert!(
        source
            .current_navigation_target(&document)
            .unwrap_err()
            .contains("版本已失效")
    );

    let (_, mut source) = cited_document();
    source.locator.as_mut().unwrap().source = Some(SourceLocator::epub("Text/other.xhtml"));
    assert!(
        source
            .current_navigation_target(&document)
            .unwrap_err()
            .contains("原始来源位置")
    );
}

#[test]
fn citation_navigation_rejects_a_repeated_quote_without_a_text_range() {
    let (mut document, mut source) = cited_document();
    document.units[0].document = BlockDocument::new(vec![Block::paragraph(
        "block-1",
        "重复文字，中间内容，重复文字",
    )]);
    source.locator = Some(
        DocumentLocator::block("book", "unit-1", "block-1")
            .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor")),
    );
    source.quote = Some("重复文字".to_string());

    assert!(
        source
            .current_navigation_target(&document)
            .unwrap_err()
            .contains("不唯一")
    );
}

#[test]
fn unit_quote_keeps_no_invented_context_so_dom_duplicates_are_ambiguous() {
    let (document, mut source) = cited_document();
    source.locator = Some(
        DocumentLocator::unit("book", "unit-1")
            .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor")),
    );
    source.quote = Some("引用目标".to_string());

    let target = source.current_navigation_target(&document).unwrap();
    let focus = target.focus.unwrap();
    assert_eq!(focus.text, "引用目标");
    assert!(focus.before.is_empty());
    assert!(focus.after.is_empty());
    let script = citation_dom_navigation_script(
        &focus,
        serde_json::json!({"type": "citation_navigation_result", "request_id": 2}),
    )
    .unwrap();
    assert!(script.contains("haystack.indexOf(context, matchStart + 1) >= 0"));
    assert!(script.contains("finish(false, matchStart < 0 ? 'not_found' : 'ambiguous')"));
}

#[test]
fn restored_selection_snapshot_requires_a_unique_current_ast_quote() {
    let (mut document, mut source) = cited_document();
    source.selection_snapshot = true;
    source.locator = Some(
        DocumentLocator::unit("book", "unit-1")
            .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor")),
    );

    source.quote = Some("仅存在于未保存编辑器中的正文".to_string());
    source.mark_restored_status(Some(&document));
    assert!(
        source.stale,
        "missing unsaved text cannot be restored as current"
    );

    source.quote = Some("引用目标".to_string());
    source.mark_restored_status(Some(&document));
    assert!(!source.stale, "a unique current AST match is provable");

    document.units[0].document = BlockDocument::new(vec![Block::paragraph(
        "block-1",
        "重复文字，中间内容，重复文字",
    )]);
    source.quote = Some("重复文字".to_string());
    source.mark_restored_status(Some(&document));
    assert!(source.stale, "a unit-only repeated quote is ambiguous");

    let (document, mut visual_passage) = cited_document();
    visual_passage.locator = Some(
        DocumentLocator::unit("book", "unit-1")
            .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor"))
            .with_region(NormalizedRect::new(10, 10, 200, 200)),
    );
    visual_passage.quote = Some("OCR text is not required in the reflowed AST".to_string());
    visual_passage.selection_snapshot = false;
    visual_passage.mark_restored_status(Some(&document));
    assert!(
        !visual_passage.stale,
        "visual passage provenance must bypass selection AST verification"
    );
}

#[test]
fn dom_navigation_preserves_virtual_block_spaces_and_rejects_duplicate_quotes() {
    let focus = AiCitationTextFocus {
        text: "重复文字".to_string(),
        before: String::new(),
        after: String::new(),
    };
    let script = citation_dom_navigation_script(
        &focus,
        serde_json::json!({"type": "citation_navigation_result", "request_id": 1}),
    )
    .unwrap();
    assert!(script.contains("let hasPendingSpace = false"));
    assert!(script.contains("emitSpace(null)"));
    assert!(script.contains("positions.push(pendingSpace)"));
    assert!(script.contains("haystack.indexOf(context, matchStart + 1) >= 0"));
    assert!(script.contains("'ambiguous'"));
}
