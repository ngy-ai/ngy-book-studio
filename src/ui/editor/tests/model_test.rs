//! Tests for previews, projections, media metadata, TOC depth and search.

use super::*;

#[test]
fn html_source_builds_a_sanitized_preview() {
    let (preview, source, document) = preview_document_from_source(
        DEFAULT_CHAPTER_HTML,
        "测试章",
        "unit-test",
        "<h1>标题</h1><p>正文 <strong>加粗</strong></p>",
    )
    .unwrap();

    assert!(preview.contains("<h1"));
    assert!(preview.contains("正文"));
    assert!(source.contains("<h1"));
    assert!(source.contains("<strong>加粗</strong>"));
    assert_eq!("标题\n\n正文 加粗", document.plain_text());
    let (preview, source, document) = preview_document_from_source(
        DEFAULT_CHAPTER_HTML,
        "测试章",
        "unit-test",
        "<p onclick=\"alert(1)\">正文</p><script>alert(1)</script>",
    )
    .unwrap();
    assert_eq!(document.plain_text(), "正文");
    for sanitized in [&preview, &source] {
        assert!(!sanitized.contains("<script"));
        assert!(!sanitized.contains("onclick"));
        assert!(!sanitized.contains("alert(1)"));
    }
}

#[test]
fn editor_projection_uses_the_canonical_ast_without_loading_original_resources() {
    let mut document = BookDocument::created("book-a", "统一模型");
    document.units.push(ContentUnit::new(
        "unit-a",
        ContentUnitKind::Chapter,
        "第一章",
        "<h1>过期源码</h1>",
        BlockDocument::new(vec![Block::Paragraph {
            id: "paragraph-a".to_string(),
            content: vec![Inline::Text {
                value: "来自规范化 AST".to_string(),
            }],
        }]),
    ));

    let chapters = editor_chapters_from_document(&document).unwrap();
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].href, "chapter-1.xhtml");
    assert_eq!(chapters[0].spine_index, Some(0));
    assert!(chapters[0].html.contains("来自规范化 AST"));
    assert!(!chapters[0].html.contains("过期源码"));
    assert!(!chapters[0].html.contains("stylesheet"));
}

#[test]
fn media_helpers_find_replace_and_delete_nested_blocks() {
    let mut blocks = vec![Block::BlockQuote {
        id: "quote".to_string(),
        blocks: vec![Block::Audio {
            id: "audio".to_string(),
            asset_id: "old".to_string(),
            title: Some("讲解".to_string()),
            caption: Vec::new(),
        }],
    }];

    assert_eq!(
        vec![EditorMediaBlock {
            id: "audio".to_string(),
            kind: MediaKind::Audio,
            label: "讲解".to_string(),
            title: Some("讲解".to_string()),
            description: None,
        }],
        media_blocks(&blocks)
    );
    assert!(replace_media_asset(&mut blocks, "audio", "new"));
    assert!(matches!(
        &blocks[0],
        Block::BlockQuote { blocks, .. }
            if matches!(&blocks[0], Block::Audio { asset_id, .. } if asset_id == "new")
    ));
    assert!(delete_media(&mut blocks, "audio"));
    assert!(media_blocks(&blocks).is_empty());
}

#[test]
fn media_metadata_updates_alt_caption_and_visible_search_text() {
    let mut blocks = vec![Block::Image {
        id: "diagram".to_string(),
        asset_id: "asset-image".to_string(),
        alt: "旧说明".to_string(),
        title: None,
        caption: Vec::new(),
    }];
    let metadata =
        EditorMediaMetadataDraft::from_inputs("  人物关系图  ", "  第一行说明\n第二行说明  ")
            .unwrap();

    assert!(update_media_metadata(&mut blocks, "diagram", &metadata));
    assert!(matches!(
        &blocks[0],
        Block::Image { alt, title, caption, .. }
            if alt == "第一行说明\n第二行说明"
                && title.as_deref() == Some("人物关系图")
                && editor_caption_text(caption).as_deref()
                    == Some("第一行说明\n第二行说明")
    ));
    let document = BlockDocument::new(blocks.clone());
    assert!(document.plain_text().contains("第一行说明"));
    assert!(document.plain_text().contains("人物关系图"));
    let media = media_blocks(&blocks);
    assert_eq!(media[0].title.as_deref(), Some("人物关系图"));
    assert_eq!(
        media[0].description.as_deref(),
        Some("第一行说明\n第二行说明")
    );
    assert!(EditorMediaMetadataDraft::from_inputs("ok\u{0007}", "").is_err());
    assert!(
        EditorMediaMetadataDraft::from_inputs(&"题".repeat(MAX_EDITOR_MEDIA_TITLE_CHARS + 1), "")
            .is_err()
    );
}

#[test]
fn existing_inline_image_metadata_can_be_edited_without_new_bytes() {
    let mut blocks = vec![Block::Paragraph {
        id: "paragraph".to_string(),
        content: vec![Inline::Image {
            asset_id: "asset-inline".to_string(),
            alt: "旧说明".to_string(),
            title: Some("旧标题".to_string()),
        }],
    }];
    let metadata = EditorMediaMetadataDraft::from_inputs("新标题", "新说明").unwrap();
    assert!(update_media_metadata(
        &mut blocks,
        "inline-image:asset-inline",
        &metadata,
    ));
    assert!(matches!(
        &blocks[0],
        Block::Paragraph { content, .. }
            if matches!(
                &content[0],
                Inline::Image { alt, title, .. }
                    if alt == "新说明" && title.as_deref() == Some("新标题")
            )
    ));
}

#[test]
fn toc_depth_and_unit_kind_cycle_are_deterministic() {
    let mut parent = TocNode::new(
        "parent",
        "上级",
        ngy_book_studio::document::TocTarget::unit("unit-a"),
    );
    parent.children.push(TocNode::new(
        "child",
        "下级",
        ngy_book_studio::document::TocTarget::unit("unit-b"),
    ));
    assert_eq!(0, toc_depth_for_unit(&[parent.clone()], "unit-a"));
    assert_eq!(1, toc_depth_for_unit(&[parent], "unit-b"));

    let mut kind = ContentUnitKind::Chapter;
    for expected in [
        ContentUnitKind::Section,
        ContentUnitKind::Page,
        ContentUnitKind::Slide,
        ContentUnitKind::Worksheet,
        ContentUnitKind::Chapter,
    ] {
        kind = next_unit_kind(kind);
        assert_eq!(expected, kind);
    }
}

#[test]
fn suggested_export_filename_is_epub_and_windows_safe() {
    assert_eq!(suggested_epub_filename("Rust/入门"), "Rust_入门.epub");
    assert_eq!(
        suggested_epub_filename("A<B>C:D\"E/F\\G|H?I*J"),
        "A_B_C_D_E_F_G_H_I_J.epub"
    );
    assert_eq!(suggested_epub_filename("  墨页.EPUB  "), "墨页.epub");
    assert_eq!(suggested_epub_filename(" . . "), "book.epub");
    assert_eq!(suggested_epub_filename("CON"), "CON_.epub");
    assert_eq!(suggested_epub_filename("LPT9.notes"), "LPT9_.notes.epub");
}

#[test]
fn editor_search_uses_visible_xhtml_text_and_unsaved_chapters() {
    let chapters = vec![EditorChapter {
        title: "起步".to_string(),
        href: "Text/start.xhtml".to_string(),
        spine_index: Some(0),
        html: r#"<html><head><title>HEAD_ONLY_TOKEN</title>
                <style>.hidden{content:'错误命中'}</style></head>
                <body><h1>全<strong>文</strong>检索 Rust &amp; EPUB</h1>
                <p>Rust 的 EPUB 是尚未保存的关键词。</p></body></html>"#
            .to_string(),
    }];

    let hits = search_editor_chapters(&chapters, "尚未保存");
    assert_eq!(1, hits.len());
    assert_eq!(0, hits[0].chapter_index);
    assert!(hits[0].snippet.contains("尚未保存"));
    assert!(search_editor_chapters(&chapters, "Rust & EPUB").len() == 1);
    assert_eq!(1, search_editor_chapters(&chapters, "Rust EPUB").len());
    assert_eq!(1, search_editor_chapters(&chapters, "全文检索").len());
    assert!(search_editor_chapters(&chapters, "错误命中").is_empty());
    assert!(search_editor_chapters(&chapters, "HEAD_ONLY_TOKEN").is_empty());
}
