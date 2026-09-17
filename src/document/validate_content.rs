//! Structural checks over blocks, inlines, tables and raw HTML.

use super::*;

pub(crate) fn validate_blocks(
    blocks: &[Block],
    path: &str,
    depth: usize,
    block_ids: &mut HashSet<String>,
    validator: &mut Validator,
) {
    if depth > MAX_DOCUMENT_DEPTH {
        validator.issue(path, format!("block nesting exceeds {MAX_DOCUMENT_DEPTH}"));
        return;
    }
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{index}]");
        let id_path = format!("{block_path}.id");
        validate_id(validator, &id_path, block.id());
        if !block.id().is_empty() && !block_ids.insert(block.id().to_string()) {
            validator.issue(id_path, "duplicate block ID");
        }
        match block {
            Block::Paragraph { content, .. } => validate_inlines(
                content,
                &format!("{block_path}.content"),
                depth + 1,
                validator,
            ),
            Block::Heading { level, content, .. } => {
                if !(1..=6).contains(level) {
                    validator.issue(format!("{block_path}.level"), "heading level must be 1..=6");
                }
                validate_inlines(
                    content,
                    &format!("{block_path}.content"),
                    depth + 1,
                    validator,
                );
            }
            Block::BlockQuote { blocks, .. } => {
                if blocks.is_empty() {
                    validator.issue(
                        format!("{block_path}.blocks"),
                        "block quote must not be empty",
                    );
                }
                validate_blocks(
                    blocks,
                    &format!("{block_path}.blocks"),
                    depth + 1,
                    block_ids,
                    validator,
                );
            }
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                if items.is_empty() {
                    validator.issue(format!("{block_path}.items"), "list must not be empty");
                }
                if let Block::OrderedList { start, .. } = block
                    && *start == 0
                {
                    validator.issue(
                        format!("{block_path}.start"),
                        "ordered-list start must be at least 1",
                    );
                }
                for (item_index, item) in items.iter().enumerate() {
                    let item_path = format!("{block_path}.items[{item_index}].blocks");
                    if item.blocks.is_empty() {
                        validator.issue(&item_path, "list item must not be empty");
                    }
                    validate_blocks(&item.blocks, &item_path, depth + 1, block_ids, validator);
                }
            }
            Block::CodeBlock { language, .. } => {
                if language.as_deref().is_some_and(|language| {
                    language.contains(['\r', '\n']) || language.chars().any(char::is_control)
                }) {
                    validator.issue(
                        format!("{block_path}.language"),
                        "code language must be a single printable line",
                    );
                }
            }
            Block::Table { header, rows, .. } => {
                validate_table(header.as_ref(), rows, &block_path, depth, validator);
            }
            Block::Image {
                asset_id, caption, ..
            }
            | Block::Audio {
                asset_id, caption, ..
            } => {
                validate_id(validator, &format!("{block_path}.asset_id"), asset_id);
                validate_inlines(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    validator,
                );
            }
            Block::Video {
                asset_id,
                poster_asset_id,
                caption,
                ..
            } => {
                validate_id(validator, &format!("{block_path}.asset_id"), asset_id);
                if let Some(poster_asset_id) = poster_asset_id.as_deref() {
                    validate_id(
                        validator,
                        &format!("{block_path}.poster_asset_id"),
                        poster_asset_id,
                    );
                }
                validate_inlines(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    validator,
                );
            }
            Block::RawHtml {
                source, plain_text, ..
            } => validate_raw_html(source, plain_text, &block_path, validator),
            Block::ThematicBreak { .. } => {}
        }
    }
}

pub(crate) fn validate_inlines(
    inlines: &[Inline],
    path: &str,
    depth: usize,
    validator: &mut Validator,
) {
    if inlines.is_empty() {
        return;
    }
    if depth > MAX_DOCUMENT_DEPTH {
        validator.issue(path, format!("inline nesting exceeds {MAX_DOCUMENT_DEPTH}"));
        return;
    }
    for (index, inline) in inlines.iter().enumerate() {
        let inline_path = format!("{path}[{index}]");
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content } => validate_inlines(
                content,
                &format!("{inline_path}.content"),
                depth + 1,
                validator,
            ),
            Inline::Link { href, content, .. } => {
                if href.trim().is_empty() || href.chars().any(char::is_control) {
                    validator.issue(format!("{inline_path}.href"), "invalid link target");
                }
                validate_inlines(
                    content,
                    &format!("{inline_path}.content"),
                    depth + 1,
                    validator,
                );
            }
            Inline::Image { asset_id, .. } => {
                validate_id(validator, &format!("{inline_path}.asset_id"), asset_id)
            }
            Inline::RawHtml { source, plain_text } => {
                validate_raw_html(source, plain_text, &inline_path, validator)
            }
            Inline::Text { .. } | Inline::Code { .. } | Inline::HardBreak | Inline::SoftBreak => {}
        }
    }
}

pub(crate) fn validate_raw_html(
    source: &str,
    plain_text: &str,
    path: &str,
    validator: &mut Validator,
) {
    if source.is_empty() {
        validator.issue(format!("{path}.source"), "raw HTML must not be empty");
        return;
    }
    if source.contains('\0') {
        validator.issue(format!("{path}.source"), "raw HTML must not contain NUL");
    }
    match raw_html_plain_text(source) {
        None => validator.issue(
            format!("{path}.source"),
            format!("raw HTML nesting exceeds {MAX_DOCUMENT_DEPTH}"),
        ),
        Some(expected) if plain_text != expected => validator.issue(
            format!("{path}.plain_text"),
            "raw HTML plain text must match its inert source projection",
        ),
        Some(_) => {}
    }
}

/// Computes the one canonical inert projection used by RawHtml producers and
/// validators. Traversal is iterative and detaches children as it goes so even
/// a rejected, extremely deep DOM is not recursively dropped.
pub(crate) fn raw_html_plain_text(source: &str) -> Option<String> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    let mut pending = vec![(dom.document.clone(), 0_usize, false)];
    let mut segments = Vec::new();
    let mut too_deep = false;

    while let Some((node, depth, suppressed)) = pending.pop() {
        let mut child_depth = depth;
        let mut child_suppressed = suppressed;
        match &node.data {
            NodeData::Text { contents } if !suppressed => {
                segments.push(contents.borrow().to_string());
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                let transport_wrapper = matches!(tag, "html" | "head" | "body");
                if !transport_wrapper {
                    if depth > MAX_DOCUMENT_DEPTH {
                        too_deep = true;
                    }
                    child_depth = depth + 1;
                }
                child_suppressed = suppressed || matches!(tag, "script" | "style");
                if !child_suppressed
                    && tag == "img"
                    && let Some(alt) = attrs
                        .borrow()
                        .iter()
                        .find(|attribute| attribute.name.local.as_ref() == "alt")
                        .map(|attribute| attribute.value.to_string())
                        .filter(|alt| !alt.trim().is_empty())
                {
                    segments.push(alt);
                }
            }
            _ => {}
        }

        let children = mem::take(&mut *node.children.borrow_mut());
        pending.extend(
            children
                .into_iter()
                .rev()
                .map(|child| (child, child_depth, child_suppressed)),
        );
    }

    if too_deep {
        return None;
    }
    let mut output = String::new();
    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        let mut previous_space = false;
        for character in segment.chars() {
            if character.is_whitespace() {
                if !previous_space {
                    output.push(' ');
                    previous_space = true;
                }
            } else {
                output.push(character);
                previous_space = false;
            }
        }
    }
    Some(output)
}

pub(crate) fn validate_table(
    header: Option<&TableRow>,
    rows: &[TableRow],
    path: &str,
    depth: usize,
    validator: &mut Validator,
) {
    let expected_width = header
        .map(|header| header.cells.len())
        .or_else(|| rows.first().map(|row| row.cells.len()))
        .unwrap_or_default();
    if expected_width == 0 {
        validator.issue(
            format!("{path}.rows"),
            "table must contain at least one cell",
        );
        return;
    }
    for (row_index, row) in header.into_iter().chain(rows.iter()).enumerate() {
        let row_path = format!("{path}.rows[{row_index}]");
        if row.cells.len() != expected_width {
            validator.issue(
                &row_path,
                format!("table row must contain exactly {expected_width} cells"),
            );
        }
        for (cell_index, cell) in row.cells.iter().enumerate() {
            validate_inlines(
                &cell.content,
                &format!("{row_path}.cells[{cell_index}].content"),
                depth + 1,
                validator,
            );
        }
    }
}
