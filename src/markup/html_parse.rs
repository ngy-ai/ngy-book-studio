//! Cleaning and reading HTML into the block model.

use super::*;

pub fn sanitize_html(source: &str) -> String {
    pub(crate) fn safe_relative(url: &str) -> Option<Cow<'_, str>> {
        let trimmed = url.trim();
        if trimmed.starts_with("//") || trimmed.starts_with('\\') {
            None
        } else {
            Some(Cow::Borrowed(url))
        }
    }

    let mut builder = ammonia::Builder::default();
    builder
        .add_tags([
            "audio",
            "video",
            "source",
            "picture",
            "figure",
            "figcaption",
            "track",
            "input",
        ])
        .add_tag_attributes("input", ["checked"])
        .set_tag_attribute_value("input", "type", "checkbox")
        .set_tag_attribute_value("input", "disabled", "")
        .add_generic_attributes([
            "class", "id", "title", "alt", "src", "href", "poster", "controls", "width", "height",
            "colspan", "rowspan", "preload", "kind", "srclang", "label",
        ])
        // Network schemes are deliberately absent. Relative links are retained
        // for EPUB asset rewriting and `data:` is useful for small safe images.
        .url_schemes(HashSet::from(["data", "ngy-asset", "asset"]))
        .url_relative(ammonia::UrlRelative::Custom(Box::new(safe_relative)));
    builder.clean(source).to_string()
}

pub(crate) fn parse_html(source: &str, unit_id: &str) -> Result<BlockDocument> {
    let source = sanitize_html(source);
    if source.trim().is_empty() {
        return Ok(BlockDocument::default());
    }
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    let body = find_html_element(&dom.document, "body").unwrap_or_else(|| dom.document.clone());
    ensure_html_dom_depth(&body)?;
    let mut parser = HtmlAstParser {
        unit_id,
        block_index: 0,
    };
    let blocks = parser.children(&body, 0)?;
    Ok(BlockDocument::new(blocks))
}

pub(crate) struct HtmlAstParser<'a> {
    pub(crate) unit_id: &'a str,
    pub(crate) block_index: usize,
}

impl HtmlAstParser<'_> {
    pub(crate) fn children(&mut self, node: &Handle, depth: usize) -> Result<Vec<Block>> {
        let mut blocks = Vec::new();
        let mut inlines = Vec::new();
        for child in node.children.borrow().iter() {
            let inline = match &child.data {
                NodeData::Text { .. } => true,
                NodeData::Element { name, .. } => matches!(
                    name.local.as_ref(),
                    "a" | "em"
                        | "i"
                        | "strong"
                        | "b"
                        | "s"
                        | "del"
                        | "strike"
                        | "code"
                        | "br"
                        | "span"
                        | "sub"
                        | "sup"
                        | "input"
                ),
                _ => false,
            };
            if inline {
                collect_html_inline(child, &mut inlines, depth)?;
            } else {
                self.flush_inlines(&mut inlines, &mut blocks);
                self.push_block(child, &mut blocks, depth)?;
            }
        }
        self.flush_inlines(&mut inlines, &mut blocks);
        Ok(blocks)
    }

    pub(crate) fn flush_inlines(&mut self, inlines: &mut Vec<Inline>, blocks: &mut Vec<Block>) {
        coalesce_inline_text(inlines);
        if inlines
            .iter()
            .any(|inline| !matches!(inline, Inline::Text { value } if value.trim().is_empty()))
        {
            blocks.push(Block::Paragraph {
                id: self.next_block_id("paragraph"),
                content: std::mem::take(inlines),
            });
        } else {
            inlines.clear();
        }
    }

    pub(crate) fn next_block_id(&mut self, kind: &str) -> String {
        let index = self.block_index;
        self.block_index += 1;
        deterministic_id(
            "block",
            format!("{}\0{kind}\0{index}", self.unit_id).as_bytes(),
        )
    }

    pub(crate) fn push_block(
        &mut self,
        node: &Handle,
        output: &mut Vec<Block>,
        depth: usize,
    ) -> Result<()> {
        ensure_html_depth(depth)?;
        match &node.data {
            NodeData::Text { contents } => {
                let text = contents.borrow();
                if !text.trim().is_empty() {
                    output.push(Block::Paragraph {
                        id: self.next_block_id("paragraph"),
                        content: vec![Inline::text(text.to_string())],
                    });
                }
            }
            NodeData::Element { name, .. } => {
                let tag = name.local.as_ref();
                match tag {
                    "p" => output.push(Block::Paragraph {
                        id: self.next_block_id("paragraph"),
                        content: html_inlines(node, depth)?,
                    }),
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        output.push(Block::Heading {
                            id: self.next_block_id("heading"),
                            level: tag[1..].parse().unwrap_or(1),
                            content: html_inlines(node, depth)?,
                        });
                    }
                    "img" | "audio" | "video" | "figure" | "picture" => {
                        // Canonical serialization wraps every media block in a
                        // `<figure>`. Derive IDs from the semantic media tag,
                        // not from the authored wrapper, so parsing the saved
                        // HTML again does not change block identity.
                        let media_kind = first_descendant(node, &["img", "audio", "video"])?
                            .and_then(|media| match &media.data {
                                NodeData::Element { name, .. } => Some(name.local.to_string()),
                                _ => None,
                            })
                            .unwrap_or_else(|| tag.to_string());
                        if let Some(block) =
                            html_media_block(node, self.next_block_id(&media_kind), depth)?
                        {
                            output.push(block);
                        } else {
                            self.push_raw_html(node, output)?;
                        }
                    }
                    "hr" => output.push(Block::ThematicBreak {
                        id: self.next_block_id("break"),
                    }),
                    "blockquote" => {
                        let id = self.next_block_id("quote");
                        let blocks = self.children(node, depth + 1)?;
                        if blocks.is_empty() {
                            self.push_raw_html(node, output)?;
                        } else {
                            output.push(Block::BlockQuote { id, blocks });
                        }
                    }
                    "ul" | "ol" => {
                        if node
                            .children
                            .borrow()
                            .iter()
                            .any(|child| match &child.data {
                                NodeData::Text { contents } => !contents.borrow().trim().is_empty(),
                                NodeData::Element { .. } => {
                                    !has_html_tag(child, "li") || has_html_attribute(child, "value")
                                }
                                _ => false,
                            })
                        {
                            self.push_raw_html(node, output)?;
                            return Ok(());
                        }
                        let id = self.next_block_id("list");
                        let mut items = Vec::new();
                        for child in node.children.borrow().iter() {
                            if !has_html_tag(child, "li") {
                                continue;
                            }
                            let marker = html_task_marker(child);
                            let mut blocks = self.children(child, depth + 2)?;
                            if blocks.is_empty() {
                                blocks.push(Block::Paragraph {
                                    id: self.next_block_id("paragraph"),
                                    content: Vec::new(),
                                });
                            }
                            items.push(ListItem {
                                checked: marker,
                                blocks,
                            });
                        }
                        let start = html_attribute(node, "start")
                            .and_then(|value| value.parse::<u64>().ok());
                        if items.is_empty()
                            || (tag == "ol"
                                && has_html_attribute(node, "start")
                                && start.is_none_or(|value| value == 0))
                        {
                            self.push_raw_html(node, output)?;
                        } else if tag == "ul" {
                            output.push(Block::BulletList { id, items });
                        } else {
                            output.push(Block::OrderedList {
                                id,
                                start: start.unwrap_or(1),
                                items,
                            });
                        }
                    }
                    "pre" => {
                        let code =
                            first_descendant(node, &["code"])?.unwrap_or_else(|| node.clone());
                        let mut pending = vec![node.clone()];
                        let mut plain_code = true;
                        while let Some(parent) = pending.pop() {
                            for child in parent.children.borrow().iter() {
                                if matches!(&child.data, NodeData::Element { .. }) {
                                    plain_code &= has_html_tag(child, "code");
                                    pending.push(child.clone());
                                }
                            }
                        }
                        if !plain_code {
                            self.push_raw_html(node, output)?;
                            return Ok(());
                        }
                        output.push(Block::CodeBlock {
                            id: self.next_block_id("code"),
                            language: html_attribute(&code, "class").and_then(|classes| {
                                classes.split_whitespace().find_map(|class| {
                                    class
                                        .strip_prefix("language-")
                                        .filter(|value| !value.is_empty())
                                        .map(str::to_owned)
                                })
                            }),
                            // Text outside the optional code wrapper is visible
                            // preformatted content too and must survive saving.
                            code: html_text(node, depth)?,
                        });
                    }
                    "table" => {
                        if let Some((header, rows)) = html_table(node, depth)? {
                            output.push(Block::Table {
                                id: self.next_block_id("table"),
                                header,
                                rows,
                            });
                        } else {
                            self.push_raw_html(node, output)?;
                        }
                    }
                    _ => self.push_raw_html(node, output)?,
                }
            }
            NodeData::Document => {
                for child in node.children.borrow().iter() {
                    self.push_block(child, output, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn push_raw_html(&mut self, node: &Handle, output: &mut Vec<Block>) -> Result<()> {
        let source = serialize_html_node(node)?;
        let source = sanitize_html(&source);
        if !source.trim().is_empty() {
            output.push(Block::RawHtml {
                id: self.next_block_id("html"),
                plain_text: raw_html_plain_text(&source)
                    .context("HTML nesting exceeds the document limit")?,
                source,
            });
        }
        Ok(())
    }
}

pub(crate) fn has_html_tag(node: &Handle, tag: &str) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == tag)
}

pub(crate) fn has_html_attribute(node: &Handle, attribute: &str) -> bool {
    matches!(&node.data, NodeData::Element { attrs, .. } if attrs.borrow().iter().any(|item| item.name.local.as_ref() == attribute))
}

pub(crate) fn html_task_marker(item: &Handle) -> Option<bool> {
    item.children.borrow().iter().find_map(|child| {
        if has_html_tag(child, "input") {
            Some(has_html_attribute(child, "checked"))
        } else if has_html_tag(child, "p") {
            child.children.borrow().iter().find_map(|inline| {
                has_html_tag(inline, "input").then(|| has_html_attribute(inline, "checked"))
            })
        } else {
            None
        }
    })
}

pub(crate) fn html_table(
    node: &Handle,
    depth: usize,
) -> Result<Option<(Option<TableRow>, Vec<TableRow>)>> {
    // The model has one optional header row and unmerged inline cells. Preserve
    // richer tables as cleaned HTML instead of discarding spans or nested blocks.
    let mut pending = vec![(node.clone(), depth)];
    let mut header = None;
    let mut rows = Vec::new();
    while let Some((node, depth)) = pending.pop() {
        ensure_html_depth(depth)?;
        if has_html_tag(&node, "tr") {
            let mut cells = Vec::new();
            let mut header_cells = 0;
            for cell in node.children.borrow().iter() {
                if !(has_html_tag(cell, "td") || has_html_tag(cell, "th")) {
                    continue;
                }
                if ["colspan", "rowspan"].iter().any(|attribute| {
                    html_attribute(cell, attribute).is_some_and(|value| value != "1")
                }) {
                    return Ok(None);
                }
                if first_descendant(cell, &["table", "ul", "ol", "pre", "blockquote"])?.is_some() {
                    return Ok(None);
                }
                header_cells += usize::from(has_html_tag(cell, "th"));
                cells.push(TableCell::new(html_inlines(cell, depth + 1)?));
            }
            if cells.is_empty() {
                continue;
            }
            if header_cells > 0 {
                if header_cells != cells.len() || header.is_some() || !rows.is_empty() {
                    return Ok(None);
                }
                header = Some(TableRow::new(cells));
            } else {
                rows.push(TableRow::new(cells));
            }
        } else {
            for child in node.children.borrow().iter().rev() {
                if has_html_tag(child, "caption")
                    || has_html_tag(child, "colgroup")
                    || has_html_tag(child, "tfoot")
                {
                    return Ok(None);
                }
                pending.push((child.clone(), depth + 1));
            }
        }
    }
    let width = header
        .as_ref()
        .or_else(|| rows.first())
        .map_or(0, |row| row.cells.len());
    if width == 0 || rows.iter().any(|row| row.cells.len() != width) {
        return Ok(None);
    }
    Ok(Some((header, rows)))
}
