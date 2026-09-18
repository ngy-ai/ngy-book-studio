//! Flattening markdown thoughts into a renderable part list.

use super::*;

pub(crate) fn push_markdown_part(parts: &mut Vec<ThoughtPart>, source: &str) {
    if !source.trim().is_empty() {
        parts.push(ThoughtPart::Markdown(source.to_owned().into()));
    }
}

pub(crate) fn table_rows(table: &::markdown::mdast::Table, source: &str) -> Vec<Vec<SharedString>> {
    table
        .children
        .iter()
        .map(|row| {
            row.children()
                .into_iter()
                .flatten()
                .map(|cell| {
                    // Source ranges belong to the already sanitized Markdown.
                    // Preserve inline emphasis/code without interpreting a row
                    // as a table again. Positions are supplied by this parser.
                    cell.position()
                        .and_then(|position| source.get(position.start.offset..position.end.offset))
                        .map(table_cell_markdown)
                        .unwrap_or_else(|| literal_cell_text(cell))
                        .into()
                })
                .collect()
        })
        .collect()
}

pub(crate) fn table_cell_markdown(source: &str) -> String {
    // mdast's TableCell range includes its leading separator and may include
    // the final row separator. Remove only those structural pipes; escaped
    // pipes and inline code inside the cell remain part of its Markdown.
    let source = source.trim();
    let source = source.strip_prefix('|').unwrap_or(source).trim_start();
    let source = if let Some(without_pipe) = source.strip_suffix('|') {
        let backslashes = without_pipe
            .chars()
            .rev()
            .take_while(|ch| *ch == '\\')
            .count();
        if backslashes % 2 == 0 {
            without_pipe
        } else {
            source
        }
    } else {
        source
    };
    source.trim_end().to_owned()
}

pub(crate) fn literal_cell_text(node: &::markdown::mdast::Node) -> String {
    use std::fmt::Write as _;

    let mut plain = String::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if let Some(children) = node.children() {
            pending.extend(children.iter().rev());
        } else {
            plain.push_str(&node.to_string());
        }
    }
    let mut escaped = String::with_capacity(plain.len());
    for ch in plain.chars() {
        if ch.is_ascii_punctuation() {
            write!(&mut escaped, "&#{};", ch as u32).expect("String writes cannot fail");
        } else {
            escaped.push(ch);
        }
    }
    escaped
}
