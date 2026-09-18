//! Chapter search over the visible XHTML text.

use super::*;

#[cfg(test)]
pub(crate) fn toc_depth_for_unit(nodes: &[TocNode], unit_id: &str) -> usize {
    pub(crate) fn find(nodes: &[TocNode], unit_id: &str, depth: usize) -> Option<usize> {
        nodes.iter().find_map(|node| {
            (node.target.unit_id() == unit_id)
                .then_some(depth)
                .or_else(|| find(&node.children, unit_id, depth + 1))
        })
    }
    find(nodes, unit_id, 0).unwrap_or(0)
}

pub(crate) fn suggested_epub_filename(title: &str) -> String {
    let mut stem = title
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();

    let trimmed_len = stem.trim_end_matches([' ', '.']).len();
    stem.truncate(trimmed_len);
    if stem.to_ascii_lowercase().ends_with(".epub") {
        stem.truncate(stem.len() - ".epub".len());
    }
    let trimmed_len = stem.trim_end_matches([' ', '.']).len();
    stem.truncate(trimmed_len);
    stem = stem.chars().take(120).collect();
    let trimmed_len = stem.trim_end_matches([' ', '.']).len();
    stem.truncate(trimmed_len);
    if stem.is_empty() {
        stem.push_str("book");
    }

    let device_component_len = stem.split('.').next().unwrap_or_default().len();
    let device_name = stem[..device_component_len].to_ascii_uppercase();
    let reserved = matches!(device_name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device_name
            .strip_prefix("COM")
            .or_else(|| device_name.strip_prefix("LPT"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        stem.insert(device_component_len, '_');
    }

    format!("{stem}.epub")
}

/// Converts editable XHTML into compact visible text for in-memory editor
/// searches. The persisted library index performs the same job while importing
/// and saving; this local path is needed so unsaved source/rich-text snapshots
/// are searchable before they reach SQLite.
pub(crate) fn searchable_text_from_xhtml(source: &str) -> String {
    pub(crate) fn push_text_char(output: &mut String, ch: char) {
        if ch.is_whitespace() {
            if !output.ends_with(' ') && !output.is_empty() {
                output.push(' ');
            }
        } else {
            output.push(ch);
        }
    }

    pub(crate) fn decode_entity(entity: &str) -> Option<char> {
        match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        }
    }

    pub(crate) fn is_text_boundary(name: &str) -> bool {
        matches!(
            name,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "body"
                | "br"
                | "dd"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "hr"
                | "li"
                | "main"
                | "nav"
                | "ol"
                | "p"
                | "pre"
                | "section"
                | "table"
                | "tbody"
                | "td"
                | "tfoot"
                | "th"
                | "thead"
                | "tr"
                | "ul"
        )
    }

    let mut output = String::with_capacity(source.len().min(64 * 1024));
    let mut chars = source.chars().peekable();
    let mut hidden_tags = Vec::<String>::new();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut tag = String::new();
            let mut quote = None;
            for next in chars.by_ref() {
                match next {
                    '\'' | '"' if quote.is_none() => quote = Some(next),
                    next if quote == Some(next) => quote = None,
                    '>' if quote.is_none() => break,
                    _ => {}
                }
                tag.push(next);
            }
            let tag = tag.trim();
            let closing = tag.starts_with('/');
            let self_closing = tag.ends_with('/');
            let name = tag
                .trim_start_matches('/')
                .split(|ch: char| ch.is_whitespace() || ch == '/')
                .next()
                .unwrap_or_default()
                .rsplit(':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if closing {
                if let Some(position) = hidden_tags.iter().rposition(|hidden| hidden == &name) {
                    hidden_tags.truncate(position);
                } else if hidden_tags.is_empty() && is_text_boundary(&name) {
                    push_text_char(&mut output, ' ');
                }
            } else if matches!(
                name.as_str(),
                "head" | "script" | "style" | "template" | "noscript"
            ) && !self_closing
            {
                hidden_tags.push(name);
            } else if hidden_tags.is_empty() && is_text_boundary(&name) {
                push_text_char(&mut output, ' ');
            }
            continue;
        }
        if !hidden_tags.is_empty() {
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            let mut terminated = false;
            while entity.len() <= 12 {
                let Some(next) = chars.peek().copied() else {
                    break;
                };
                if next == ';' {
                    chars.next();
                    terminated = true;
                    break;
                }
                if next.is_whitespace() || matches!(next, '<' | '&') {
                    break;
                }
                entity.push(next);
                chars.next();
            }
            if terminated {
                if let Some(decoded) = decode_entity(&entity) {
                    push_text_char(&mut output, decoded);
                } else {
                    output.push('&');
                    output.push_str(&entity);
                    output.push(';');
                }
            } else {
                output.push('&');
                output.push_str(&entity);
            }
            continue;
        }
        push_text_char(&mut output, ch);
    }
    output.trim().to_string()
}

pub(crate) fn search_snippet(text: &str, query: &str) -> Option<String> {
    let folded = text.to_lowercase();
    let query = query.to_lowercase();
    let byte_start = folded.find(&query)?;
    let match_start = folded[..byte_start].chars().count();
    let match_len = query.chars().count();
    let chars = text.chars().collect::<Vec<_>>();
    let snippet_start = match_start.saturating_sub(36);
    let snippet_end = (match_start + match_len + 56).min(chars.len());
    let mut snippet = chars[snippet_start..snippet_end].iter().collect::<String>();
    if snippet_start > 0 {
        snippet.insert(0, '…');
    }
    if snippet_end < chars.len() {
        snippet.push('…');
    }
    Some(snippet)
}

pub(crate) fn search_editor_chapters(
    chapters: &[EditorChapter],
    query: &str,
) -> Vec<EditorSearchHit> {
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Vec::new();
    }
    chapters
        .iter()
        .enumerate()
        .filter_map(|(chapter_index, chapter)| {
            let body = searchable_text_from_xhtml(&chapter.html);
            let matches_all = terms.iter().all(|term| {
                search_snippet(&body, term).is_some()
                    || search_snippet(&chapter.title, term).is_some()
            });
            if !matches_all {
                return None;
            }
            let snippet = terms
                .iter()
                .find_map(|term| search_snippet(&body, term))
                .or_else(|| {
                    terms.iter().find_map(|term| {
                        search_snippet(&chapter.title, term)
                            .map(|title| format!("章节标题：{title}"))
                    })
                })
                .unwrap_or_else(|| chapter.title.clone());
            Some(EditorSearchHit {
                chapter_index,
                chapter_title: chapter.title.clone(),
                snippet,
            })
        })
        .take(200)
        .collect()
}
