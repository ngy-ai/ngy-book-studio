//! HTML parsing, cleaning and round-tripping.

use super::*;

#[test]
fn html_structured_blocks_round_trip_with_typed_semantics() {
    let source = r#"<h1>标题</h1><blockquote><p>说明 <em>内容</em></p></blockquote><ol start="3"><li>第一项 <strong>加粗</strong><ul><li><input type="checkbox" checked>已完成</li><li><input type="checkbox">待办</li></ul></li></ol><pre><code class="language-rust">a &lt; b
line 2</code></pre><table><thead><tr><th>名称</th><th>值</th></tr></thead><tbody><tr><td><p>A</p><p>第二行</p></td><td><del>旧</del></td></tr></tbody></table>"#;
    let parsed = parse_source_for_unit(source, "structured").unwrap();
    let reparsed = parse_source_for_unit(&parsed.canonical_source, "structured").unwrap();
    assert_eq!(parsed.document, reparsed.document);
    assert_eq!(parsed.canonical_source, reparsed.canonical_source);
    assert!(matches!(
        &parsed.document.blocks[1],
        Block::BlockQuote { .. }
    ));
    let Block::OrderedList { start, items, .. } = &parsed.document.blocks[2] else {
        panic!("expected an ordered list");
    };
    assert_eq!(*start, 3);
    let Block::BulletList { items, .. } = &items[0].blocks[1] else {
        panic!("expected a nested task list");
    };
    assert_eq!(items[0].checked, Some(true));
    assert_eq!(items[1].checked, Some(false));
    assert!(
        matches!(&parsed.document.blocks[3], Block::CodeBlock { language: Some(language), code, .. } if language == "rust" && code == "a < b\nline 2")
    );
    let Block::Table { header, rows, .. } = &parsed.document.blocks[4] else {
        panic!("expected a typed table");
    };
    assert_eq!(header.as_ref().unwrap().cells[0].plain_text(), "名称");
    assert_eq!(rows[0].cells[0].plain_text(), "A\n第二行");
}

#[test]
fn html_complex_tables_preserve_their_cleaned_structure() {
    for source in [
        "<table><tr><td colspan=\"2\">merged</td></tr></table>",
        "<table><caption>caption</caption><tr><td>body</td></tr></table>",
        "<table><tr><td><ul><li>nested</li></ul></td></tr></table>",
        "<table><tr><td>first</td><td>second</td></tr><tr><td>short row</td></tr></table>",
    ] {
        let first = parse_source_for_unit(source, "complex-table").unwrap();
        assert!(matches!(&first.document.blocks[0], Block::RawHtml { .. }));
        let second = parse_source_for_unit(&first.canonical_source, "complex-table").unwrap();
        assert_eq!(first.document, second.document);
    }
}

#[test]
fn html_preformatted_text_and_empty_containers_remain_saveable() {
    for (source, expected) in [
        (
            "<pre>before<code>x</code>after<code>y</code></pre>",
            "beforexaftery",
        ),
        (
            "<pre><code>\r\n&lt;a&gt;\r\nline 2</code></pre>",
            "\n<a>\nline 2",
        ),
    ] {
        let first = parse_source_for_unit(source, "pre").unwrap();
        assert!(
            matches!(&first.document.blocks[0], Block::CodeBlock { code, .. } if code == expected)
        );
        let second = parse_source_for_unit(&first.canonical_source, "pre").unwrap();
        assert_eq!(first.document, second.document);
    }
    for source in [
        "<pre><code>before<br>after</code></pre>",
        "<ul></ul>",
        "<ul><li></li></ul>",
        "<blockquote></blockquote>",
        "<table></table>",
        "<ol start=\"0\"><li>zero</li></ol>",
    ] {
        let first = parse_source_for_unit(source, "container").unwrap();
        let second = parse_source_for_unit(&first.canonical_source, "container").unwrap();
        assert_eq!(first.document, second.document, "{source}");
    }
}

#[test]
fn html_source_keeps_markdown_punctuation_as_literal_text() {
    let parsed = parse_source_for_unit("# title **literal**", "literal").unwrap();
    assert_eq!(parsed.canonical_source, "<p># title **literal**</p>");
    assert!(matches!(
        &parsed.document.blocks[0],
        Block::Paragraph { .. }
    ));
}

#[test]
fn html_loose_tasks_and_list_adjacent_text_survive_saving() {
    let parsed = parse_source_for_unit(
        "<ul><li><p><input type=\"checkbox\" checked>done</p></li></ul>",
        "tasks",
    )
    .unwrap();
    let Block::BulletList { items, .. } = &parsed.document.blocks[0] else {
        panic!("expected task list");
    };
    assert_eq!(items[0].checked, Some(true));
    let reparsed = parse_source_for_unit(&parsed.canonical_source, "tasks").unwrap();
    assert_eq!(parsed.document, reparsed.document);

    let parsed = parse_source_for_unit("<ul>prefix<li>x</li>suffix</ul>", "list").unwrap();
    assert!(matches!(&parsed.document.blocks[0], Block::RawHtml { .. }));
    assert!(parsed.document.plain_text().contains("prefix"));
    assert!(parsed.document.plain_text().contains("suffix"));
    let reparsed = parse_source_for_unit(&parsed.canonical_source, "list").unwrap();
    assert_eq!(parsed.document, reparsed.document);
}

#[test]
fn html_removes_scripts_handlers_and_network_resources() {
    let parsed = parse_source_for_unit(
            r#"<p onclick="steal()">正文</p><script>alert(1)</script><img src="https://evil.test/a.png"><audio controls src="asset:sound"></audio>"#,
            "unit-1",
        )
        .unwrap();
    assert!(!parsed.canonical_source.contains("script"));
    assert!(!parsed.canonical_source.contains("onclick"));
    assert!(!parsed.canonical_source.contains("https://"));
    assert!(parsed.canonical_source.contains("audio"));
    assert!(parsed.canonical_source.contains("asset:sound"));
}

#[test]
fn cleaned_html_is_semantically_idempotent() {
    let source = r#"
            <h2 onclick="steal()">Heading</h2>
            <p>Body <strong>bold</strong><script>alert(1)</script></p>
            <figure><img src="ngy-asset:image-1" alt="diagram"><figcaption>Caption</figcaption></figure>
            <video controls src="ngy-asset:video-1" poster="ngy-asset:poster-1"></video>
        "#;
    let first = parse_source_for_unit(source, "unit-html").unwrap();
    let second = parse_source_for_unit(&first.canonical_source, "unit-html").unwrap();

    assert_eq!(first.document, second.document);
    assert_eq!(first.canonical_source, second.canonical_source);
    assert!(!first.canonical_source.contains("onclick"));
    assert!(!first.canonical_source.contains("script"));
}

#[test]
fn html_nesting_is_bounded_before_recursive_ast_conversion() {
    fn nested_emphasis(depth: usize) -> String {
        format!("<p>{}正文{}", "<em>".repeat(depth), "</em>".repeat(depth))
    }

    parse_source_for_unit(
        &nested_emphasis(MAX_DOCUMENT_DEPTH - 1),
        "unit-at-depth-limit",
    )
    .expect("a visible HTML leaf at the shared depth limit remains accepted");

    let error = parse_source_for_unit(&nested_emphasis(4_096), "unit-over-depth-limit")
        .expect_err("extreme HTML nesting must return an error instead of recursing deeply");
    assert!(error.to_string().contains("HTML nesting exceeds"));
}

#[test]
fn unsafe_html_link_loses_its_navigation_but_keeps_text() {
    let parsed = parse_source_for_unit(
        r#"<p><a href="javascript:alert(1)">保留文字</a></p>"#,
        "unit-1",
    )
    .unwrap();
    assert_eq!(parsed.document.plain_text(), "保留文字");
    assert!(!parsed.canonical_source.contains("javascript"));
}

#[test]
fn html_links_keep_local_navigation_and_inert_network_labels() {
    let source = r##"<p>before<a href="https://example.test/">network</a>after <a href="#section">local</a> <a href="next.xhtml">next</a></p>"##;
    let first = parse_source_for_unit(source, "links").unwrap();
    assert!(!first.canonical_source.contains("https:"));
    assert!(first.canonical_source.contains("beforenetworkafter"));
    assert!(first.canonical_source.contains("href=\"#section\""));
    assert!(first.canonical_source.contains("href=\"next.xhtml\""));
    let second = parse_source_for_unit(&first.canonical_source, "links").unwrap();
    assert_eq!(first.document, second.document);
}

#[test]
fn preserved_html_image_keeps_its_inert_alt_projection() {
    let parsed = parse_source_for_unit(r#"<img src="images/figure.png" alt="替代文字">"#, "unit-1")
        .expect("a safe relative image remains representable as inert raw HTML");
    assert!(parsed.document.plain_text().contains("替代文字"));
}

#[test]
fn preserved_html_image_canonicalizes_alt_whitespace() {
    let parsed = parse_source_for_unit(
        "<img src=\"images/figure.png\" alt=\"替代   文字  \n第二行\">",
        "unit-1",
    )
    .expect("RawHtml image alt text must use the canonical inert projection");

    assert_eq!(parsed.document.plain_text(), "替代 文字 第二行");
    parsed
        .document
        .validate()
        .expect("the canonical RawHtml projection remains valid");
}

#[test]
fn media_blocks_serialize_with_opaque_asset_urls() {
    let document = BlockDocument::new(vec![Block::Video {
        id: "video-1".into(),
        asset_id: "video-asset".into(),
        poster_asset_id: Some("poster-asset".into()),
        title: Some("演示".into()),
        caption: vec![Inline::text("说明")],
    }]);
    let html = serialize_source(&document).unwrap();
    assert!(html.contains("ngy-asset:video-asset"));
    assert!(html.contains("ngy-asset:poster-asset"));
    assert!(!html.contains("blake3/"));
}

#[test]
fn block_media_metadata_survives_html_round_trips() {
    let document = BlockDocument::new(vec![
        Block::Image {
            id: "image".into(),
            asset_id: "image-asset".into(),
            alt: "图片说明".into(),
            title: Some("图片标题".into()),
            caption: vec![Inline::text("图片说明")],
        },
        Block::Audio {
            id: "audio".into(),
            asset_id: "audio-asset".into(),
            title: Some("音频标题".into()),
            caption: vec![Inline::text("音频说明")],
        },
        Block::Video {
            id: "video".into(),
            asset_id: "video-asset".into(),
            poster_asset_id: Some("poster-asset".into()),
            title: Some("视频标题".into()),
            caption: vec![Inline::text("视频说明")],
        },
    ]);
    let source = serialize_source(&document).unwrap();
    assert!(source.contains("<figcaption>图片说明</figcaption>"));
    assert!(source.contains("<figcaption>音频说明</figcaption>"));
    assert!(source.contains("<figcaption>视频说明</figcaption>"));

    let first = parse_source_for_unit(&source, "unit-media").unwrap();
    let second = parse_source_for_unit(&first.canonical_source, "unit-media").unwrap();
    assert_eq!(first.document, second.document);
    assert_eq!(
        first.document.referenced_asset_ids(),
        vec!["image-asset", "audio-asset", "video-asset", "poster-asset"]
    );
    let plain_text = first.document.plain_text();
    assert!(plain_text.contains("图片说明"));
    assert!(plain_text.contains("音频说明"));
    assert!(plain_text.contains("视频说明"));
}
