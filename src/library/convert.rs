//! Small conversions between the model, the database rows and the on-disk
//! names.

use super::*;

pub(crate) fn unit_href(unit: &DocumentUnit) -> Option<String> {
    match unit.source_locator.as_ref() {
        Some(SourceLocator::Epub { href }) => Some(href.clone()),
        Some(SourceLocator::PdfPage { page }) => Some(format!("page:{page}")),
        Some(SourceLocator::OfficeRenderedPage { page }) => Some(format!("office-page:{page}")),
        Some(SourceLocator::OfficeSection { index }) => Some(format!("section:{index}")),
        Some(SourceLocator::Slide { index }) => Some(format!("slide:{index}")),
        Some(SourceLocator::Worksheet { name, range }) => Some(match range {
            Some(range) => format!("sheet:{name}:{range}"),
            None => format!("sheet:{name}"),
        }),
        Some(SourceLocator::KindleSection { index, href }) => {
            href.clone().or_else(|| Some(format!("kindle:{index}")))
        }
        Some(SourceLocator::DjvuPage { page }) => Some(format!("djvu:{page}")),
        Some(SourceLocator::Created) | None => Some(format!("unit:{}", unit.id)),
    }
}

pub(crate) fn non_empty_option(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub(crate) fn merge_document_asset(
    document: &mut BookDocument,
    incoming: DocumentAsset,
) -> Result<()> {
    if let Some(existing) = document
        .assets
        .iter_mut()
        .find(|asset| asset.id == incoming.id)
    {
        ensure!(
            existing.content_hash == incoming.content_hash
                && existing.byte_len == incoming.byte_len
                && existing.media_type == incoming.media_type,
            "同一资源 ID 对应了不同内容"
        );
        for role in incoming.roles {
            existing.add_role(role);
        }
        if existing.original_file_name.is_none() {
            existing.original_file_name = incoming.original_file_name;
        }
    } else {
        document.assets.push(incoming);
    }
    Ok(())
}

pub(crate) fn unique_block_id(unit: &DocumentUnit, asset_id: &str, position: usize) -> String {
    for salt in 0_u64.. {
        let id = deterministic_id(
            "block",
            format!(
                "{}\0{}\0{}\0{}\0{salt}",
                unit.id,
                unit.revision.get(),
                asset_id,
                position
            )
            .as_bytes(),
        );
        if unit.find_block(&id).is_none() {
            return id;
        }
    }
    unreachable!("u64 block ID salt space exhausted")
}

pub(crate) fn media_block(
    draft: &MediaDraft,
    block_id: String,
    asset_id: String,
    caption: Vec<crate::document::Inline>,
) -> Block {
    match draft.kind {
        MediaKind::Image => Block::Image {
            id: block_id,
            asset_id,
            alt: draft.description.clone().unwrap_or_default(),
            title: draft.title.clone(),
            caption,
        },
        MediaKind::Audio => Block::Audio {
            id: block_id,
            asset_id,
            title: draft.title.clone(),
            caption,
        },
        MediaKind::Video => Block::Video {
            id: block_id,
            asset_id,
            poster_asset_id: None,
            title: draft.title.clone(),
            caption,
        },
    }
}

pub(crate) fn replace_media_in_blocks(
    blocks: &mut [Block],
    block_id: &str,
    draft: &MediaDraft,
    asset_id: &str,
) -> bool {
    for block in blocks {
        if block.id() == block_id {
            let old = std::mem::replace(
                block,
                Block::ThematicBreak {
                    id: block_id.to_string(),
                },
            );
            let caption = match old {
                Block::Image { caption, .. }
                | Block::Audio { caption, .. }
                | Block::Video { caption, .. } => draft
                    .description
                    .as_deref()
                    .map(|value| vec![crate::document::Inline::text(value)])
                    .unwrap_or(caption),
                other => {
                    *block = other;
                    return false;
                }
            };
            *block = media_block(draft, block_id.to_string(), asset_id.to_string(), caption);
            return true;
        }
        let found = match block {
            Block::BlockQuote { blocks, .. } => {
                replace_media_in_blocks(blocks, block_id, draft, asset_id)
            }
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| replace_media_in_blocks(&mut item.blocks, block_id, draft, asset_id)),
            _ => false,
        };
        if found {
            return true;
        }
    }
    false
}

pub(crate) fn delete_media_from_blocks(blocks: &mut Vec<Block>, block_id: &str) -> bool {
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
    for block in blocks {
        let removed = match block {
            Block::BlockQuote { blocks, .. } => delete_media_from_blocks(blocks, block_id),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| delete_media_from_blocks(&mut item.blocks, block_id)),
            _ => false,
        };
        if removed {
            return true;
        }
    }
    false
}

pub(crate) fn prune_unreferenced_assets(document: &mut BookDocument) {
    let referenced = document
        .units
        .iter()
        .flat_map(|unit| unit.document.referenced_asset_ids())
        .collect::<HashSet<_>>();
    let cover = document.cover_asset_id.as_deref();
    let original = match &document.source {
        DocumentSource::Imported {
            original_asset_id, ..
        } => Some(original_asset_id.as_str()),
        DocumentSource::Created => None,
    };
    document.assets.retain(|asset| {
        referenced.contains(asset.id.as_str())
            || cover == Some(asset.id.as_str())
            || original == Some(asset.id.as_str())
    });
}

pub(crate) fn asset_kind(roles: &[AssetRole]) -> &'static str {
    if roles.contains(&AssetRole::OriginalSource) {
        "original_source"
    } else if roles.contains(&AssetRole::Cover) {
        "cover"
    } else if roles.contains(&AssetRole::Audio) {
        "audio"
    } else if roles.contains(&AssetRole::Video) {
        "video"
    } else if roles.contains(&AssetRole::Poster) {
        "poster"
    } else if roles.contains(&AssetRole::ContentImage) {
        "image"
    } else if roles.contains(&AssetRole::Font) {
        "font"
    } else if roles.contains(&AssetRole::Stylesheet) {
        "stylesheet"
    } else {
        "attachment"
    }
}

pub(crate) fn asset_roles_from_kind(kind: &str) -> Vec<AssetRole> {
    match kind {
        "original_source" => vec![AssetRole::OriginalSource],
        "cover" => vec![AssetRole::Cover, AssetRole::ContentImage],
        "audio" => vec![AssetRole::Audio],
        "video" => vec![AssetRole::Video],
        "poster" => vec![AssetRole::Poster],
        "image" => vec![AssetRole::ContentImage],
        "font" => vec![AssetRole::Font],
        "stylesheet" => vec![AssetRole::Stylesheet],
        _ => vec![AssetRole::Attachment],
    }
}

pub(crate) fn document_format(document: &BookDocument) -> &'static str {
    match &document.source {
        DocumentSource::Created => "epub",
        DocumentSource::Imported { format, .. } => format_name(*format),
    }
}

pub(crate) fn format_name(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Epub => "epub",
        BookFormat::Pdf => "pdf",
        BookFormat::Doc => "doc",
        BookFormat::Docx => "docx",
        BookFormat::Pptx => "pptx",
        BookFormat::Xlsx => "xlsx",
        BookFormat::Mobi => "mobi",
        BookFormat::Azw => "azw",
        BookFormat::Azw3 => "azw3",
        BookFormat::Kfx => "kfx",
        BookFormat::Djvu => "djvu",
    }
}

pub(crate) fn parse_format(value: &str) -> Result<BookFormat> {
    Ok(match value {
        "epub" => BookFormat::Epub,
        "pdf" => BookFormat::Pdf,
        "doc" => BookFormat::Doc,
        "docx" => BookFormat::Docx,
        "pptx" => BookFormat::Pptx,
        "xlsx" => BookFormat::Xlsx,
        "mobi" => BookFormat::Mobi,
        "azw" => BookFormat::Azw,
        "azw3" => BookFormat::Azw3,
        "kfx" => BookFormat::Kfx,
        "djvu" => BookFormat::Djvu,
        _ => bail!("未知图书格式：{value}"),
    })
}

pub(crate) fn unit_kind(kind: ContentUnitKind) -> &'static str {
    match kind {
        ContentUnitKind::Chapter => "chapter",
        ContentUnitKind::Section => "section",
        ContentUnitKind::Page => "page",
        ContentUnitKind::Slide => "slide",
        ContentUnitKind::Worksheet => "worksheet",
    }
}

pub(crate) fn parse_unit_kind(value: &str) -> Result<ContentUnitKind> {
    Ok(match value {
        "chapter" => ContentUnitKind::Chapter,
        "section" => ContentUnitKind::Section,
        "page" => ContentUnitKind::Page,
        "slide" => ContentUnitKind::Slide,
        "worksheet" => ContentUnitKind::Worksheet,
        _ => bail!("未知内容单元类型：{value}"),
    })
}

pub(crate) fn imported_source_name(document: &BookDocument) -> Option<String> {
    match &document.source {
        DocumentSource::Imported {
            original_file_name, ..
        } => original_file_name.clone(),
        DocumentSource::Created => None,
    }
}

pub(crate) fn split_authors(value: &str) -> Vec<String> {
    value
        .split(['、', ',', ';'])
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "未知作者")
        .map(str::to_string)
        .collect()
}

pub(crate) fn non_empty(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

pub(crate) fn validate_group_name(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(!name.is_empty(), "分组名称不能为空");
    ensure!(
        name.chars().count() <= MAX_GROUP_NAME_LEN,
        "分组名称最多 {MAX_GROUP_NAME_LEN} 个字符"
    );
    Ok(name.to_string())
}

pub(crate) fn safe_file_stem(value: &str) -> String {
    let stem = value
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            character => character,
        })
        .collect::<String>();
    non_empty(&stem, "book")
}

pub(crate) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("无法读取当前目录")?
            .join(path))
    }
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
