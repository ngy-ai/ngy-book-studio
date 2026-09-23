//! Tests for parsing, cleaning, round-tripping and XHTML projection.

use super::*;

fn xhtml_body(fragment: &str) -> String {
    format!("<body xmlns=\"{XHTML_NAMESPACE}\">{fragment}</body>")
}

#[test]
fn block_texts_extract_innermost_blocks_and_ignore_hidden_text() {
    let source = r#"<div id="sbo-rt-content">
            <h1>Rust Brain Teasers</h1>
            <script>ignored()</script>
            <p>Copyright 2022
               second line</p>
            <ul><li><p>Nested item</p></li></ul>
            <table><tbody><tr><td>单元格</td></tr></tbody></table>
        </div>"#;
    assert_eq!(
        block_texts_from_html(source).unwrap(),
        vec![
            "Rust Brain Teasers",
            "Copyright 2022 second line",
            "Nested item",
            "单元格"
        ],
    );
    // An image-only chapter has nothing to translate.
    assert!(
        block_texts_from_html(r#"<div id="Cover"><img src="ngy-asset:cover"></div>"#)
            .unwrap()
            .is_empty()
    );
    assert!(block_texts_from_html("a\0b").is_err());
}

#[test]
fn translation_sources_keep_formatting_slots_breaks_and_code_context() {
    let source = "<h2>Chapter <em>one</em></h2>\
            <p>Read <strong>very <em>carefully</em></strong>: \
            <code>call(&quot;x&quot;)</code><br>Next <sup>2</sup>\u{a0}line.</p>\
            <p><code>untranslated()</code></p>\
            <pre><code>never translate this</code></pre>";
    let blocks = translation_blocks_from_html(source).unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].text, "Chapter one");
    assert_eq!(blocks[0].segments, ["Chapter ", "one"]);
    assert_eq!(
        blocks[1].text,
        "Read very carefully: call(\"x\")Next 2 line."
    );
    assert_eq!(
        blocks[1].segments,
        [
            "Read ",
            "very ",
            "carefully",
            ": ",
            "Next ",
            "2",
            "\u{a0}line."
        ]
    );
}

#[test]
fn translation_sources_exclude_inert_descendants_and_blank_leaves() {
    let source = "<p>Alpha<script>secret()</script><style>ignored{}</style>\
            <noscript>fallback</noscript><template><p>inert</p></template>\
            <strong>Beta</strong><span>\u{feff}\u{a0}</span>Gamma</p>\
            <p>\u{feff}\u{a0}</p>";
    let blocks = translation_blocks_from_html(source).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "AlphaBeta Gamma");
    assert_eq!(blocks[0].segments, ["Alpha", "Beta", "Gamma"]);
}

#[test]
fn translation_sources_skip_leaves_that_are_only_invisible_format_characters() {
    // 现场回归：代码行用 ZWSP 缩进，ZWSP 不属于 ECMAScript `\s`，旧过滤把它当成
    // 翻译槽；模型只能回空白，整块被 `empty_segment_text` 拒绝并停在 96 号块。
    // 该块本身是代码，现在整块不再进入翻译（见 code_blocks_are_never_translated）。
    let code_line = "<p><span>\u{200b}\u{200b}</span><span>const</span>\
            <span> THREE_AND_A_BIT : f32 = 3.4028236;</span></p>";
    assert!(translation_blocks_from_html(code_line).unwrap().is_empty());
    // 正文块保留同一保护：只由不可见格式字符组成的叶子不成为翻译槽，而不可见字符
    // 仍留在匹配文本里（读者侧靠这段文本定位块级元素），所以只能排除翻译槽。
    let prose = "<p><span>\u{200b}\u{200b}</span><span>甲</span><span> 乙</span></p>";
    let blocks = translation_blocks_from_html(prose).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "\u{200b}\u{200b}甲 乙");
    assert_eq!(blocks[0].segments, ["甲", " 乙"]);
}

#[test]
fn code_blocks_are_never_translated() {
    // Code that is not marked up with pre/code is still code: Calibre/Word
    // exports and the acceptance EPUB keep every code line in its own `<p>`.
    for source in [
        "<p>const THREE_AND_A_BIT : f32 = 3.4028236;</p>",
        "<p>fn main() {</p><p>}</p>",
        "<p>let a = 1<br>return a;</p>",
        "<p>// 注释行</p>",
        "<p>#include &lt;stdio.h&gt;</p>",
        "<p>count =&gt; count + 1</p>",
        "<p>total += 1</p>",
        "<p>&lt;div class=\"code\"&gt;</p>",
        // 现场（2026-09-15，《RUST AND SCALA FOR BEGINNERS》azw3，块 3579/3588/
        // 3620/3623）：一行代码被语法高亮的 span 切成 3–5 个片段，模型要在 JSON
        // 里转义 `"\n"` 这种字面量，两次尝试后整块被跳过。
        "<p>println(result1+\"\\n\"+result2+\"\\n\"+result3)</p>",
        "<p>println(&quot;10 + 10 = &quot;+result)</p>",
        // 同一本书的块 3580：类型注解加注释头。
        "<p>def functionExample(a:Int = 0, b:Int = 0):Int = { // Parameters with default values as 0</p>",
    ] {
        assert!(
            translation_blocks_from_html(source).unwrap().is_empty(),
            "{source} must not be translated"
        );
    }
    // Prose stays translatable, including the punctuation a code line carries,
    // a quotation, a colon without a following space, a statement keyword at the
    // start of a sentence, and inline code that merely appears inside a sentence.
    let prose = "<p>见上文（注 1）。</p><p>A note (see above)</p>\
            <p>Read <code>x = 1;</code> now</p><p>https://example.test/a/b</p>\
            <p>第一章：起步</p><p>a &lt; b 的关系</p>\
            <p>He said &quot;hello&quot; to me.</p><p>let me explain the difference</p>\
            <p>Note: the file is here</p>";
    let blocks = translation_blocks_from_html(prose).unwrap();
    assert_eq!(blocks.len(), 9);
    assert_eq!(blocks[2].segments, ["Read ", " now"]);
    assert_eq!(blocks[5].segments, ["a < b 的关系"]);
    assert_eq!(blocks[6].text, "He said \"hello\" to me.");
}

#[test]
fn translation_sources_preserve_repeated_blocks_and_nested_cell_order() {
    let source = "<ul><li><p>Same <b>text</b></p></li><li>Same <b>text</b></li></ul>\
            <blockquote><p>Quote</p></blockquote>\
            <table><tr><th>Title</th><td><p>Cell <i>value</i></p></td></tr></table>";
    let blocks = translation_blocks_from_html(source).unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["Same text", "Same text", "Quote", "Title", "Cell value"]
    );
    assert_eq!(blocks[0].segments, blocks[1].segments);
    assert_eq!(blocks[4].segments, ["Cell ", "value"]);
}

#[test]
fn translation_sources_reject_invalid_or_excessively_deep_source() {
    assert!(translation_blocks_from_html("<p>bad\0source</p>").is_err());
    let source = format!(
        "<p>{}text{}</p>",
        "<span>".repeat(MAX_DOCUMENT_DEPTH + 1),
        "</span>".repeat(MAX_DOCUMENT_DEPTH + 1)
    );
    assert!(translation_blocks_from_html(&source).is_err());
}

#[test]
fn reader_document_translation_slots_follow_original_body_without_ast_rewriting() {
    let source = r#"<!doctype html><html><head><title>Metadata</title>
            <script>ignore()</script></head><body>
            <p>Read <a href="https://example.test">this</a> now</p>
            <p><span>One</span><span>two</span></p>
            <ul><li>First <em>item</em></li></ul></body></html>"#;
    let blocks = translation_blocks_from_document_html(source).unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].segments, ["Read ", "this", " now"]);
    assert_eq!(blocks[1].text, "Onetwo");
    assert_eq!(blocks[1].segments, ["One", "two"]);
    assert_eq!(blocks[2].segments, ["First ", "item"]);
}

#[test]
fn xhtml_projection_preserves_controlled_media_and_task_attributes() {
    let document = BlockDocument::new(vec![
        Block::Image {
            id: "image".into(),
            asset_id: "image-asset".into(),
            alt: "图示 <A> & B".into(),
            title: Some("带\"引号\"".into()),
            caption: vec![Inline::text("图注")],
        },
        Block::Paragraph {
            id: "paragraph".into(),
            content: vec![
                Inline::text("第一行"),
                Inline::HardBreak,
                Inline::Image {
                    asset_id: "inline-image".into(),
                    alt: "行内图".into(),
                    title: None,
                },
            ],
        },
        Block::ThematicBreak { id: "rule".into() },
        Block::BulletList {
            id: "tasks".into(),
            items: vec![
                ListItem::task(true, vec![Block::paragraph("done", "已完成")]),
                ListItem::task(false, vec![Block::paragraph("todo", "待办")]),
            ],
        },
        Block::Audio {
            id: "audio".into(),
            asset_id: "audio-asset".into(),
            title: None,
            caption: Vec::new(),
        },
        Block::Video {
            id: "video".into(),
            asset_id: "video-asset".into(),
            poster_asset_id: Some("poster-asset".into()),
            title: None,
            caption: Vec::new(),
        },
    ]);
    let original_html = serialize_source(&document).unwrap();
    let fragment = serialize_xhtml(&document).unwrap();
    let body = xhtml_body(&fragment);
    let xml = resvg::usvg::roxmltree::Document::parse(&body).expect("strict XHTML");
    let images = xml
        .descendants()
        .filter(|node| node.has_tag_name((XHTML_NAMESPACE, "img")))
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].attribute("alt"), Some("图示 <A> & B"));
    assert_eq!(images[0].attribute("title"), Some("带\"引号\""));
    assert_eq!(images[1].attribute("src"), Some("ngy-asset:inline-image"));
    for tag in ["br", "hr"] {
        assert!(xml.descendants().any(|node| node.has_tag_name(tag)));
    }
    let tasks = xml
        .descendants()
        .filter(|node| node.has_tag_name("input"))
        .collect::<Vec<_>>();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].attribute("checked"), Some(""));
    assert_eq!(tasks[1].attribute("checked"), None);
    assert!(tasks.iter().all(|node| node.has_attribute("disabled")));
    for tag in ["audio", "video"] {
        let media = xml
            .descendants()
            .find(|node| node.has_tag_name(tag))
            .unwrap();
        assert_eq!(media.attribute("controls"), Some(""));
    }
    let video = xml
        .descendants()
        .find(|node| node.has_tag_name("video"))
        .unwrap();
    assert_eq!(video.attribute("poster"), Some("ngy-asset:poster-asset"));
    assert_eq!(serialize_source(&document).unwrap(), original_html);
}

#[test]
fn xml_safe_entities_rewrite_only_references_xml_cannot_resolve() {
    let safe = xml_safe_entities(
        "a&nbsp;b &amp; c &mdash; d &acE; e &notanentity; f &#160; g &#x20; h &amp;nbsp;i",
    );
    assert_eq!(
        safe.html,
        "a&#160;b &amp; c &#8212; d &#8766;&#819; e &notanentity; f &#160; g &#x20; h &amp;nbsp;i"
    );
    assert_eq!(safe.rewritten, ["nbsp", "mdash", "acE"]);
    assert_eq!(safe.rewritten_count, 3);
}

#[test]
fn xml_safe_entities_borrow_a_fragment_that_needs_no_rewrite() {
    for html in [
        "plain text without references",
        "already safe: &amp; &lt; &gt; &quot; &apos; &#160; &#xA0;",
    ] {
        let safe = xml_safe_entities(html);
        assert!(
            matches!(safe.html, Cow::Borrowed(_)),
            "no rewrite must stay borrowed: {html}"
        );
        assert_eq!(safe.rewritten_count, 0);
        assert!(safe.rewritten.is_empty());
    }
}

#[test]
fn xhtml_projection_preserves_cleaned_raw_html_entities_and_quoted_attributes() {
    let raw = r#"<div title="A &quot;B&quot; &lt;tag> &amp; C">A&nbsp;B &amp; C<picture><source src="ngy-asset:source"><img src="ngy-asset:raw-image" alt="raw"></picture><hr><script>unsafe()</script></div>"#;
    let inline = r#"<span title="x < y &amp; &quot;q&quot;">inline&nbsp;value<br></span>"#;
    let document = BlockDocument::new(vec![
        Block::RawHtml {
            id: "raw".into(),
            source: raw.into(),
            plain_text: raw_html_plain_text(raw).unwrap(),
        },
        Block::Paragraph {
            id: "inline".into(),
            content: vec![Inline::RawHtml {
                source: inline.into(),
                plain_text: raw_html_plain_text(inline).unwrap(),
            }],
        },
    ]);
    let body = xhtml_body(&serialize_xhtml(&document).unwrap());
    let xml = resvg::usvg::roxmltree::Document::parse(&body).expect("strict XHTML");
    let div = xml
        .descendants()
        .find(|node| node.has_tag_name("div"))
        .unwrap();
    assert_eq!(div.attribute("title"), Some("A \"B\" <tag> & C"));
    assert_eq!(div.text(), Some("A\u{a0}B & C"));
    let span = xml
        .descendants()
        .find(|node| node.has_tag_name("span"))
        .unwrap();
    assert_eq!(span.attribute("title"), Some("x < y & \"q\""));
    assert_eq!(span.text(), Some("inline\u{a0}value"));
    let source = xml
        .descendants()
        .find(|node| node.has_tag_name("source"))
        .unwrap();
    assert_eq!(source.attribute("src"), Some("ngy-asset:source"));
    assert!(xml.descendants().any(|node| node.has_tag_name("img")));
    assert!(xml.descendants().any(|node| node.has_tag_name("br")));
    assert!(xml.descendants().any(|node| node.has_tag_name("hr")));
    assert!(!xml.descendants().any(|node| node.has_tag_name("script")));
    assert!(!body.contains("unsafe()"));
}

#[test]
fn xhtml_projection_keeps_text_markup_literal() {
    let text = r#"显示 <img src="https://example.invalid/a"> & &#160; >"#;
    let document = BlockDocument::new(vec![Block::paragraph("text", text)]);
    let body = xhtml_body(&serialize_xhtml(&document).unwrap());
    let xml = resvg::usvg::roxmltree::Document::parse(&body).expect("strict XHTML");
    let paragraph = xml
        .descendants()
        .find(|node| node.has_tag_name("p"))
        .unwrap();
    assert_eq!(paragraph.text(), Some(text));
    assert!(!xml.descendants().any(|node| node.has_tag_name("img")));
}

#[test]
fn xhtml_projection_rejects_xml_forbidden_characters() {
    for character in ['\u{1}', '\u{b}', '\u{fffe}', '\u{ffff}'] {
        let document = BlockDocument::new(vec![Block::paragraph(
            "invalid-text",
            format!("前{character}后"),
        )]);
        let error = serialize_xhtml(&document).expect_err("XML must reject this character");
        assert!(
            error
                .to_string()
                .contains(&format!("U+{:04X}", character as u32))
        );
    }
}

#[test]
fn xhtml_projection_obeys_the_existing_html_depth_limit() {
    let source = format!(
        "{}正文{}",
        "<div>".repeat(MAX_DOCUMENT_DEPTH),
        "</div>".repeat(MAX_DOCUMENT_DEPTH)
    );
    let document = BlockDocument::new(vec![Block::RawHtml {
        id: "nested".into(),
        plain_text: raw_html_plain_text(&source).unwrap(),
        source,
    }]);
    let body = xhtml_body(&serialize_xhtml(&document).unwrap());
    resvg::usvg::roxmltree::Document::parse(&body).expect("bounded XHTML remains valid");

    let mut combined = document;
    for index in 0..3 {
        combined = BlockDocument::new(vec![Block::BlockQuote {
            id: format!("quote-{index}"),
            blocks: combined.blocks,
        }]);
    }
    assert!(serialize_xhtml(&combined).is_err());
}

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
        format!(
            "<p>{}正文{}</p>",
            "<em>".repeat(depth),
            "</em>".repeat(depth)
        )
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
