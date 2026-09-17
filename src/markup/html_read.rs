//! Small readers over the parsed HTML tree: attributes, media, tables and
//! inline runs.

use super::*;

pub(crate) fn find_html_element(node: &Handle, tag: &str) -> Option<Handle> {
    let mut pending = vec![node.clone()];
    while let Some(node) = pending.pop() {
        if matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == tag) {
            return Some(node);
        }
        pending.extend(node.children.borrow().iter().rev().cloned());
    }
    None
}

pub(crate) fn ensure_html_depth(depth: usize) -> Result<()> {
    if depth > MAX_DOCUMENT_DEPTH {
        bail!("HTML nesting exceeds {MAX_DOCUMENT_DEPTH}");
    }
    Ok(())
}

/// Checks the parsed DOM iteratively before any recursive AST conversion or
/// plain-text extraction. The root itself is a transport wrapper (`body` or
/// `document`), so its first element children start at model depth zero.
pub(crate) fn ensure_html_dom_depth(root: &Handle) -> Result<()> {
    let mut pending = root
        .children
        .borrow()
        .iter()
        .rev()
        .cloned()
        .map(|node| (node, 0_usize))
        .collect::<Vec<_>>();
    while let Some((node, depth)) = pending.pop() {
        let is_element = matches!(&node.data, NodeData::Element { .. });
        if is_element {
            ensure_html_depth(depth)?;
        }
        let child_depth = if is_element { depth + 1 } else { depth };
        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .cloned()
                .map(|child| (child, child_depth)),
        );
    }
    Ok(())
}

pub(crate) fn html_attribute(node: &Handle, attribute: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|item| item.name.local.as_ref() == attribute)
        .map(|item| item.value.to_string())
        .filter(|value| !value.trim().is_empty())
}

pub(crate) fn serialize_html_node(node: &Handle) -> Result<String> {
    let serializable = SerializableHandle::from(node.clone());
    let mut bytes = Vec::new();
    serialize(
        &mut bytes,
        &serializable,
        SerializeOpts {
            traversal_scope: TraversalScope::IncludeNode,
            scripting_enabled: false,
            create_missing_parent: true,
        },
    )
    .context("无法规范化 HTML 节点")?;
    String::from_utf8(bytes).context("HTML 序列化结果不是 UTF-8")
}

pub(crate) fn first_descendant(node: &Handle, tags: &[&str]) -> Result<Option<Handle>> {
    pub(crate) fn find(node: &Handle, tags: &[&str], depth: usize) -> Result<Option<Handle>> {
        ensure_html_depth(depth)?;
        if matches!(&node.data, NodeData::Element { name, .. } if tags.contains(&name.local.as_ref()))
        {
            return Ok(Some(node.clone()));
        }
        for child in node.children.borrow().iter() {
            let child_depth = if matches!(&child.data, NodeData::Element { .. }) {
                depth + 1
            } else {
                depth
            };
            if let Some(found) = find(child, tags, child_depth)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    find(node, tags, 0)
}

pub(crate) fn first_media_source(node: &Handle) -> Result<Option<String>> {
    if let Some(source) = html_attribute(node, "src") {
        return Ok(Some(source));
    }
    Ok(first_descendant(node, &["source"])?.and_then(|source| html_attribute(&source, "src")))
}

pub(crate) fn html_media_block(node: &Handle, id: String, depth: usize) -> Result<Option<Block>> {
    let Some(media) = first_descendant(node, &["img", "audio", "video"])? else {
        return Ok(None);
    };
    let NodeData::Element { name, .. } = &media.data else {
        return Ok(None);
    };
    let tag = name.local.as_ref();
    let Some(source) = first_media_source(&media)? else {
        return Ok(None);
    };
    let Some(asset_id) = asset_id_from_href(&source) else {
        return Ok(None);
    };
    let caption = first_descendant(node, &["figcaption"])?
        .map(|caption| html_inlines(&caption, depth))
        .transpose()?
        .unwrap_or_default();
    let title = html_attribute(&media, "title");
    Ok(Some(match tag {
        "img" => Block::Image {
            id,
            asset_id,
            alt: html_attribute(&media, "alt").unwrap_or_default(),
            title,
            caption,
        },
        "audio" => Block::Audio {
            id,
            asset_id,
            title,
            caption,
        },
        "video" => Block::Video {
            id,
            asset_id,
            poster_asset_id: html_attribute(&media, "poster")
                .as_deref()
                .and_then(asset_id_from_href),
            title,
            caption,
        },
        _ => return Ok(None),
    }))
}

pub(crate) fn html_inlines(node: &Handle, depth: usize) -> Result<Vec<Inline>> {
    let mut output = Vec::new();
    for child in node.children.borrow().iter() {
        collect_html_inline(child, &mut output, depth + 1)?;
    }
    coalesce_inline_text(&mut output);
    Ok(output)
}

pub(crate) fn coalesce_inline_text(inlines: &mut Vec<Inline>) {
    let mut merged = Vec::with_capacity(inlines.len());
    for inline in std::mem::take(inlines) {
        if let Inline::Text { value } = inline {
            if value.is_empty() {
                continue;
            }
            if let Some(Inline::Text { value: previous }) = merged.last_mut() {
                previous.push_str(&value);
            } else {
                merged.push(Inline::text(value));
            }
        } else {
            merged.push(inline);
        }
    }
    *inlines = merged;
}

pub(crate) fn collect_html_inline(
    node: &Handle,
    output: &mut Vec<Inline>,
    depth: usize,
) -> Result<()> {
    ensure_html_depth(depth)?;
    match &node.data {
        NodeData::Text { contents } => output.push(Inline::text(contents.borrow().to_string())),
        NodeData::Element { name, .. } => {
            let tag = name.local.as_ref();
            match tag {
                "em" | "i" => output.push(Inline::Emphasis {
                    content: html_inlines(node, depth)?,
                }),
                "strong" | "b" => output.push(Inline::Strong {
                    content: html_inlines(node, depth)?,
                }),
                "s" | "del" | "strike" => output.push(Inline::Strikethrough {
                    content: html_inlines(node, depth)?,
                }),
                "code" => output.push(Inline::Code {
                    value: html_text(node, depth)?,
                }),
                "a" => {
                    let content = html_inlines(node, depth)?;
                    if let Some(href) = html_attribute(node, "href").filter(|href| safe_link(href))
                    {
                        output.push(Inline::Link {
                            href,
                            title: html_attribute(node, "title"),
                            content,
                        });
                    } else {
                        output.extend(content);
                    }
                }
                "br" => output.push(Inline::HardBreak),
                "input" => {}
                "p" => {
                    if !output.is_empty() {
                        output.push(Inline::HardBreak);
                    }
                    output.extend(html_inlines(node, depth)?);
                }
                "img" => {
                    let asset_id = html_attribute(node, "src")
                        .as_deref()
                        .and_then(asset_id_from_href);
                    if let Some(asset_id) = asset_id {
                        output.push(Inline::Image {
                            asset_id,
                            alt: html_attribute(node, "alt").unwrap_or_default(),
                            title: html_attribute(node, "title"),
                        });
                    } else if let Some(alt) = html_attribute(node, "alt") {
                        output.push(Inline::text(alt));
                    }
                }
                _ => {
                    let source = sanitize_html(&serialize_html_node(node)?);
                    if !source.trim().is_empty() {
                        output.push(Inline::RawHtml {
                            plain_text: raw_html_plain_text(&source)
                                .context("HTML nesting exceeds the document limit")?,
                            source,
                        });
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn html_text(node: &Handle, depth: usize) -> Result<String> {
    let mut output = String::new();

    pub(crate) fn collect(node: &Handle, output: &mut String, depth: usize) -> Result<()> {
        ensure_html_depth(depth)?;
        if let NodeData::Text { contents } = &node.data {
            output.push_str(&contents.borrow());
        }
        for child in node.children.borrow().iter() {
            let child_depth = if matches!(&child.data, NodeData::Element { .. }) {
                depth + 1
            } else {
                depth
            };
            collect(child, output, child_depth)?;
        }
        Ok(())
    }
    collect(node, &mut output, depth)?;
    Ok(output)
}

pub(crate) fn safe_link(href: &str) -> bool {
    let href = href.trim();
    if href.is_empty() || href.starts_with("//") || href.starts_with('\\') {
        return false;
    }
    let lower = href.to_ascii_lowercase();
    !lower.contains(':')
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("asset:")
        || lower.starts_with("ngy-asset:")
        || lower.starts_with("data:image/")
}

pub(crate) fn asset_id_from_href(href: &str) -> Option<String> {
    href.strip_prefix("ngy-asset:")
        .or_else(|| href.strip_prefix("asset:"))
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.contains(['/', '\\', '#', '?']))
        .map(str::to_owned)
}
