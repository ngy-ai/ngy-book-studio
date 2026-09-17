//! Reference checks: assets, the table of contents and text ranges.

use super::*;

pub(crate) fn validate_block_asset_uses(
    blocks: &[Block],
    path: &str,
    depth: usize,
    assets: &HashMap<&str, &AssetRef>,
    validator: &mut Validator,
) {
    // Structural validation already records the depth error. Stop this second
    // traversal at the same boundary so an invalid document cannot overflow
    // the stack before `BookDocument::validate` returns that error.
    if depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{index}]");
        match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                validate_inline_asset_uses(
                    content,
                    &format!("{block_path}.content"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::BlockQuote { blocks, .. } => validate_block_asset_uses(
                blocks,
                &format!("{block_path}.blocks"),
                depth + 1,
                assets,
                validator,
            ),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                for (item_index, item) in items.iter().enumerate() {
                    validate_block_asset_uses(
                        &item.blocks,
                        &format!("{block_path}.items[{item_index}].blocks"),
                        depth + 1,
                        assets,
                        validator,
                    );
                }
            }
            Block::Table { header, rows, .. } => {
                for (row_index, row) in header.iter().chain(rows.iter()).enumerate() {
                    for (cell_index, cell) in row.cells.iter().enumerate() {
                        validate_inline_asset_uses(
                            &cell.content,
                            &format!("{block_path}.rows[{row_index}].cells[{cell_index}].content"),
                            depth + 1,
                            assets,
                            validator,
                        );
                    }
                }
            }
            Block::Image {
                asset_id, caption, ..
            } => {
                validate_asset_use(
                    validator,
                    &format!("{block_path}.asset_id"),
                    asset_id,
                    assets,
                    &[AssetRole::ContentImage, AssetRole::Cover, AssetRole::Poster],
                );
                validate_inline_asset_uses(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::Audio {
                asset_id, caption, ..
            } => {
                validate_asset_use(
                    validator,
                    &format!("{block_path}.asset_id"),
                    asset_id,
                    assets,
                    &[AssetRole::Audio],
                );
                validate_inline_asset_uses(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::Video {
                asset_id,
                poster_asset_id,
                caption,
                ..
            } => {
                validate_asset_use(
                    validator,
                    &format!("{block_path}.asset_id"),
                    asset_id,
                    assets,
                    &[AssetRole::Video],
                );
                if let Some(poster_asset_id) = poster_asset_id.as_deref() {
                    validate_asset_use(
                        validator,
                        &format!("{block_path}.poster_asset_id"),
                        poster_asset_id,
                        assets,
                        &[AssetRole::Poster, AssetRole::ContentImage, AssetRole::Cover],
                    );
                }
                validate_inline_asset_uses(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::CodeBlock { .. } | Block::ThematicBreak { .. } | Block::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn validate_inline_asset_uses(
    inlines: &[Inline],
    path: &str,
    depth: usize,
    assets: &HashMap<&str, &AssetRef>,
    validator: &mut Validator,
) {
    if depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    for (index, inline) in inlines.iter().enumerate() {
        let inline_path = format!("{path}[{index}]");
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => validate_inline_asset_uses(
                content,
                &format!("{inline_path}.content"),
                depth + 1,
                assets,
                validator,
            ),
            Inline::Image { asset_id, .. } => validate_asset_use(
                validator,
                &format!("{inline_path}.asset_id"),
                asset_id,
                assets,
                &[AssetRole::ContentImage, AssetRole::Cover, AssetRole::Poster],
            ),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

pub(crate) fn validate_asset_use(
    validator: &mut Validator,
    path: &str,
    asset_id: &str,
    assets: &HashMap<&str, &AssetRef>,
    allowed_roles: &[AssetRole],
) {
    let Some(asset) = assets.get(asset_id).copied() else {
        if !asset_id.is_empty() {
            validator.issue(path, "referenced asset does not exist");
        }
        return;
    };
    if !allowed_roles.iter().any(|role| asset.has_role(*role)) {
        validator.issue(path, "referenced asset has an incompatible role");
    }
}

pub(crate) fn validate_toc_nodes(
    nodes: &[TocNode],
    path: &str,
    depth: usize,
    units: &HashMap<&str, &ContentUnit>,
    toc_ids: &mut HashSet<String>,
    validator: &mut Validator,
) {
    if nodes.is_empty() {
        return;
    }
    if depth > MAX_DOCUMENT_DEPTH {
        validator.issue(path, format!("TOC nesting exceeds {MAX_DOCUMENT_DEPTH}"));
        return;
    }
    for (index, node) in nodes.iter().enumerate() {
        let node_path = format!("{path}[{index}]");
        validate_id(validator, &format!("{node_path}.id"), &node.id);
        if !node.id.is_empty() && !toc_ids.insert(node.id.clone()) {
            validator.issue(format!("{node_path}.id"), "duplicate TOC-node ID");
        }
        validate_required_text(validator, &format!("{node_path}.label"), &node.label);
        let unit_id = node.target.unit_id();
        validate_id(validator, &format!("{node_path}.target.unit_id"), unit_id);
        match units.get(unit_id).copied() {
            Some(unit) => {
                if let TocTarget::Block { block_id, .. } = &node.target {
                    validate_id(validator, &format!("{node_path}.target.block_id"), block_id);
                    if unit.find_block(block_id).is_none() {
                        validator.issue(
                            format!("{node_path}.target.block_id"),
                            "TOC target block does not exist in content unit",
                        );
                    }
                }
            }
            None => validator.issue(
                format!("{node_path}.target.unit_id"),
                "TOC target content unit does not exist",
            ),
        }
        validate_toc_nodes(
            &node.children,
            &format!("{node_path}.children"),
            depth + 1,
            units,
            toc_ids,
            validator,
        );
    }
}

pub(crate) fn validate_text_range(text: &str, range: TextRange, validator: &mut Validator) {
    let Ok(start) = usize::try_from(range.start_byte) else {
        validator.issue("locator.text_range.start_byte", "offset does not fit usize");
        return;
    };
    let Ok(end) = usize::try_from(range.end_byte) else {
        validator.issue("locator.text_range.end_byte", "offset does not fit usize");
        return;
    };
    if start > end {
        return;
    }
    if end > text.len() {
        validator.issue("locator.text_range", "text range exceeds block text");
        return;
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        validator.issue(
            "locator.text_range",
            "text range must end on UTF-8 character boundaries",
        );
    }
}
