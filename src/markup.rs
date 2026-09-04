//! Parsing and canonical serialization for editable Markdown and HTML sources.
//!
//! A source edit is not publishable until it passes through this module.  The
//! returned source is normalized and every raw HTML fragment has been cleaned;
//! callers persist it together with the block AST as one revision.

use std::{borrow::Cow, collections::HashSet};

use anyhow::{Context as _, Result, bail};
use html5ever::{
    parse_document,
    serialize::{SerializeOpts, TraversalScope, serialize},
    tendril::TendrilSink as _,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::document::{
    Block, BlockDocument, Inline, ListItem, MAX_DOCUMENT_DEPTH, SourceKind, TableCell, TableRow,
    deterministic_id, raw_html_plain_text,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub source_kind: SourceKind,
    pub canonical_source: String,
    pub document: BlockDocument,
}

/// Parses, cleans and normalizes an editable source using a content-derived ID
/// seed. Use [`parse_source_for_unit`] when a stable unit ID is available.
pub fn parse_source(source_kind: SourceKind, source: &str) -> Result<ParsedSource> {
    let seed = deterministic_id("source", source.as_bytes());
    parse_source_for_unit(source_kind, source, &seed)
}

/// Parses, cleans and normalizes an editable source.  `unit_id` is used only
/// for deterministic block IDs and never appears in the serialized source.
pub fn parse_source_for_unit(
    source_kind: SourceKind,
    source: &str,
    unit_id: &str,
) -> Result<ParsedSource> {
    if source.contains('\0') {
        bail!("正文不能包含 NUL 字符");
    }
    let document = match source_kind {
        SourceKind::Markdown => parse_markdown(source, unit_id)?,
        SourceKind::Html => parse_html(source, unit_id)?,
    };
    document.validate().context("正文块结构无效")?;
    let canonical_source = serialize_source(&document, source_kind)?;
    Ok(ParsedSource {
        source_kind,
        canonical_source,
        document,
    })
}

pub fn serialize_source(document: &BlockDocument, source_kind: SourceKind) -> Result<String> {
    document.validate().context("无法序列化无效的正文块结构")?;
    let mut output = String::new();
    match source_kind {
        SourceKind::Markdown => write_markdown_blocks(&document.blocks, 0, &mut output),
        SourceKind::Html => write_html_blocks(&document.blocks, &mut output),
    }
    Ok(output.trim().to_string())
}

pub fn sanitize_html(source: &str) -> String {
    fn safe_relative(url: &str) -> Option<Cow<'_, str>> {
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
        ])
        .add_generic_attributes([
            "class", "id", "title", "alt", "src", "href", "poster", "controls", "width", "height",
            "colspan", "rowspan", "preload", "kind", "srclang", "label",
        ])
        // Network schemes are deliberately absent. Relative links are retained
        // for EPUB asset rewriting and `data:` is useful for small safe images.
        .url_schemes(HashSet::from(["data", "moye-asset", "asset"]))
        .url_relative(ammonia::UrlRelative::Custom(Box::new(safe_relative)));
    builder.clean(source).to_string()
}

fn parse_html(source: &str, unit_id: &str) -> Result<BlockDocument> {
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
    let mut blocks = Vec::new();
    for child in body.children.borrow().iter() {
        parser.push_block(child, &mut blocks, 0)?;
    }
    Ok(BlockDocument::new(blocks))
}

struct HtmlAstParser<'a> {
    unit_id: &'a str,
    block_index: usize,
}

impl HtmlAstParser<'_> {
    fn next_block_id(&mut self, kind: &str) -> String {
        let index = self.block_index;
        self.block_index += 1;
        deterministic_id(
            "block",
            format!("{}\0{kind}\0{index}", self.unit_id).as_bytes(),
        )
    }

    fn push_block(&mut self, node: &Handle, output: &mut Vec<Block>, depth: usize) -> Result<()> {
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

    fn push_raw_html(&mut self, node: &Handle, output: &mut Vec<Block>) -> Result<()> {
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

fn find_html_element(node: &Handle, tag: &str) -> Option<Handle> {
    let mut pending = vec![node.clone()];
    while let Some(node) = pending.pop() {
        if matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == tag) {
            return Some(node);
        }
        pending.extend(node.children.borrow().iter().rev().cloned());
    }
    None
}

fn ensure_html_depth(depth: usize) -> Result<()> {
    if depth > MAX_DOCUMENT_DEPTH {
        bail!("HTML nesting exceeds {MAX_DOCUMENT_DEPTH}");
    }
    Ok(())
}

/// Checks the parsed DOM iteratively before any recursive AST conversion or
/// plain-text extraction. The root itself is a transport wrapper (`body` or
/// `document`), so its first element children start at model depth zero.
fn ensure_html_dom_depth(root: &Handle) -> Result<()> {
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

fn html_attribute(node: &Handle, attribute: &str) -> Option<String> {
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

fn serialize_html_node(node: &Handle) -> Result<String> {
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

fn first_descendant(node: &Handle, tags: &[&str]) -> Result<Option<Handle>> {
    fn find(node: &Handle, tags: &[&str], depth: usize) -> Result<Option<Handle>> {
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

fn first_media_source(node: &Handle) -> Result<Option<String>> {
    if let Some(source) = html_attribute(node, "src") {
        return Ok(Some(source));
    }
    Ok(first_descendant(node, &["source"])?.and_then(|source| html_attribute(&source, "src")))
}

fn html_media_block(node: &Handle, id: String, depth: usize) -> Result<Option<Block>> {
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

fn html_inlines(node: &Handle, depth: usize) -> Result<Vec<Inline>> {
    fn collect(node: &Handle, output: &mut Vec<Inline>, depth: usize) -> Result<()> {
        ensure_html_depth(depth)?;
        for child in node.children.borrow().iter() {
            match &child.data {
                NodeData::Text { contents } => {
                    output.push(Inline::text(contents.borrow().to_string()))
                }
                NodeData::Element { name, .. } => {
                    let tag = name.local.as_ref();
                    match tag {
                        "em" | "i" => output.push(Inline::Emphasis {
                            content: nested(child, depth + 1)?,
                        }),
                        "strong" | "b" => output.push(Inline::Strong {
                            content: nested(child, depth + 1)?,
                        }),
                        "s" | "del" | "strike" => output.push(Inline::Strikethrough {
                            content: nested(child, depth + 1)?,
                        }),
                        "code" => output.push(Inline::Code {
                            value: html_text(child, depth + 1)?,
                        }),
                        "a" => {
                            let content = nested(child, depth + 1)?;
                            if let Some(href) =
                                html_attribute(child, "href").filter(|href| safe_link(href))
                            {
                                output.push(Inline::Link {
                                    href,
                                    title: html_attribute(child, "title"),
                                    content,
                                });
                            } else {
                                output.extend(content);
                            }
                        }
                        "br" => output.push(Inline::HardBreak),
                        "img" => {
                            let asset_id = html_attribute(child, "src")
                                .as_deref()
                                .and_then(asset_id_from_href);
                            if let Some(asset_id) = asset_id {
                                output.push(Inline::Image {
                                    asset_id,
                                    alt: html_attribute(child, "alt").unwrap_or_default(),
                                    title: html_attribute(child, "title"),
                                });
                            } else if let Some(alt) = html_attribute(child, "alt") {
                                output.push(Inline::text(alt));
                            }
                        }
                        _ => {
                            let source = sanitize_html(&serialize_html_node(child)?);
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
        }
        Ok(())
    }

    fn nested(node: &Handle, depth: usize) -> Result<Vec<Inline>> {
        let mut output = Vec::new();
        collect(node, &mut output, depth)?;
        Ok(output)
    }

    let mut output = Vec::new();
    collect(node, &mut output, depth)?;
    Ok(output)
}

fn html_text(node: &Handle, depth: usize) -> Result<String> {
    let mut output = String::new();
    fn collect(node: &Handle, output: &mut String, depth: usize) -> Result<()> {
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

fn parse_markdown(source: &str, unit_id: &str) -> Result<BlockDocument> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let events = Parser::new_ext(source, options).collect::<Vec<_>>();
    let mut parser = MarkdownAstParser {
        events,
        cursor: 0,
        unit_id,
        block_index: 0,
    };
    let mut blocks = parser.parse_blocks(None, 0)?;
    if parser.cursor != parser.events.len() {
        bail!("Markdown 解析没有完整消费输入");
    }
    apply_gfm_autolink_literals(&mut blocks);
    Ok(BlockDocument::new(blocks))
}

fn apply_gfm_autolink_literals(blocks: &mut [Block]) {
    for block in blocks {
        match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                autolink_inlines(content);
            }
            Block::BlockQuote { blocks, .. } => apply_gfm_autolink_literals(blocks),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                for item in items {
                    apply_gfm_autolink_literals(&mut item.blocks);
                }
            }
            Block::Table { header, rows, .. } => {
                for row in header.iter_mut().chain(rows.iter_mut()) {
                    for cell in &mut row.cells {
                        autolink_inlines(&mut cell.content);
                    }
                }
            }
            Block::Image { caption, .. }
            | Block::Audio { caption, .. }
            | Block::Video { caption, .. } => autolink_inlines(caption),
            Block::CodeBlock { .. } | Block::ThematicBreak { .. } | Block::RawHtml { .. } => {}
        }
    }
}

fn autolink_inlines(inlines: &mut Vec<Inline>) {
    let mut linked = Vec::with_capacity(inlines.len());
    let mut adjacent_text = String::new();
    for mut inline in std::mem::take(inlines) {
        match &mut inline {
            Inline::Text { value } => {
                adjacent_text.push_str(value);
                continue;
            }
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content } => autolink_inlines(content),
            // A link label is normalized but never autolinked: GFM autolinks
            // must not create nested links inside explicit Markdown links.
            Inline::Link { content, .. } => coalesce_inline_text(content),
            Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::Image { .. }
            | Inline::RawHtml { .. } => {}
        }
        if !adjacent_text.is_empty() {
            linked.extend(split_gfm_autolink_text(&adjacent_text));
            adjacent_text.clear();
        }
        linked.push(inline);
    }
    if !adjacent_text.is_empty() {
        linked.extend(split_gfm_autolink_text(&adjacent_text));
    }
    *inlines = linked;
}

fn coalesce_inline_text(inlines: &mut Vec<Inline>) {
    let mut normalized = Vec::with_capacity(inlines.len());
    let mut adjacent_text = String::new();
    for mut inline in std::mem::take(inlines) {
        match &mut inline {
            Inline::Text { value } => {
                adjacent_text.push_str(value);
                continue;
            }
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => coalesce_inline_text(content),
            Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::Image { .. }
            | Inline::RawHtml { .. } => {}
        }
        if !adjacent_text.is_empty() {
            normalized.push(Inline::text(std::mem::take(&mut adjacent_text)));
        }
        normalized.push(inline);
    }
    if !adjacent_text.is_empty() {
        normalized.push(Inline::text(adjacent_text));
    }
    *inlines = normalized;
}

fn split_gfm_autolink_text(text: &str) -> Vec<Inline> {
    let mut output = Vec::new();
    let mut emitted = 0;
    let mut search_cursor = 0;
    while let Some((start, scheme_len, add_http_scheme)) = next_gfm_url(text, search_cursor) {
        let token_end = text[start..]
            .char_indices()
            .find_map(|(offset, character)| {
                (character.is_whitespace()
                    || matches!(
                        character,
                        '<' | '>' | '"' | '\'' | '`' | '。' | '，' | '：' | '；' | '！' | '？'
                    ))
                .then_some(start + offset)
            })
            .unwrap_or(text.len());
        let end = trim_gfm_url_end(text, start, token_end);
        if end <= start + scheme_len {
            search_cursor = start + scheme_len;
            continue;
        }
        if start > emitted {
            output.push(Inline::text(&text[emitted..start]));
        }
        let label = &text[start..end];
        let href = if add_http_scheme {
            format!("http://{label}")
        } else {
            label.to_string()
        };
        output.push(Inline::Link {
            href,
            title: None,
            content: vec![Inline::text(label)],
        });
        emitted = end;
        search_cursor = end;
    }
    if emitted < text.len() {
        output.push(Inline::text(&text[emitted..]));
    }
    if output.is_empty() {
        output.push(Inline::text(text));
    }
    output
}

fn next_gfm_url(text: &str, cursor: usize) -> Option<(usize, usize, bool)> {
    for (offset, _) in text[cursor..].char_indices() {
        let start = cursor + offset;
        let preceding_is_word = text[..start]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || matches!(character, '_' | '@'));
        if preceding_is_word {
            continue;
        }
        let remaining = &text[start..];
        if remaining
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        {
            return Some((start, 8, false));
        }
        if remaining
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        {
            return Some((start, 7, false));
        }
        if remaining
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("www."))
        {
            return Some((start, 4, true));
        }
    }
    None
}

fn trim_gfm_url_end(text: &str, start: usize, mut end: usize) -> usize {
    while end > start {
        let candidate = &text[start..end];
        let last = candidate
            .chars()
            .next_back()
            .expect("non-empty URL candidate");
        let trim = matches!(
            last,
            '.' | ',' | ':' | ';' | '!' | '?' | '。' | '，' | '：' | '；' | '！' | '？'
        ) || (last == ')'
            && candidate.matches(')').count() > candidate.matches('(').count())
            || (last == ']' && candidate.matches(']').count() > candidate.matches('[').count())
            || (last == '}' && candidate.matches('}').count() > candidate.matches('{').count());
        if !trim {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

struct MarkdownAstParser<'a> {
    events: Vec<Event<'a>>,
    cursor: usize,
    unit_id: &'a str,
    block_index: usize,
}

impl MarkdownAstParser<'_> {
    fn next_block_id(&mut self, kind: &str) -> String {
        let index = self.block_index;
        self.block_index += 1;
        deterministic_id(
            "block",
            format!("{}\0{kind}\0{index}", self.unit_id).as_bytes(),
        )
    }

    fn parse_blocks(&mut self, expected_end: Option<TagEnd>, depth: usize) -> Result<Vec<Block>> {
        ensure_markdown_depth(depth)?;
        let mut blocks = Vec::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::End(end) => {
                    if expected_end.as_ref() == Some(&end) {
                        self.cursor += 1;
                        return Ok(blocks);
                    }
                    bail!("Markdown 块结束标记不匹配: {end:?}");
                }
                Event::Start(tag) => {
                    self.cursor += 1;
                    match tag {
                        Tag::Paragraph => {
                            let content = self.parse_inlines(TagEnd::Paragraph, depth + 1)?;
                            blocks.push(Block::Paragraph {
                                id: self.next_block_id("paragraph"),
                                content,
                            });
                        }
                        Tag::Heading { level, .. } => {
                            let content = self.parse_inlines(TagEnd::Heading(level), depth + 1)?;
                            blocks.push(Block::Heading {
                                id: self.next_block_id("heading"),
                                level: heading_level(level),
                                content,
                            });
                        }
                        Tag::BlockQuote(kind) => {
                            let children =
                                self.parse_blocks(Some(TagEnd::BlockQuote(kind)), depth + 1)?;
                            blocks.push(Block::BlockQuote {
                                id: self.next_block_id("quote"),
                                blocks: children,
                            });
                        }
                        Tag::List(start) => blocks.push(self.parse_list(start, depth)?),
                        Tag::CodeBlock(kind) => blocks.push(self.parse_code_block(kind)?),
                        Tag::HtmlBlock => blocks.push(self.parse_html_block(depth)?),
                        Tag::Table(_) => blocks.push(self.parse_table(depth)?),
                        unsupported => {
                            let end = unsupported.to_end();
                            let content = self.parse_visible_until(end, depth + 1)?;
                            if !content.trim().is_empty() {
                                blocks.push(Block::paragraph(
                                    self.next_block_id("fallback"),
                                    content,
                                ));
                            }
                        }
                    }
                }
                Event::Rule => {
                    self.cursor += 1;
                    blocks.push(Block::ThematicBreak {
                        id: self.next_block_id("rule"),
                    });
                }
                Event::Html(html) | Event::InlineHtml(html) => {
                    self.cursor += 1;
                    let source = sanitize_html(&html);
                    if !source.trim().is_empty() {
                        blocks.push(Block::RawHtml {
                            id: self.next_block_id("html"),
                            plain_text: raw_html_plain_text(&source)
                                .context("HTML nesting exceeds the document limit")?,
                            source,
                        });
                    }
                }
                Event::Text(text) | Event::Code(text) => {
                    self.cursor += 1;
                    blocks.push(Block::paragraph(
                        self.next_block_id("text"),
                        text.into_string(),
                    ));
                }
                Event::SoftBreak | Event::HardBreak | Event::TaskListMarker(_) => {
                    self.cursor += 1;
                }
                Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::FootnoteReference(text) => {
                    self.cursor += 1;
                    blocks.push(Block::paragraph(
                        self.next_block_id("text"),
                        text.into_string(),
                    ));
                }
            }
        }
        if expected_end.is_some() {
            bail!("Markdown 块缺少结束标记");
        }
        Ok(blocks)
    }

    fn parse_list(&mut self, start: Option<u64>, depth: usize) -> Result<Block> {
        let list_end = TagEnd::List(start.is_some());
        let mut items = Vec::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::End(end) if end == list_end => {
                    self.cursor += 1;
                    break;
                }
                Event::Start(Tag::Item) => {
                    self.cursor += 1;
                    let checked = self.task_marker_before_item_end();
                    // CommonMark omits Paragraph tags in a tight list. Treat
                    // the direct inline event stream as one semantic paragraph.
                    let blocks = if self
                        .events
                        .get(self.cursor)
                        .is_some_and(event_begins_tight_inline)
                    {
                        let content = self.parse_inlines(TagEnd::Item, depth + 1)?;
                        vec![Block::Paragraph {
                            id: self.next_block_id("paragraph"),
                            content,
                        }]
                    } else {
                        self.parse_blocks(Some(TagEnd::Item), depth + 1)?
                    };
                    items.push(ListItem { checked, blocks });
                }
                other => bail!("列表中出现了无效事件: {other:?}"),
            }
        }
        let id = self.next_block_id("list");
        Ok(match start {
            Some(start) => Block::OrderedList { id, start, items },
            None => Block::BulletList { id, items },
        })
    }

    fn task_marker_before_item_end(&self) -> Option<bool> {
        let mut depth = 0_usize;
        for event in &self.events[self.cursor..] {
            match event {
                Event::Start(Tag::Item) => depth += 1,
                Event::End(TagEnd::Item) if depth == 0 => break,
                Event::End(TagEnd::Item) => depth = depth.saturating_sub(1),
                Event::TaskListMarker(value) if depth == 0 => return Some(*value),
                _ => {}
            }
        }
        None
    }

    fn parse_code_block(&mut self, kind: CodeBlockKind<'_>) -> Result<Block> {
        let language = match kind {
            CodeBlockKind::Indented => None,
            CodeBlockKind::Fenced(value) => value
                .split_ascii_whitespace()
                .next()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        };
        let mut code = String::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::End(TagEnd::CodeBlock) => {
                    self.cursor += 1;
                    return Ok(Block::CodeBlock {
                        id: self.next_block_id("code"),
                        language,
                        code,
                    });
                }
                Event::Text(text) | Event::Code(text) => {
                    self.cursor += 1;
                    code.push_str(&text);
                }
                Event::SoftBreak | Event::HardBreak => {
                    self.cursor += 1;
                    code.push('\n');
                }
                other => bail!("代码块中出现了无效事件: {other:?}"),
            }
        }
        bail!("代码块缺少结束标记")
    }

    fn parse_html_block(&mut self, depth: usize) -> Result<Block> {
        let mut html = String::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::End(TagEnd::HtmlBlock) => {
                    self.cursor += 1;
                    let source = sanitize_html(&html);
                    let dom =
                        parse_document(RcDom::default(), Default::default()).one(source.clone());
                    let body = find_html_element(&dom.document, "body")
                        .unwrap_or_else(|| dom.document.clone());
                    ensure_html_dom_depth(&body)?;
                    let significant = body
                        .children
                        .borrow()
                        .iter()
                        .filter(|node| match &node.data {
                            NodeData::Text { contents } => !contents.borrow().trim().is_empty(),
                            NodeData::Comment { .. } => false,
                            _ => true,
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if significant.len() == 1
                        && let Some(media) =
                            html_media_block(&significant[0], self.next_block_id("media"), depth)?
                    {
                        return Ok(media);
                    }
                    return Ok(Block::RawHtml {
                        id: self.next_block_id("html"),
                        plain_text: raw_html_plain_text(&source)
                            .context("HTML nesting exceeds the document limit")?,
                        source,
                    });
                }
                Event::Html(value) | Event::InlineHtml(value) | Event::Text(value) => {
                    self.cursor += 1;
                    html.push_str(&value);
                }
                Event::SoftBreak | Event::HardBreak => {
                    self.cursor += 1;
                    html.push('\n');
                }
                other => bail!("HTML 块中出现了无效事件: {other:?}"),
            }
        }
        bail!("HTML 块缺少结束标记")
    }

    fn parse_table(&mut self, depth: usize) -> Result<Block> {
        let mut header = None;
        let mut rows = Vec::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::Start(Tag::TableHead) => {
                    self.cursor += 1;
                    header = Some(self.parse_table_head(depth)?);
                }
                Event::Start(Tag::TableRow) => {
                    self.cursor += 1;
                    let row = self.parse_table_row(depth)?;
                    rows.push(row);
                }
                Event::End(TagEnd::Table) => {
                    self.cursor += 1;
                    return Ok(Block::Table {
                        id: self.next_block_id("table"),
                        header,
                        rows,
                    });
                }
                other => bail!("表格中出现了无效事件: {other:?}"),
            }
        }
        bail!("表格缺少结束标记")
    }

    fn parse_table_head(&mut self, depth: usize) -> Result<TableRow> {
        let mut cells = Vec::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::Start(Tag::TableCell) => {
                    self.cursor += 1;
                    cells.push(TableCell::new(
                        self.parse_inlines(TagEnd::TableCell, depth + 1)?,
                    ));
                }
                Event::End(TagEnd::TableHead) => {
                    self.cursor += 1;
                    return Ok(TableRow::new(cells));
                }
                other => bail!("表头中出现了无效事件: {other:?}"),
            }
        }
        bail!("表头缺少结束标记")
    }

    fn parse_table_row(&mut self, depth: usize) -> Result<TableRow> {
        let mut cells = Vec::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::Start(Tag::TableCell) => {
                    self.cursor += 1;
                    cells.push(TableCell::new(
                        self.parse_inlines(TagEnd::TableCell, depth + 1)?,
                    ));
                }
                Event::End(TagEnd::TableRow) => {
                    self.cursor += 1;
                    return Ok(TableRow::new(cells));
                }
                other => bail!("表格行中出现了无效事件: {other:?}"),
            }
        }
        bail!("表格行缺少结束标记")
    }

    fn parse_inlines(&mut self, expected_end: TagEnd, depth: usize) -> Result<Vec<Inline>> {
        ensure_markdown_depth(depth)?;
        let mut output = Vec::new();
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::End(end) if end == expected_end => {
                    self.cursor += 1;
                    return Ok(output);
                }
                Event::End(end) => bail!("Markdown 行内结束标记不匹配: {end:?}"),
                Event::Start(Tag::Emphasis) => {
                    self.cursor += 1;
                    output.push(Inline::Emphasis {
                        content: self.parse_inlines(TagEnd::Emphasis, depth + 1)?,
                    });
                }
                Event::Start(Tag::Strong) => {
                    self.cursor += 1;
                    output.push(Inline::Strong {
                        content: self.parse_inlines(TagEnd::Strong, depth + 1)?,
                    });
                }
                Event::Start(Tag::Strikethrough) => {
                    self.cursor += 1;
                    output.push(Inline::Strikethrough {
                        content: self.parse_inlines(TagEnd::Strikethrough, depth + 1)?,
                    });
                }
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) => {
                    self.cursor += 1;
                    let content = self.parse_inlines(TagEnd::Link, depth + 1)?;
                    let href = dest_url.into_string();
                    if safe_link(&href) {
                        output.push(Inline::Link {
                            href,
                            title: non_empty(title.as_ref()),
                            content,
                        });
                    } else {
                        output.extend(content);
                    }
                }
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    self.cursor += 1;
                    let alt_nodes = self.parse_inlines(TagEnd::Image, depth + 1)?;
                    let alt = inline_text(&alt_nodes);
                    let href = dest_url.into_string();
                    if let Some(asset_id) = asset_id_from_href(&href) {
                        output.push(Inline::Image {
                            asset_id,
                            alt,
                            title: non_empty(title.as_ref()),
                        });
                    } else if safe_link(&href) {
                        let title = non_empty(title.as_ref());
                        let mut raw = format!(
                            "<img src=\"{}\" alt=\"{}\"",
                            escape_html(&href),
                            escape_html(&alt)
                        );
                        if let Some(title) = title {
                            raw.push_str(&format!(" title=\"{}\"", escape_html(&title)));
                        }
                        raw.push_str(" />");
                        let source = sanitize_html(&raw);
                        output.push(Inline::RawHtml {
                            plain_text: raw_html_plain_text(&source)
                                .context("HTML nesting exceeds the document limit")?,
                            source,
                        });
                    } else {
                        output.push(Inline::text(alt));
                    }
                }
                Event::Text(text) => {
                    self.cursor += 1;
                    output.push(Inline::text(text.into_string()));
                }
                Event::Code(text) => {
                    self.cursor += 1;
                    output.push(Inline::Code {
                        value: text.into_string(),
                    });
                }
                Event::SoftBreak => {
                    self.cursor += 1;
                    output.push(Inline::SoftBreak);
                }
                Event::HardBreak => {
                    self.cursor += 1;
                    output.push(Inline::HardBreak);
                }
                Event::TaskListMarker(_) => self.cursor += 1,
                Event::InlineHtml(html) | Event::Html(html) => {
                    self.cursor += 1;
                    let source = sanitize_html(&html);
                    if !source.is_empty() {
                        output.push(Inline::RawHtml {
                            plain_text: raw_html_plain_text(&source)
                                .context("HTML nesting exceeds the document limit")?,
                            source,
                        });
                    }
                }
                Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::FootnoteReference(text) => {
                    self.cursor += 1;
                    output.push(Inline::text(text.into_string()));
                }
                Event::Rule => {
                    self.cursor += 1;
                    output.push(Inline::text("---"));
                }
                Event::Start(other) => {
                    self.cursor += 1;
                    let content = self.parse_visible_until(other.to_end(), depth + 1)?;
                    output.push(Inline::text(content));
                }
            }
        }
        bail!("Markdown 行内内容缺少结束标记")
    }

    fn parse_visible_until(&mut self, expected_end: TagEnd, base_depth: usize) -> Result<String> {
        ensure_markdown_depth(base_depth)?;
        let mut text = String::new();
        let mut depth = 0_usize;
        while self.cursor < self.events.len() {
            match self.events[self.cursor].clone() {
                Event::Start(_) => {
                    ensure_markdown_depth(base_depth + depth + 1)?;
                    depth += 1;
                    self.cursor += 1;
                }
                Event::End(end) if depth == 0 && end == expected_end => {
                    self.cursor += 1;
                    return Ok(text);
                }
                Event::End(_) => {
                    depth = depth.saturating_sub(1);
                    self.cursor += 1;
                }
                Event::Text(value)
                | Event::Code(value)
                | Event::InlineMath(value)
                | Event::DisplayMath(value)
                | Event::FootnoteReference(value) => {
                    self.cursor += 1;
                    text.push_str(&value);
                }
                Event::SoftBreak | Event::HardBreak => {
                    self.cursor += 1;
                    text.push('\n');
                }
                Event::Html(value) | Event::InlineHtml(value) => {
                    self.cursor += 1;
                    let source = sanitize_html(&value);
                    text.push_str(
                        &raw_html_plain_text(&source)
                            .context("HTML nesting exceeds the document limit")?,
                    );
                }
                Event::Rule => {
                    self.cursor += 1;
                    text.push_str("---");
                }
                Event::TaskListMarker(checked) => {
                    self.cursor += 1;
                    text.push_str(if checked { "[x] " } else { "[ ] " });
                }
            }
        }
        bail!("Markdown 内容缺少结束标记")
    }
}

fn ensure_markdown_depth(depth: usize) -> Result<()> {
    if depth > MAX_DOCUMENT_DEPTH {
        bail!("Markdown nesting exceeds {MAX_DOCUMENT_DEPTH}");
    }
    Ok(())
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn event_begins_tight_inline(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::InlineHtml(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::TaskListMarker(_)
            | Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough)
            | Event::Start(Tag::Link { .. } | Tag::Image { .. })
    )
}

fn non_empty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn safe_link(href: &str) -> bool {
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
        || lower.starts_with("moye-asset:")
        || lower.starts_with("data:image/")
}

fn asset_id_from_href(href: &str) -> Option<String> {
    href.strip_prefix("moye-asset:")
        .or_else(|| href.strip_prefix("asset:"))
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.contains(['/', '\\', '#', '?']))
        .map(str::to_owned)
}

fn inline_text(inlines: &[Inline]) -> String {
    inlines.iter().map(Inline::plain_text).collect()
}

fn write_markdown_blocks(blocks: &[Block], indent: usize, output: &mut String) {
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 && !output.ends_with("\n\n") {
            if !output.ends_with('\n') {
                output.push('\n');
            }
            output.push('\n');
        }
        match block {
            Block::Paragraph { content, .. } => write_markdown_inlines(content, output),
            Block::Heading { level, content, .. } => {
                output.push_str(&"#".repeat((*level).clamp(1, 6) as usize));
                output.push(' ');
                write_markdown_inlines(content, output);
            }
            Block::BlockQuote { blocks, .. } => {
                let mut nested = String::new();
                write_markdown_blocks(blocks, indent, &mut nested);
                for line in nested.lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                while output.ends_with('\n') {
                    output.pop();
                }
            }
            Block::BulletList { items, .. } => write_markdown_list(items, None, indent, output),
            Block::OrderedList { start, items, .. } => {
                write_markdown_list(items, Some(*start), indent, output)
            }
            Block::CodeBlock { language, code, .. } => {
                output.push_str("```");
                if let Some(language) = language {
                    output.push_str(language);
                }
                output.push('\n');
                output.push_str(code);
                if !code.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("```");
            }
            Block::ThematicBreak { .. } => output.push_str("---"),
            Block::Table { header, rows, .. } => write_markdown_table(header, rows, output),
            Block::Image {
                asset_id,
                alt,
                title,
                caption,
                ..
            } => write_html_image(asset_id, alt, title, caption, output),
            Block::Audio {
                asset_id,
                title,
                caption,
                ..
            } => write_html_media("audio", asset_id, None, title, caption, output),
            Block::Video {
                asset_id,
                poster_asset_id,
                title,
                caption,
                ..
            } => write_html_media(
                "video",
                asset_id,
                poster_asset_id.as_deref(),
                title,
                caption,
                output,
            ),
            Block::RawHtml { source, .. } => output.push_str(&sanitize_html(source)),
        }
    }
}

fn write_markdown_list(items: &[ListItem], start: Option<u64>, indent: usize, output: &mut String) {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&" ".repeat(indent));
        match start {
            Some(start) => output.push_str(&format!("{}. ", start.saturating_add(index as u64))),
            None => output.push_str("- "),
        }
        if let Some(checked) = item.checked {
            output.push_str(if checked { "[x] " } else { "[ ] " });
        }
        let mut nested = String::new();
        write_markdown_blocks(&item.blocks, indent + 2, &mut nested);
        let mut lines = nested.lines();
        if let Some(first) = lines.next() {
            output.push_str(first);
        }
        for line in lines {
            output.push('\n');
            output.push_str(&" ".repeat(indent + 2));
            output.push_str(line);
        }
    }
}

fn write_markdown_table(header: &Option<TableRow>, rows: &[TableRow], output: &mut String) {
    let columns = header
        .iter()
        .chain(rows.iter())
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1);
    let fallback = TableRow::new((0..columns).map(|_| TableCell::text("")).collect());
    write_markdown_row(header.as_ref().unwrap_or(&fallback), columns, output);
    output.push('\n');
    output.push('|');
    for _ in 0..columns {
        output.push_str(" --- |");
    }
    for row in rows {
        output.push('\n');
        write_markdown_row(row, columns, output);
    }
}

fn write_markdown_row(row: &TableRow, columns: usize, output: &mut String) {
    output.push('|');
    for index in 0..columns {
        output.push(' ');
        if let Some(cell) = row.cells.get(index) {
            let mut value = String::new();
            write_markdown_inlines(&cell.content, &mut value);
            output.push_str(&value.replace('|', "\\|"));
        }
        output.push_str(" |");
    }
}

fn write_markdown_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { value } => output.push_str(&escape_markdown_text(value)),
            Inline::Emphasis { content } => {
                output.push('*');
                write_markdown_inlines(content, output);
                output.push('*');
            }
            Inline::Strong { content } => {
                output.push_str("**");
                write_markdown_inlines(content, output);
                output.push_str("**");
            }
            Inline::Strikethrough { content } => {
                output.push_str("~~");
                write_markdown_inlines(content, output);
                output.push_str("~~");
            }
            Inline::Code { value } => {
                let fence = if value.contains('`') { "``" } else { "`" };
                output.push_str(fence);
                output.push_str(value);
                output.push_str(fence);
            }
            Inline::Link {
                href,
                title,
                content,
            } => {
                output.push('[');
                write_markdown_inlines(content, output);
                output.push_str("](");
                output.push_str(href);
                if let Some(title) = title {
                    output.push_str(" \"");
                    output.push_str(&title.replace('"', "\\\""));
                    output.push('"');
                }
                output.push(')');
            }
            Inline::HardBreak => output.push_str("  \n"),
            Inline::SoftBreak => output.push('\n'),
            Inline::Image {
                asset_id,
                alt,
                title,
            } => {
                output.push_str("![");
                output.push_str(&escape_markdown_text(alt));
                output.push_str("](moye-asset:");
                output.push_str(asset_id);
                if let Some(title) = title {
                    output.push_str(" \"");
                    output.push_str(&title.replace('"', "\\\""));
                    output.push('"');
                }
                output.push(')');
            }
            Inline::RawHtml { source, .. } => output.push_str(&sanitize_html(source)),
        }
    }
}

fn write_html_blocks(blocks: &[Block], output: &mut String) {
    for block in blocks {
        match block {
            Block::Paragraph { content, .. } => {
                output.push_str("<p>");
                write_html_inlines(content, output);
                output.push_str("</p>");
            }
            Block::Heading { level, content, .. } => {
                let level = (*level).clamp(1, 6);
                output.push_str(&format!("<h{level}>"));
                write_html_inlines(content, output);
                output.push_str(&format!("</h{level}>"));
            }
            Block::BlockQuote { blocks, .. } => {
                output.push_str("<blockquote>");
                write_html_blocks(blocks, output);
                output.push_str("</blockquote>");
            }
            Block::BulletList { items, .. } => write_html_list("ul", items, output),
            Block::OrderedList { start, items, .. } => {
                output.push_str(&format!("<ol start=\"{start}\">"));
                write_html_list_items(items, output);
                output.push_str("</ol>");
            }
            Block::CodeBlock { language, code, .. } => {
                output.push_str("<pre><code");
                if let Some(language) = language {
                    output.push_str(" class=\"language-");
                    output.push_str(&escape_html(language));
                    output.push('"');
                }
                output.push('>');
                output.push_str(&escape_html(code));
                output.push_str("</code></pre>");
            }
            Block::ThematicBreak { .. } => output.push_str("<hr>"),
            Block::Table { header, rows, .. } => {
                output.push_str("<table>");
                if let Some(header) = header {
                    output.push_str("<thead>");
                    write_html_row(header, "th", output);
                    output.push_str("</thead>");
                }
                output.push_str("<tbody>");
                for row in rows {
                    write_html_row(row, "td", output);
                }
                output.push_str("</tbody></table>");
            }
            Block::Image {
                asset_id,
                alt,
                title,
                caption,
                ..
            } => write_html_image(asset_id, alt, title, caption, output),
            Block::Audio {
                asset_id,
                title,
                caption,
                ..
            } => write_html_media("audio", asset_id, None, title, caption, output),
            Block::Video {
                asset_id,
                poster_asset_id,
                title,
                caption,
                ..
            } => write_html_media(
                "video",
                asset_id,
                poster_asset_id.as_deref(),
                title,
                caption,
                output,
            ),
            Block::RawHtml { source, .. } => output.push_str(&sanitize_html(source)),
        }
    }
}

fn write_html_list(tag: &str, items: &[ListItem], output: &mut String) {
    output.push_str(&format!("<{tag}>"));
    write_html_list_items(items, output);
    output.push_str(&format!("</{tag}>"));
}

fn write_html_list_items(items: &[ListItem], output: &mut String) {
    for item in items {
        output.push_str("<li>");
        if let Some(checked) = item.checked {
            output.push_str(if checked {
                "<input type=\"checkbox\" checked disabled>"
            } else {
                "<input type=\"checkbox\" disabled>"
            });
        }
        write_html_blocks(&item.blocks, output);
        output.push_str("</li>");
    }
}

fn write_html_row(row: &TableRow, cell_tag: &str, output: &mut String) {
    output.push_str("<tr>");
    for cell in &row.cells {
        output.push_str(&format!("<{cell_tag}>"));
        write_html_inlines(&cell.content, output);
        output.push_str(&format!("</{cell_tag}>"));
    }
    output.push_str("</tr>");
}

fn write_html_caption(caption: &[Inline], output: &mut String) {
    if !caption.is_empty() {
        output.push_str("<figcaption>");
        write_html_inlines(caption, output);
        output.push_str("</figcaption>");
    }
}

fn write_html_image(
    asset_id: &str,
    alt: &str,
    title: &Option<String>,
    caption: &[Inline],
    output: &mut String,
) {
    output.push_str("<figure><img src=\"moye-asset:");
    output.push_str(&escape_html(asset_id));
    output.push_str("\" alt=\"");
    output.push_str(&escape_html(alt));
    output.push('"');
    if let Some(title) = title {
        output.push_str(" title=\"");
        output.push_str(&escape_html(title));
        output.push('"');
    }
    output.push('>');
    write_html_caption(caption, output);
    output.push_str("</figure>");
}

fn write_html_media(
    tag: &str,
    asset_id: &str,
    poster_asset_id: Option<&str>,
    title: &Option<String>,
    caption: &[Inline],
    output: &mut String,
) {
    output.push_str("<figure><");
    output.push_str(tag);
    output.push_str(" controls src=\"moye-asset:");
    output.push_str(&escape_html(asset_id));
    output.push('"');
    if let Some(poster) = poster_asset_id {
        output.push_str(" poster=\"moye-asset:");
        output.push_str(&escape_html(poster));
        output.push('"');
    }
    if let Some(title) = title {
        output.push_str(" title=\"");
        output.push_str(&escape_html(title));
        output.push('"');
    }
    output.push_str("></");
    output.push_str(tag);
    output.push('>');
    write_html_caption(caption, output);
    output.push_str("</figure>");
}

fn write_html_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { value } => output.push_str(&escape_html(value)),
            Inline::Emphasis { content } => {
                output.push_str("<em>");
                write_html_inlines(content, output);
                output.push_str("</em>");
            }
            Inline::Strong { content } => {
                output.push_str("<strong>");
                write_html_inlines(content, output);
                output.push_str("</strong>");
            }
            Inline::Strikethrough { content } => {
                output.push_str("<s>");
                write_html_inlines(content, output);
                output.push_str("</s>");
            }
            Inline::Code { value } => {
                output.push_str("<code>");
                output.push_str(&escape_html(value));
                output.push_str("</code>");
            }
            Inline::Link {
                href,
                title,
                content,
            } => {
                output.push_str("<a href=\"");
                output.push_str(&escape_html(href));
                output.push('"');
                if let Some(title) = title {
                    output.push_str(" title=\"");
                    output.push_str(&escape_html(title));
                    output.push('"');
                }
                output.push('>');
                write_html_inlines(content, output);
                output.push_str("</a>");
            }
            Inline::HardBreak => output.push_str("<br>"),
            Inline::SoftBreak => output.push('\n'),
            Inline::Image {
                asset_id,
                alt,
                title,
            } => {
                output.push_str("<img src=\"moye-asset:");
                output.push_str(&escape_html(asset_id));
                output.push_str("\" alt=\"");
                output.push_str(&escape_html(alt));
                output.push('"');
                if let Some(title) = title {
                    output.push_str(" title=\"");
                    output.push_str(&escape_html(title));
                    output.push('"');
                }
                output.push('>');
            }
            Inline::RawHtml { source, .. } => output.push_str(&sanitize_html(source)),
        }
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_markdown_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_gfm_round_trip_is_semantically_idempotent() {
        let source = "# 标题\n\n- [x] 完成 **加粗**\n- [ ] 待办\n\n| 名称 | 值 |\n| --- | --- |\n| A | ~~旧~~ |";
        let first = parse_source_for_unit(SourceKind::Markdown, source, "unit-1").unwrap();
        let second =
            parse_source_for_unit(SourceKind::Markdown, &first.canonical_source, "unit-1").unwrap();
        assert_eq!(first.document, second.document);
        assert!(first.canonical_source.contains("[x]"));
        assert!(first.canonical_source.contains("| 名称 | 值 |"));
    }

    #[test]
    fn markdown_gfm_autolink_literals_are_typed_and_canonical() {
        let parsed = parse_source_for_unit(
            SourceKind::Markdown,
            "访问 https://example.com/a_(b)，或 www.example.org/path。`https://code.test` [已有](https://linked.test)",
            "unit-autolink",
        )
        .unwrap();
        let Block::Paragraph { content, .. } = &parsed.document.blocks[0] else {
            panic!("expected paragraph");
        };
        let links = content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Link { href, content, .. } => Some((href.as_str(), content.as_slice())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(links.len(), 3, "parsed inline AST: {content:#?}");
        assert_eq!(links[0].0, "https://example.com/a_(b)");
        assert_eq!(links[1].0, "http://www.example.org/path");
        assert_eq!(links[2].0, "https://linked.test");
        assert!(content.iter().any(
            |inline| matches!(inline, Inline::Code { value } if value == "https://code.test")
        ));
        assert!(
            parsed
                .canonical_source
                .contains("[https://example.com/a\\_(b)](https://example.com/a_(b))")
        );

        let reparsed = parse_source_for_unit(
            SourceKind::Markdown,
            &parsed.canonical_source,
            "unit-autolink",
        )
        .unwrap();
        assert_eq!(parsed.document, reparsed.document);
    }

    #[test]
    fn html_removes_scripts_handlers_and_network_resources() {
        let parsed = parse_source_for_unit(
            SourceKind::Html,
            r#"<p onclick="steal()">正文</p><script>alert(1)</script><img src="https://evil.test/a.png"><audio controls src="asset:sound"></audio>"#,
            "unit-1",
        )
        .unwrap();
        assert!(!parsed.canonical_source.contains("script"));
        assert!(!parsed.canonical_source.contains("onclick"));
        assert!(!parsed.canonical_source.contains("https://"));
        assert!(parsed.canonical_source.contains("audio"));
        assert!(parsed.canonical_source.contains("asset:sound"));
    }

    #[test]
    fn cleaned_html_is_semantically_idempotent() {
        let source = r#"
            <h2 onclick="steal()">Heading</h2>
            <p>Body <strong>bold</strong><script>alert(1)</script></p>
            <figure><img src="moye-asset:image-1" alt="diagram"><figcaption>Caption</figcaption></figure>
            <video controls src="moye-asset:video-1" poster="moye-asset:poster-1"></video>
        "#;
        let first = parse_source_for_unit(SourceKind::Html, source, "unit-html").unwrap();
        let second =
            parse_source_for_unit(SourceKind::Html, &first.canonical_source, "unit-html").unwrap();

        assert_eq!(first.document, second.document);
        assert_eq!(first.canonical_source, second.canonical_source);
        assert!(!first.canonical_source.contains("onclick"));
        assert!(!first.canonical_source.contains("script"));
    }

    #[test]
    fn html_nesting_is_bounded_before_recursive_ast_conversion() {
        fn nested_emphasis(depth: usize) -> String {
            format!(
                "<p>{}正文{}</p>",
                "<em>".repeat(depth),
                "</em>".repeat(depth)
            )
        }

        parse_source_for_unit(
            SourceKind::Html,
            &nested_emphasis(MAX_DOCUMENT_DEPTH - 1),
            "unit-at-depth-limit",
        )
        .expect("a visible HTML leaf at the shared depth limit remains accepted");

        let error = parse_source_for_unit(
            SourceKind::Html,
            &nested_emphasis(4_096),
            "unit-over-depth-limit",
        )
        .expect_err("extreme HTML nesting must return an error instead of recursing deeply");
        assert!(error.to_string().contains("HTML nesting exceeds"));
    }

    #[test]
    fn markdown_block_nesting_is_bounded_during_ast_conversion() {
        fn nested_quote(depth: usize) -> String {
            format!("{}正文\n", "> ".repeat(depth))
        }

        parse_source_for_unit(
            SourceKind::Markdown,
            &nested_quote(MAX_DOCUMENT_DEPTH - 1),
            "unit-at-depth-limit",
        )
        .expect("a visible Markdown leaf at the shared depth limit remains accepted");

        let error = parse_source_for_unit(
            SourceKind::Markdown,
            &nested_quote(4_096),
            "unit-over-depth-limit",
        )
        .expect_err("extreme Markdown block nesting must stop during AST conversion");
        assert!(error.to_string().contains("Markdown nesting exceeds"));
    }

    #[test]
    fn markdown_inline_event_nesting_is_bounded_during_ast_conversion() {
        fn nested_emphasis_events(depth: usize) -> Vec<Event<'static>> {
            let mut events = Vec::with_capacity(depth.saturating_mul(2).saturating_add(2));
            events.extend((0..depth).map(|_| Event::Start(Tag::Emphasis)));
            events.push(Event::Text("正文".into()));
            events.extend((0..depth).map(|_| Event::End(TagEnd::Emphasis)));
            events.push(Event::End(TagEnd::Paragraph));
            events
        }

        let mut parser = MarkdownAstParser {
            events: nested_emphasis_events(MAX_DOCUMENT_DEPTH - 1),
            cursor: 0,
            unit_id: "unit-at-depth-limit",
            block_index: 0,
        };
        let inlines = parser
            .parse_inlines(TagEnd::Paragraph, 1)
            .expect("an inline leaf at the shared depth limit remains accepted");
        let document = BlockDocument::new(vec![Block::Paragraph {
            id: "paragraph-at-depth-limit".to_string(),
            content: inlines,
        }]);
        document.validate().expect("boundary inline AST is valid");

        let mut parser = MarkdownAstParser {
            events: nested_emphasis_events(4_096),
            cursor: 0,
            unit_id: "unit-over-depth-limit",
            block_index: 0,
        };
        let error = parser
            .parse_inlines(TagEnd::Paragraph, 1)
            .expect_err("extreme inline nesting must stop during AST conversion");
        assert!(error.to_string().contains("Markdown nesting exceeds"));
    }

    #[test]
    fn unsafe_markdown_link_loses_its_navigation_but_keeps_text() {
        let parsed = parse_source_for_unit(
            SourceKind::Markdown,
            "[保留文字](javascript:alert(1))",
            "unit-1",
        )
        .unwrap();
        assert_eq!(parsed.canonical_source, "保留文字");
        assert!(!parsed.canonical_source.contains("javascript"));
    }

    #[test]
    fn preserved_markdown_image_keeps_its_inert_alt_projection() {
        let parsed = parse_source_for_unit(
            SourceKind::Markdown,
            "![替代文字](images/figure.png)",
            "unit-1",
        )
        .expect("a safe relative image remains representable as inert raw HTML");
        assert!(parsed.document.plain_text().contains("替代文字"));
    }

    #[test]
    fn preserved_markdown_image_canonicalizes_alt_whitespace() {
        let parsed = parse_source_for_unit(
            SourceKind::Markdown,
            "![替代   文字  \n第二行](images/figure.png)",
            "unit-1",
        )
        .expect("RawHtml image alt text must use the canonical inert projection");

        assert_eq!(parsed.document.plain_text(), "替代 文字 第二行");
        parsed
            .document
            .validate()
            .expect("the canonical RawHtml projection remains valid");
    }

    #[test]
    fn media_blocks_serialize_with_opaque_asset_urls() {
        let document = BlockDocument::new(vec![Block::Video {
            id: "video-1".into(),
            asset_id: "video-asset".into(),
            poster_asset_id: Some("poster-asset".into()),
            title: Some("演示".into()),
            caption: vec![Inline::text("说明")],
        }]);
        let markdown = serialize_source(&document, SourceKind::Markdown).unwrap();
        assert!(markdown.contains("moye-asset:video-asset"));
        assert!(markdown.contains("moye-asset:poster-asset"));
        assert!(!markdown.contains("blake3/"));
    }

    #[test]
    fn block_media_metadata_survives_markdown_round_trips() {
        let document = BlockDocument::new(vec![
            Block::Image {
                id: "image".into(),
                asset_id: "image-asset".into(),
                alt: "图片说明".into(),
                title: Some("图片标题".into()),
                caption: vec![Inline::text("图片说明")],
            },
            Block::Audio {
                id: "audio".into(),
                asset_id: "audio-asset".into(),
                title: Some("音频标题".into()),
                caption: vec![Inline::text("音频说明")],
            },
            Block::Video {
                id: "video".into(),
                asset_id: "video-asset".into(),
                poster_asset_id: Some("poster-asset".into()),
                title: Some("视频标题".into()),
                caption: vec![Inline::text("视频说明")],
            },
        ]);
        let source = serialize_source(&document, SourceKind::Markdown).unwrap();
        assert!(source.contains("<figcaption>图片说明</figcaption>"));
        assert!(source.contains("<figcaption>音频说明</figcaption>"));
        assert!(source.contains("<figcaption>视频说明</figcaption>"));

        let first = parse_source_for_unit(SourceKind::Markdown, &source, "unit-media").unwrap();
        let second =
            parse_source_for_unit(SourceKind::Markdown, &first.canonical_source, "unit-media")
                .unwrap();
        assert_eq!(first.document, second.document);
        assert_eq!(
            first.document.referenced_asset_ids(),
            vec!["image-asset", "audio-asset", "video-asset", "poster-asset"]
        );
        let plain_text = first.document.plain_text();
        assert!(plain_text.contains("图片说明"));
        assert!(plain_text.contains("音频说明"));
        assert!(plain_text.contains("视频说明"));
    }
}
