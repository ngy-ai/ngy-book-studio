//! The EPUB navigation document and the per-unit XHTML documents.

use super::*;

pub(crate) fn render_navigation(
    document: &BookDocument,
    unit_hrefs: &HashMap<&str, String>,
) -> Result<String> {
    let language = document.language.as_deref().unwrap_or("und");
    let mut output = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="{}" xml:lang="{}">
<head><meta charset="utf-8"/><title>{}</title></head>
<body><nav epub:type="toc" id="toc"><h1>目录</h1><ol>"#,
        escape_xml(language),
        escape_xml(language),
        escape_xml(&document.title),
    );
    if document.toc.is_empty() {
        for unit in &document.units {
            let href = unit_hrefs
                .get(unit.id.as_str())
                .context("EPUB 目录引用了不存在的内容单元")?;
            output.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_xml(href),
                escape_xml(&unit.title)
            ));
        }
    } else {
        render_toc_nodes(&document.toc, unit_hrefs, &mut output)?;
    }
    output.push_str("</ol></nav></body></html>\n");
    Ok(output)
}

pub(crate) fn render_toc_nodes(
    nodes: &[TocNode],
    unit_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for node in nodes {
        let unit_id = node.target.unit_id();
        let mut href = unit_hrefs
            .get(unit_id)
            .with_context(|| format!("EPUB 目录引用了不存在的内容单元 {unit_id}"))?
            .clone();
        if let TocTarget::Block { block_id, .. } = &node.target {
            href.push('#');
            href.push_str(&anchor_for_id(block_id));
        }
        output.push_str(&format!(
            "<li><a href=\"{}\">{}</a>",
            escape_xml(&href),
            escape_xml(&node.label)
        ));
        if !node.children.is_empty() {
            output.push_str("<ol>");
            render_toc_nodes(&node.children, unit_hrefs, output)?;
            output.push_str("</ol>");
        }
        output.push_str("</li>");
    }
    Ok(())
}

pub(crate) fn render_epub_unit(
    book: &BookDocument,
    unit: &crate::document::ContentUnit,
    asset_hrefs: &HashMap<&str, String>,
) -> Result<String> {
    let language = book.language.as_deref().unwrap_or("und");
    let mut body = String::new();
    if !unit_body_starts_with_title(unit) {
        body.push_str(&format!("<h1>{}</h1>", escape_xml(&unit.title)));
    }
    render_blocks(&unit.document.blocks, asset_hrefs, &mut body)?;
    // This document is served as `application/xhtml+xml`, so a named reference
    // the parser cannot resolve replaces the whole chapter with WebView2's
    // "Entity 'nbsp' not defined" page. `render_blocks` reaches raw HTML
    // through ammonia, whose serializer spells U+00A0 `&nbsp;`.
    let body = crate::markup::xml_safe_entities(&body);
    if body.rewritten_count > 0 {
        tracing::debug!(
            target: "ngy_reader",
            unit = %unit.id,
            rewritten = body.rewritten_count,
            entities = ?body.rewritten,
            "阅读器投影：把未定义的命名实体改写为数值引用"
        );
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" lang="{0}" xml:lang="{0}">
<head><meta charset="utf-8"/><title>{1}</title><link rel="stylesheet" type="text/css" href="../styles/book.css"/></head>
<body><article>{2}</article></body></html>
"#,
        escape_xml(language),
        escape_xml(&unit.title),
        body.html,
    ))
}

pub(crate) fn unit_body_starts_with_title(unit: &crate::document::ContentUnit) -> bool {
    let title = unit.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return false;
    }

    let Some(Block::Heading { content, .. }) = unit.document.blocks.first() else {
        return false;
    };
    content
        .iter()
        .map(Inline::plain_text)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        == title
}
