//! Writing the block model back out as HTML.

use super::*;

pub(crate) fn write_html_blocks(blocks: &[Block], output: &mut String) {
    for block in blocks {
        match block {
            Block::Paragraph { content, .. } => {
                output.push_str("<p>");
                write_html_inlines(content, output);
                output.push_str("</p>");
            }
            Block::Heading { level, content, .. } => {
                let level = (*level).clamp(1, 6);
                output.push_str(&format!("<h{level}>"));
                write_html_inlines(content, output);
                output.push_str(&format!("</h{level}>"));
            }
            Block::BlockQuote { blocks, .. } => {
                output.push_str("<blockquote>");
                write_html_blocks(blocks, output);
                output.push_str("</blockquote>");
            }
            Block::BulletList { items, .. } => write_html_list("ul", items, output),
            Block::OrderedList { start, items, .. } => {
                output.push_str(&format!("<ol start=\"{start}\">"));
                write_html_list_items(items, output);
                output.push_str("</ol>");
            }
            Block::CodeBlock { language, code, .. } => {
                output.push_str("<pre><code");
                if let Some(language) = language {
                    output.push_str(" class=\"language-");
                    output.push_str(&escape_html(language));
                    output.push('"');
                }
                output.push('>');
                output.push_str(&escape_html(code));
                output.push_str("</code></pre>");
            }
            Block::ThematicBreak { .. } => output.push_str("<hr>"),
            Block::Table { header, rows, .. } => {
                output.push_str("<table>");
                if let Some(header) = header {
                    output.push_str("<thead>");
                    write_html_row(header, "th", output);
                    output.push_str("</thead>");
                }
                output.push_str("<tbody>");
                for row in rows {
                    write_html_row(row, "td", output);
                }
                output.push_str("</tbody></table>");
            }
            Block::Image {
                asset_id,
                alt,
                title,
                caption,
                ..
            } => write_html_image(asset_id, alt, title, caption, output),
            Block::Audio {
                asset_id,
                title,
                caption,
                ..
            } => write_html_media("audio", asset_id, None, title, caption, output),
            Block::Video {
                asset_id,
                poster_asset_id,
                title,
                caption,
                ..
            } => write_html_media(
                "video",
                asset_id,
                poster_asset_id.as_deref(),
                title,
                caption,
                output,
            ),
            Block::RawHtml { source, .. } => output.push_str(&sanitize_html(source)),
        }
    }
}

pub(crate) fn write_html_list(tag: &str, items: &[ListItem], output: &mut String) {
    output.push_str(&format!("<{tag}>"));
    write_html_list_items(items, output);
    output.push_str(&format!("</{tag}>"));
}

pub(crate) fn write_html_list_items(items: &[ListItem], output: &mut String) {
    for item in items {
        output.push_str("<li>");
        if let Some(checked) = item.checked {
            output.push_str(if checked {
                "<input type=\"checkbox\" checked disabled>"
            } else {
                "<input type=\"checkbox\" disabled>"
            });
        }
        write_html_blocks(&item.blocks, output);
        output.push_str("</li>");
    }
}

pub(crate) fn write_html_row(row: &TableRow, cell_tag: &str, output: &mut String) {
    output.push_str("<tr>");
    for cell in &row.cells {
        output.push_str(&format!("<{cell_tag}>"));
        write_html_inlines(&cell.content, output);
        output.push_str(&format!("</{cell_tag}>"));
    }
    output.push_str("</tr>");
}

pub(crate) fn write_html_caption(caption: &[Inline], output: &mut String) {
    if !caption.is_empty() {
        output.push_str("<figcaption>");
        write_html_inlines(caption, output);
        output.push_str("</figcaption>");
    }
}

pub(crate) fn write_html_image(
    asset_id: &str,
    alt: &str,
    title: &Option<String>,
    caption: &[Inline],
    output: &mut String,
) {
    output.push_str("<figure><img src=\"ngy-asset:");
    output.push_str(&escape_html(asset_id));
    output.push_str("\" alt=\"");
    output.push_str(&escape_html(alt));
    output.push('"');
    if let Some(title) = title {
        output.push_str(" title=\"");
        output.push_str(&escape_html(title));
        output.push('"');
    }
    output.push('>');
    write_html_caption(caption, output);
    output.push_str("</figure>");
}

pub(crate) fn write_html_media(
    tag: &str,
    asset_id: &str,
    poster_asset_id: Option<&str>,
    title: &Option<String>,
    caption: &[Inline],
    output: &mut String,
) {
    output.push_str("<figure><");
    output.push_str(tag);
    output.push_str(" controls src=\"ngy-asset:");
    output.push_str(&escape_html(asset_id));
    output.push('"');
    if let Some(poster) = poster_asset_id {
        output.push_str(" poster=\"ngy-asset:");
        output.push_str(&escape_html(poster));
        output.push('"');
    }
    if let Some(title) = title {
        output.push_str(" title=\"");
        output.push_str(&escape_html(title));
        output.push('"');
    }
    output.push_str("></");
    output.push_str(tag);
    output.push('>');
    write_html_caption(caption, output);
    output.push_str("</figure>");
}

pub(crate) fn write_html_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { value } => output.push_str(&escape_html(value)),
            Inline::Emphasis { content } => {
                output.push_str("<em>");
                write_html_inlines(content, output);
                output.push_str("</em>");
            }
            Inline::Strong { content } => {
                output.push_str("<strong>");
                write_html_inlines(content, output);
                output.push_str("</strong>");
            }
            Inline::Strikethrough { content } => {
                output.push_str("<s>");
                write_html_inlines(content, output);
                output.push_str("</s>");
            }
            Inline::Code { value } => {
                output.push_str("<code>");
                output.push_str(&escape_html(value));
                output.push_str("</code>");
            }
            Inline::Link {
                href,
                title,
                content,
            } => {
                output.push_str("<a href=\"");
                output.push_str(&escape_html(href));
                output.push('"');
                if let Some(title) = title {
                    output.push_str(" title=\"");
                    output.push_str(&escape_html(title));
                    output.push('"');
                }
                output.push('>');
                write_html_inlines(content, output);
                output.push_str("</a>");
            }
            Inline::HardBreak => output.push_str("<br>"),
            Inline::SoftBreak => output.push('\n'),
            Inline::Image {
                asset_id,
                alt,
                title,
            } => {
                output.push_str("<img src=\"ngy-asset:");
                output.push_str(&escape_html(asset_id));
                output.push_str("\" alt=\"");
                output.push_str(&escape_html(alt));
                output.push('"');
                if let Some(title) = title {
                    output.push_str(" title=\"");
                    output.push_str(&escape_html(title));
                    output.push('"');
                }
                output.push('>');
            }
            Inline::RawHtml { source, .. } => output.push_str(&sanitize_html(source)),
        }
    }
}

pub(crate) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
