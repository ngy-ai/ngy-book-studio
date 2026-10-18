//! `<head>` content must never leak into the body.

use super::*;

#[test]
fn head_title_text_never_becomes_a_body_block() {
    // Pandoc EPUB chapters keep the file name in `<head><title>`. Cleaning the
    // whole document used to strip that element but keep its text, so every
    // chapter opened with a stray "ch027.xhtml" paragraph above its heading.
    let source = "<!DOCTYPE html>\
        <html xmlns=\"http://www.w3.org/1999/xhtml\">\
        <head>\
        <meta charset=\"utf-8\" />\
        <title>ch027.xhtml</title>\
        <style>h1 { color: red; }</style>\
        <link rel=\"stylesheet\" type=\"text/css\" href=\"../styles/book.css\" />\
        </head>\
        <body epub:type=\"bodymatter\">\
        <section id=\"multimodal-rag\" class=\"level1\">\
        <h1>Multimodal RAG and Cross-Modal Retrieval</h1>\
        <p>Vision-native document RAG is one slice.</p>\
        </section>\
        </body>\
        </html>";
    let parsed = parse_source(source).unwrap();

    assert_eq!(
        parsed.document.blocks.len(),
        2,
        "<head> 文本不得进入正文：{:?}",
        parsed.document.blocks
    );
    assert!(matches!(
        parsed.document.blocks[0],
        Block::Heading { level: 1, .. }
    ));
    assert!(matches!(parsed.document.blocks[1], Block::Paragraph { .. }));
    // The leaked text would also defeat the exporter's title de-duplication,
    // which only recognizes a leading heading.
    assert!(!parsed.canonical_source.contains("ch027.xhtml"));
    assert!(!parsed.canonical_source.contains("color: red"));
}

#[test]
fn a_document_with_only_head_content_has_no_body_blocks() {
    let parsed = parse_source(
        "<html><head><title>ch027.xhtml</title><style>p { color: red; }</style></head>\
         <body></body></html>",
    )
    .unwrap();

    assert!(
        parsed.document.blocks.is_empty(),
        "<head> 不是正文：{:?}",
        parsed.document.blocks
    );
    assert!(parsed.canonical_source.trim().is_empty());
}
