//! Reading and rewriting the XHTML document the editor holds.

use super::*;

pub(crate) fn find_xml_sequence(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

pub(crate) fn xml_tag_end(bytes: &[u8], start: usize, track_brackets: bool) -> Option<usize> {
    let mut quote = None;
    let mut bracket_depth = 0usize;
    for (offset, byte) in bytes.get(start..)?.iter().copied().enumerate() {
        let index = start + offset;
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'[' if track_brackets => bracket_depth += 1,
            b']' if track_brackets => bracket_depth = bracket_depth.saturating_sub(1),
            b'>' if bracket_depth == 0 => return Some(index + 1),
            _ => {}
        }
    }
    None
}

pub(crate) fn xml_body_element_range(xml: &str) -> Option<(usize, usize)> {
    let bytes = xml.as_bytes();
    let mut cursor = 0usize;
    let mut body_start = None;
    let mut body_depth = 0usize;

    while cursor < bytes.len() {
        let start = cursor + bytes.get(cursor..)?.iter().position(|byte| *byte == b'<')?;
        if bytes.get(start..)?.starts_with(b"<!--") {
            cursor = find_xml_sequence(bytes, start + 4, b"-->")? + 3;
            continue;
        }
        if bytes.get(start..)?.starts_with(b"<![CDATA[") {
            cursor = find_xml_sequence(bytes, start + 9, b"]]>")? + 3;
            continue;
        }
        if bytes.get(start..)?.starts_with(b"<?") {
            cursor = find_xml_sequence(bytes, start + 2, b"?>")? + 2;
            continue;
        }
        if bytes.get(start..)?.starts_with(b"<!") {
            cursor = xml_tag_end(bytes, start + 2, true)?;
            continue;
        }

        let mut name_cursor = start + 1;
        let closing = bytes.get(name_cursor) == Some(&b'/');
        if closing {
            name_cursor += 1;
        }
        while bytes.get(name_cursor).is_some_and(u8::is_ascii_whitespace) {
            name_cursor += 1;
        }
        let name_start = name_cursor;
        while bytes
            .get(name_cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>'))
        {
            name_cursor += 1;
        }
        if name_cursor == name_start {
            cursor = start + 1;
            continue;
        }
        let end = xml_tag_end(bytes, name_cursor, false)?;
        let name = &bytes[name_start..name_cursor];
        let local_name = name.rsplit(|byte| *byte == b':').next().unwrap_or(name);
        let is_body = local_name.eq_ignore_ascii_case(b"body");
        let mut before_end = end.saturating_sub(2);
        while before_end > start && bytes[before_end].is_ascii_whitespace() {
            before_end -= 1;
        }
        let self_closing = bytes.get(before_end) == Some(&b'/');

        if is_body {
            if closing {
                if body_depth == 0 {
                    return None;
                }
                body_depth -= 1;
                if body_depth == 0 {
                    return Some((body_start?, end));
                }
            } else if self_closing {
                if body_depth == 0 {
                    return Some((start, end));
                }
            } else {
                if body_depth == 0 {
                    body_start = Some(start);
                }
                body_depth += 1;
            }
        }
        cursor = end;
    }
    None
}

pub(crate) fn replace_body_element(document: &str, replacement: &str) -> Option<String> {
    let (replacement_start, replacement_end) = xml_body_element_range(replacement)?;
    let replacement_bytes = replacement.as_bytes();
    let trimmed_start = replacement_bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())?;
    let trimmed_end = replacement_bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())?
        + 1;
    if (replacement_start, replacement_end) != (trimmed_start, trimmed_end) {
        return None;
    }

    let (body_start, close_end) = xml_body_element_range(document)?;
    let mut html = String::with_capacity(document.len() + replacement.len());
    html.push_str(&document[..body_start]);
    html.push_str(&replacement[replacement_start..replacement_end]);
    html.push_str(&document[close_end..]);
    Some(html)
}

pub(crate) fn parse_rich_text_snapshot(document: &str, unit_id: &str) -> Result<ParsedSource> {
    let (body_start, body_end) =
        xml_body_element_range(document).context("富文本快照缺少正文元素")?;
    // The WebView snapshot is a complete trusted XHTML shell. Parsing that
    // whole shell through an HTML fragment sanitizer can preserve the removed
    // <title> text as visible body content, duplicating the chapter title on
    // every rich-text save. Only the serialized body is editable content.
    parse_source_for_unit(&document[body_start..body_end], unit_id)
}

pub(crate) fn preview_document_from_source(
    template: &str,
    title: &str,
    unit_id: &str,
    source: &str,
) -> Result<(String, String, ngy_book_studio::document::BlockDocument)> {
    let parsed = parse_source_for_unit(source, unit_id)?;
    let html = serialize_xhtml(&parsed.document)?;
    let fallback = editor_document_shell(title, &html);
    let replacement = if xml_body_element_range(&html).is_some() {
        html
    } else {
        format!("<body><article>{html}</article></body>")
    };
    let preview = replace_body_element(template, &replacement).unwrap_or(fallback);
    Ok((preview, parsed.canonical_source, parsed.document))
}

pub(crate) fn editor_document_shell(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><meta charset=\"utf-8\"/><title>{}</title></head><body><article>{body}</article></body></html>",
        escape_editor_xml(title)
    )
}

pub(crate) fn escape_editor_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn editor_caption_text(caption: &[Inline]) -> Option<String> {
    let text = caption.iter().map(Inline::plain_text).collect::<String>();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub(crate) fn editor_media_label(
    kind: MediaKind,
    title: Option<&str>,
    description: Option<&str>,
) -> String {
    title
        .filter(|title| !title.trim().is_empty())
        .or_else(|| description.filter(|description| !description.trim().is_empty()))
        .unwrap_or(match kind {
            MediaKind::Image => "图片",
            MediaKind::Audio => "音频",
            MediaKind::Video => "视频",
        })
        .to_string()
}

pub(crate) fn media_blocks(blocks: &[Block]) -> Vec<EditorMediaBlock> {
    pub(crate) fn collect_inlines(inlines: &[Inline], output: &mut Vec<EditorMediaBlock>) {
        for inline in inlines {
            match inline {
                Inline::Image {
                    asset_id,
                    alt,
                    title,
                } => output.push(EditorMediaBlock {
                    id: format!("inline-image:{asset_id}"),
                    kind: MediaKind::Image,
                    label: editor_media_label(MediaKind::Image, title.as_deref(), Some(alt)),
                    title: title.clone(),
                    description: (!alt.trim().is_empty()).then(|| alt.trim().to_string()),
                }),
                Inline::Emphasis { content }
                | Inline::Strong { content }
                | Inline::Strikethrough { content }
                | Inline::Link { content, .. } => collect_inlines(content, output),
                _ => {}
            }
        }
    }

    pub(crate) fn collect(blocks: &[Block], output: &mut Vec<EditorMediaBlock>) {
        for block in blocks {
            match block {
                Block::Image {
                    id,
                    alt,
                    title,
                    caption,
                    ..
                } => {
                    let description = editor_caption_text(caption)
                        .or_else(|| (!alt.trim().is_empty()).then(|| alt.trim().to_string()));
                    output.push(EditorMediaBlock {
                        id: id.clone(),
                        kind: MediaKind::Image,
                        label: editor_media_label(
                            MediaKind::Image,
                            title.as_deref(),
                            description.as_deref(),
                        ),
                        title: title.clone(),
                        description,
                    });
                }
                Block::Audio {
                    id, title, caption, ..
                } => {
                    let description = editor_caption_text(caption);
                    output.push(EditorMediaBlock {
                        id: id.clone(),
                        kind: MediaKind::Audio,
                        label: editor_media_label(
                            MediaKind::Audio,
                            title.as_deref(),
                            description.as_deref(),
                        ),
                        title: title.clone(),
                        description,
                    });
                }
                Block::Video {
                    id, title, caption, ..
                } => {
                    let description = editor_caption_text(caption);
                    output.push(EditorMediaBlock {
                        id: id.clone(),
                        kind: MediaKind::Video,
                        label: editor_media_label(
                            MediaKind::Video,
                            title.as_deref(),
                            description.as_deref(),
                        ),
                        title: title.clone(),
                        description,
                    });
                }
                Block::BlockQuote { blocks, .. } => collect(blocks, output),
                Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                    for item in items {
                        collect(&item.blocks, output);
                    }
                }
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    collect_inlines(content, output)
                }
                Block::Table { header, rows, .. } => {
                    for row in header.iter().chain(rows) {
                        for cell in &row.cells {
                            collect_inlines(&cell.content, output);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut output = Vec::new();
    collect(blocks, &mut output);
    output
}

pub(crate) fn editor_caption_from_description(description: Option<&str>) -> Vec<Inline> {
    description
        .map(|description| vec![Inline::text(description)])
        .unwrap_or_default()
}
