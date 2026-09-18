//! Rendering the document blocks and inlines as XHTML.

use super::*;

pub(crate) fn render_blocks(
    blocks: &[Block],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for block in blocks {
        let anchor = anchor_for_id(block.id());
        match block {
            Block::Paragraph { content, .. } => {
                output.push_str(&format!("<p id=\"{anchor}\">"));
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</p>");
            }
            Block::Heading { level, content, .. } => {
                output.push_str(&format!("<h{level} id=\"{anchor}\">"));
                render_inlines(content, asset_hrefs, output)?;
                output.push_str(&format!("</h{level}>"));
            }
            Block::BlockQuote { blocks, .. } => {
                output.push_str(&format!("<blockquote id=\"{anchor}\">"));
                render_blocks(blocks, asset_hrefs, output)?;
                output.push_str("</blockquote>");
            }
            Block::BulletList { items, .. } => {
                output.push_str(&format!("<ul id=\"{anchor}\">"));
                render_list_items(items, asset_hrefs, output)?;
                output.push_str("</ul>");
            }
            Block::OrderedList { start, items, .. } => {
                output.push_str(&format!("<ol id=\"{anchor}\" start=\"{start}\">"));
                render_list_items(items, asset_hrefs, output)?;
                output.push_str("</ol>");
            }
            Block::CodeBlock { language, code, .. } => {
                let language = language
                    .as_deref()
                    .map(|language| format!(" class=\"language-{}\"", escape_xml(language)))
                    .unwrap_or_default();
                output.push_str(&format!(
                    "<pre id=\"{anchor}\"><code{language}>{}</code></pre>",
                    escape_xml(code)
                ));
            }
            Block::ThematicBreak { .. } => {
                output.push_str(&format!("<hr id=\"{anchor}\"/>"));
            }
            Block::Table { header, rows, .. } => {
                output.push_str(&format!("<table id=\"{anchor}\">"));
                if let Some(header) = header {
                    output.push_str("<thead>");
                    render_table_row(header, true, asset_hrefs, output)?;
                    output.push_str("</thead>");
                }
                output.push_str("<tbody>");
                for row in rows {
                    render_table_row(row, false, asset_hrefs, output)?;
                }
                output.push_str("</tbody></table>");
            }
            Block::Image {
                asset_id,
                alt,
                title,
                caption,
                ..
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!(
                    "<figure id=\"{anchor}\"><img src=\"{}\" alt=\"{}\"",
                    escape_xml(href),
                    escape_xml(alt)
                ));
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str("/>");
                render_caption(caption, asset_hrefs, output)?;
                output.push_str("</figure>");
            }
            Block::Audio {
                asset_id,
                title,
                caption,
                ..
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!("<figure id=\"{anchor}\"><audio controls=\"controls\" preload=\"metadata\" src=\"{}\"", escape_xml(href)));
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str(">当前阅读器不支持音频播放。</audio>");
                render_caption(caption, asset_hrefs, output)?;
                output.push_str("</figure>");
            }
            Block::Video {
                asset_id,
                poster_asset_id,
                title,
                caption,
                ..
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!("<figure id=\"{anchor}\"><video controls=\"controls\" preload=\"metadata\" src=\"{}\"", escape_xml(href)));
                if let Some(poster) = poster_asset_id {
                    output.push_str(&format!(
                        " poster=\"{}\"",
                        escape_xml(required_asset_href(asset_hrefs, poster)?)
                    ));
                }
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str(">当前阅读器不支持视频播放。</video>");
                render_caption(caption, asset_hrefs, output)?;
                output.push_str("</figure>");
            }
            Block::RawHtml { source, .. } => {
                output.push_str(&format!("<section id=\"{anchor}\">"));
                output.push_str(&sanitize_raw_html(source));
                output.push_str("</section>");
            }
        }
    }
    Ok(())
}

pub(crate) fn render_list_items(
    items: &[ListItem],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for item in items {
        output.push_str("<li>");
        if let Some(checked) = item.checked {
            output.push_str(if checked { "[x] " } else { "[ ] " });
        }
        render_blocks(&item.blocks, asset_hrefs, output)?;
        output.push_str("</li>");
    }
    Ok(())
}

pub(crate) fn render_table_row(
    row: &TableRow,
    header: bool,
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    let tag = if header { "th" } else { "td" };
    output.push_str("<tr>");
    for cell in &row.cells {
        output.push_str(&format!("<{tag}>"));
        render_inlines(&cell.content, asset_hrefs, output)?;
        output.push_str(&format!("</{tag}>"));
    }
    output.push_str("</tr>");
    Ok(())
}

pub(crate) fn render_caption(
    caption: &[Inline],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    if !caption.is_empty() {
        output.push_str("<figcaption>");
        render_inlines(caption, asset_hrefs, output)?;
        output.push_str("</figcaption>");
    }
    Ok(())
}

pub(crate) fn render_inlines(
    inlines: &[Inline],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for inline in inlines {
        match inline {
            Inline::Text { value } => output.push_str(&escape_xml(value)),
            Inline::Emphasis { content } => {
                output.push_str("<em>");
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</em>");
            }
            Inline::Strong { content } => {
                output.push_str("<strong>");
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</strong>");
            }
            Inline::Strikethrough { content } => {
                output.push_str("<del>");
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</del>");
            }
            Inline::Code { value } => {
                output.push_str(&format!("<code>{}</code>", escape_xml(value)));
            }
            Inline::Link {
                href,
                title,
                content,
            } => {
                if safe_link_href(href) {
                    output.push_str(&format!("<a href=\"{}\"", escape_xml(href)));
                    if let Some(title) = title {
                        output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                    }
                    output.push('>');
                    render_inlines(content, asset_hrefs, output)?;
                    output.push_str("</a>");
                } else {
                    render_inlines(content, asset_hrefs, output)?;
                }
            }
            Inline::HardBreak => output.push_str("<br/>"),
            Inline::SoftBreak => output.push('\n'),
            Inline::Image {
                asset_id,
                alt,
                title,
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\"",
                    escape_xml(href),
                    escape_xml(alt)
                ));
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str("/>");
            }
            Inline::RawHtml { source, .. } => output.push_str(&sanitize_raw_html(source)),
        }
    }
    Ok(())
}
