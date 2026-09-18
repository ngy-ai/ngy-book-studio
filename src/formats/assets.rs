//! Turning imported bytes and markup into assets and documents.

use super::*;

pub(crate) fn original_asset(
    source: &ImportSource,
    format: BookFormat,
    media_type: &str,
) -> ImportedAsset {
    asset_from_bytes(
        &format!("original-{format:?}"),
        vec![AssetRole::OriginalSource],
        media_type,
        source.file_name.clone(),
        source.bytes.clone(),
    )
}

pub(crate) fn asset_from_bytes(
    id_seed: &str,
    roles: Vec<AssetRole>,
    media_type: impl Into<String>,
    original_file_name: Option<String>,
    bytes: Arc<Vec<u8>>,
) -> ImportedAsset {
    let content_hash = blake3::hash(bytes.as_slice()).to_hex().to_string();
    let id =
        crate::document::deterministic_id("asset", format!("{id_seed}\0{content_hash}").as_bytes());
    ImportedAsset {
        metadata: AssetRef {
            id,
            roles,
            media_type: media_type.into(),
            original_file_name,
            byte_len: bytes.len() as u64,
            content_hash,
        },
        bytes,
    }
}

pub(crate) fn text_block_document(seed: &str, text: &str) -> BlockDocument {
    let blocks = text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .enumerate()
        .map(|(index, paragraph)| {
            Block::paragraph(
                crate::document::deterministic_id(
                    "block",
                    format!("{seed}\0{index}\0{paragraph}").as_bytes(),
                ),
                paragraph,
            )
        })
        .collect();
    BlockDocument::new(blocks)
}

/// Rewrites media URLs from an imported HTML chapter to opaque canonical
/// asset IDs before the chapter enters the editable AST. Only resources that
/// were declared by the source container are retained. The chapter base and
/// every candidate path are normalized with encoded dot-segments resolved, so
/// an authored `../` can reach a sibling manifest resource but cannot escape
/// the logical archive root.
pub(crate) fn rewrite_imported_html_assets(
    source: &str,
    chapter_href: &str,
    asset_ids_by_href: &HashMap<String, String>,
) -> Result<String> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    rewrite_media_attributes(&dom.document, chapter_href, asset_ids_by_href);

    let serializable = SerializableHandle::from(dom.document.clone());
    let mut bytes = Vec::new();
    serialize(
        &mut bytes,
        &serializable,
        SerializeOpts {
            traversal_scope: TraversalScope::ChildrenOnly(None),
            scripting_enabled: false,
            create_missing_parent: true,
        },
    )
    .context("failed to serialize imported HTML after asset rewriting")?;
    String::from_utf8(bytes).context("rewritten imported HTML is not UTF-8")
}

/// Produces the lookup key used by importers for manifest resources.
pub(crate) fn normalized_archive_href(href: &str) -> Option<String> {
    normalize_archive_path(href.split(['?', '#']).next().unwrap_or(href))
}

pub(crate) fn rewrite_media_attributes(
    node: &Handle,
    chapter_href: &str,
    asset_ids_by_href: &HashMap<String, String>,
) {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let tag = name.local.as_ref();
        attrs.borrow_mut().retain_mut(|attribute| {
            let attribute_name = attribute.name.local.as_ref();
            let is_asset_url = matches!(
                (tag, attribute_name),
                ("img" | "audio" | "video" | "source", "src") | ("video", "poster")
            );
            if !is_asset_url {
                return true;
            }
            let Some(asset_id) = resolve_imported_asset_href(
                chapter_href,
                attribute.value.as_ref(),
                asset_ids_by_href,
            ) else {
                return false;
            };
            attribute.value = format!("ngy-asset:{asset_id}").into();
            true
        });
    }

    // Clone the handles so the RefCell borrow is not held during recursion.
    let children = node.children.borrow().iter().cloned().collect::<Vec<_>>();
    for child in children {
        rewrite_media_attributes(&child, chapter_href, asset_ids_by_href);
    }
}

pub(crate) fn resolve_imported_asset_href<'a>(
    chapter_href: &str,
    reference: &str,
    asset_ids_by_href: &'a HashMap<String, String>,
) -> Option<&'a String> {
    let reference = reference.trim();
    let reference_path = reference.split(['?', '#']).next().unwrap_or(reference);
    if reference_path.is_empty()
        || reference_path.starts_with("//")
        || reference_path.starts_with('\\')
        || reference_path
            .split(['/', '\\'])
            .next()
            .is_some_and(|head| head.contains(':'))
    {
        return None;
    }

    let joined = if reference_path.starts_with('/') {
        reference_path.to_string()
    } else {
        let mut base = normalized_archive_href(chapter_href)?;
        if let Some((directory, _)) = base.rsplit_once('/') {
            base = directory.to_string();
        } else {
            base.clear();
        }
        if base.is_empty() {
            reference_path.to_string()
        } else {
            format!("{base}/{reference_path}")
        }
    };
    let normalized = normalize_archive_path(&joined)?;
    asset_ids_by_href.get(&normalized)
}

pub(crate) fn normalize_archive_path(path: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    if decoded.contains('\\') || decoded.chars().any(char::is_control) {
        return None;
    }
    let mut segments = Vec::new();
    for segment in decoded.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            value => segments.push(value),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

pub(crate) fn safe_title(value: Option<&str>, fallback: &str) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

pub(crate) fn target_path_with_extension(path: &Path, extension: &str) -> PathBuf {
    let mut target = path.to_path_buf();
    target.set_extension(extension);
    target
}
