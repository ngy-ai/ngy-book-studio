//! Depth-guarded text flattening and the stable hash used by tests.

use super::*;

pub(crate) fn inline_plain_text_at_depth(inlines: &[Inline], depth: usize) -> String {
    if depth > MAX_DOCUMENT_DEPTH {
        return String::new();
    }
    let mut output = String::new();
    for inline in inlines {
        output.push_str(&inline.plain_text_at_depth(depth));
    }
    output
}

pub(crate) fn join_block_text_at_depth(blocks: &[Block], separator: &str, depth: usize) -> String {
    if depth > MAX_DOCUMENT_DEPTH {
        return String::new();
    }
    blocks
        .iter()
        .map(|block| block.plain_text_at_depth(depth))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

pub(crate) fn join_visible_text<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut visible = Vec::<&str>::new();
    for part in parts {
        let part = part.trim();
        if !part.is_empty() && !visible.contains(&part) {
            visible.push(part);
        }
    }
    visible.join("\n")
}

pub(crate) fn push_non_blank(output: &mut Vec<String>, value: &str) {
    if !value.trim().is_empty() {
        output.push(value.to_string());
    }
}

pub(crate) fn collect_inline_asset_ids<'a>(
    inlines: &'a [Inline],
    output: &mut Vec<&'a str>,
    depth: usize,
) {
    if depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    for inline in inlines {
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => collect_inline_asset_ids(content, output, depth + 1),
            Inline::Image { asset_id, .. } => output.push(asset_id),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn stable_json_hash(value: &impl Serialize) -> String {
    match serde_json::to_vec(value) {
        Ok(bytes) => blake3::hash(&bytes).to_hex().to_string(),
        Err(error) => {
            // Public hash helpers cannot return an error without breaking their
            // established API. Invalid over-deep models are nevertheless
            // rejected by Serialize without recursing to the bottom; hash the
            // stable rejection instead of panicking or overflowing the stack.
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"invalid-document\0");
            hasher.update(error.to_string().as_bytes());
            hasher.finalize().to_hex().to_string()
        }
    }
}
