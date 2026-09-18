//! Media metadata edits applied directly to the block tree.

use super::*;

pub(crate) fn update_media_metadata(
    blocks: &mut [Block],
    block_id: &str,
    metadata: &EditorMediaMetadataDraft,
) -> bool {
    pub(crate) fn update_inline(
        inlines: &mut [Inline],
        asset_id: &str,
        metadata: &EditorMediaMetadataDraft,
    ) -> bool {
        for inline in inlines {
            match inline {
                Inline::Image {
                    asset_id: current,
                    alt,
                    title,
                } if current == asset_id => {
                    *alt = metadata.description.clone().unwrap_or_default();
                    *title = metadata.title.clone();
                    return true;
                }
                Inline::Emphasis { content }
                | Inline::Strong { content }
                | Inline::Strikethrough { content }
                | Inline::Link { content, .. } => {
                    if update_inline(content, asset_id, metadata) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if let Some(asset_id) = block_id.strip_prefix("inline-image:") {
        for block in blocks {
            let updated = match block {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    update_inline(content, asset_id, metadata)
                }
                Block::Table { header, rows, .. } => header
                    .iter_mut()
                    .chain(rows.iter_mut())
                    .flat_map(|row| row.cells.iter_mut())
                    .any(|cell| update_inline(&mut cell.content, asset_id, metadata)),
                Block::BlockQuote { blocks, .. } => {
                    update_media_metadata(blocks, block_id, metadata)
                }
                Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                    .iter_mut()
                    .any(|item| update_media_metadata(&mut item.blocks, block_id, metadata)),
                _ => false,
            };
            if updated {
                return true;
            }
        }
        return false;
    }

    for block in blocks {
        if block.id() == block_id {
            match block {
                Block::Image {
                    alt,
                    title,
                    caption,
                    ..
                } => {
                    *alt = metadata.description.clone().unwrap_or_default();
                    *title = metadata.title.clone();
                    *caption = editor_caption_from_description(metadata.description.as_deref());
                    return true;
                }
                Block::Audio { title, caption, .. } | Block::Video { title, caption, .. } => {
                    *title = metadata.title.clone();
                    *caption = editor_caption_from_description(metadata.description.as_deref());
                    return true;
                }
                _ => return false,
            }
        }
        let updated = match block {
            Block::BlockQuote { blocks, .. } => update_media_metadata(blocks, block_id, metadata),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| update_media_metadata(&mut item.blocks, block_id, metadata)),
            _ => false,
        };
        if updated {
            return true;
        }
    }
    false
}

pub(crate) fn replace_media_asset(blocks: &mut [Block], block_id: &str, asset_id: &str) -> bool {
    pub(crate) fn replace_inline(
        inlines: &mut [Inline],
        old_asset_id: &str,
        asset_id: &str,
    ) -> bool {
        for inline in inlines {
            match inline {
                Inline::Image {
                    asset_id: current, ..
                } if current == old_asset_id => {
                    *current = asset_id.to_string();
                    return true;
                }
                Inline::Emphasis { content }
                | Inline::Strong { content }
                | Inline::Strikethrough { content }
                | Inline::Link { content, .. } => {
                    if replace_inline(content, old_asset_id, asset_id) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if let Some(old_asset_id) = block_id.strip_prefix("inline-image:") {
        for block in blocks {
            let replaced = match block {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    replace_inline(content, old_asset_id, asset_id)
                }
                Block::Table { header, rows, .. } => header
                    .iter_mut()
                    .chain(rows.iter_mut())
                    .flat_map(|row| row.cells.iter_mut())
                    .any(|cell| replace_inline(&mut cell.content, old_asset_id, asset_id)),
                Block::BlockQuote { blocks, .. } => replace_media_asset(blocks, block_id, asset_id),
                Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                    .iter_mut()
                    .any(|item| replace_media_asset(&mut item.blocks, block_id, asset_id)),
                _ => false,
            };
            if replaced {
                return true;
            }
        }
        return false;
    }
    for block in blocks {
        if block.id() == block_id {
            match block {
                Block::Image {
                    asset_id: current, ..
                }
                | Block::Audio {
                    asset_id: current, ..
                }
                | Block::Video {
                    asset_id: current, ..
                } => {
                    *current = asset_id.to_string();
                    return true;
                }
                _ => return false,
            }
        }
        let replaced = match block {
            Block::BlockQuote { blocks, .. } => replace_media_asset(blocks, block_id, asset_id),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| replace_media_asset(&mut item.blocks, block_id, asset_id)),
            _ => false,
        };
        if replaced {
            return true;
        }
    }
    false
}

pub(crate) fn delete_media(blocks: &mut Vec<Block>, block_id: &str) -> bool {
    pub(crate) fn delete_inline(inlines: &mut Vec<Inline>, asset_id: &str) -> bool {
        if let Some(position) = inlines.iter().position(
            |inline| matches!(inline, Inline::Image { asset_id: current, .. } if current == asset_id),
        ) {
            inlines.remove(position);
            return true;
        }
        inlines.iter_mut().any(|inline| match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => delete_inline(content, asset_id),
            _ => false,
        })
    }

    if let Some(asset_id) = block_id.strip_prefix("inline-image:") {
        return blocks.iter_mut().any(|block| match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                delete_inline(content, asset_id)
            }
            Block::Table { header, rows, .. } => header
                .iter_mut()
                .chain(rows.iter_mut())
                .flat_map(|row| row.cells.iter_mut())
                .any(|cell| delete_inline(&mut cell.content, asset_id)),
            Block::BlockQuote { blocks, .. } => delete_media(blocks, block_id),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| delete_media(&mut item.blocks, block_id)),
            _ => false,
        });
    }
    if let Some(position) = blocks.iter().position(|block| block.id() == block_id) {
        if matches!(
            blocks[position],
            Block::Image { .. } | Block::Audio { .. } | Block::Video { .. }
        ) {
            blocks.remove(position);
            return true;
        }
        return false;
    }
    blocks.iter_mut().any(|block| match block {
        Block::BlockQuote { blocks, .. } => delete_media(blocks, block_id),
        Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
            .iter_mut()
            .any(|item| delete_media(&mut item.blocks, block_id)),
        _ => false,
    })
}

pub(crate) fn prune_editor_assets(document: &mut BookDocument) {
    let referenced = document
        .units
        .iter()
        .flat_map(|unit| unit.document.referenced_asset_ids())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let cover = document.cover_asset_id.clone();
    let original = match &document.source {
        BookSource::Imported {
            original_asset_id, ..
        } => Some(original_asset_id.clone()),
        BookSource::Created => None,
    };
    document.assets.retain(|asset| {
        referenced.contains(&asset.id)
            || cover.as_deref() == Some(asset.id.as_str())
            || original.as_deref() == Some(asset.id.as_str())
    });
}

pub(crate) fn clear_editor_cover_assignment(document: &mut BookDocument) {
    document.cover_asset_id = None;
    for asset in &mut document.assets {
        asset.roles.retain(|role| *role != AssetRole::Cover);
    }
}

pub(crate) fn retain_referenced_pending_assets(
    pending: &mut HashMap<String, Arc<Vec<u8>>>,
    document: &BookDocument,
) {
    let retained = document
        .assets
        .iter()
        .map(|asset| asset.id.as_str())
        .collect::<HashSet<_>>();
    pending.retain(|asset_id, _| retained.contains(asset_id.as_str()));
}

pub(crate) fn editor_book_format_label(document: &BookDocument) -> &'static str {
    match &document.source {
        // Newly created books use the normalized EPUB publishing path.
        BookSource::Created => "EPUB",
        BookSource::Imported { format, .. } => match format {
            BookFormat::Epub => "EPUB",
            BookFormat::Pdf => "PDF",
            BookFormat::Doc => "DOC",
            BookFormat::Docx => "DOCX",
            BookFormat::Pptx => "PPTX",
            BookFormat::Xlsx => "XLSX",
            BookFormat::Mobi => "MOBI",
            BookFormat::Azw => "AZW",
            BookFormat::Azw3 => "AZW3",
            BookFormat::Kfx => "KFX",
            BookFormat::Djvu => "DJVU",
        },
    }
}

pub(crate) fn editor_clear_cover_enabled(has_cover: bool, controls_disabled: bool) -> bool {
    has_cover && !controls_disabled
}

pub(crate) fn unit_kind_label(kind: ContentUnitKind) -> &'static str {
    match kind {
        ContentUnitKind::Chapter => "章节",
        ContentUnitKind::Section => "小节",
        ContentUnitKind::Page => "页面",
        ContentUnitKind::Slide => "幻灯片",
        ContentUnitKind::Worksheet => "工作表",
    }
}

pub(crate) fn next_unit_kind(kind: ContentUnitKind) -> ContentUnitKind {
    match kind {
        ContentUnitKind::Chapter => ContentUnitKind::Section,
        ContentUnitKind::Section => ContentUnitKind::Page,
        ContentUnitKind::Page => ContentUnitKind::Slide,
        ContentUnitKind::Slide => ContentUnitKind::Worksheet,
        ContentUnitKind::Worksheet => ContentUnitKind::Chapter,
    }
}
