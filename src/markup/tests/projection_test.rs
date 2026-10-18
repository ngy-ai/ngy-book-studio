//! XHTML projection: strict-XML validity, entity rewriting and limits.

use super::*;

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
