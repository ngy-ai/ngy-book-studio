//! Parsing and canonical serialization for editable HTML sources.
//!
//! A source edit is not publishable until it passes through this module.  The
//! returned source is normalized and every raw HTML fragment has been cleaned;
//! callers persist it together with the block AST as one revision.

use std::{borrow::Cow, collections::HashSet};

use anyhow::{Context as _, Result, bail};
use html5ever::{
    QualName, parse_document, parse_fragment,
    serialize::{SerializeOpts, TraversalScope, serialize},
    tendril::TendrilSink as _,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};

use crate::document::{
    Block, BlockDocument, Inline, ListItem, MAX_DOCUMENT_DEPTH, TableCell, TableRow,
    deterministic_id, raw_html_plain_text,
};
use crate::translation::{TranslationSource, has_visible_text, normalize_source_text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub canonical_source: String,
    pub document: BlockDocument,
}

/// Parses, cleans and normalizes an editable source using a content-derived ID
/// seed. Use [`parse_source_for_unit`] when a stable unit ID is available.
pub fn parse_source(source: &str) -> Result<ParsedSource> {
    let seed = deterministic_id("source", source.as_bytes());
    parse_source_for_unit(source, &seed)
}

/// Parses, cleans and normalizes an editable source.  `unit_id` is used only
/// for deterministic block IDs and never appears in the serialized source.
pub fn parse_source_for_unit(source: &str, unit_id: &str) -> Result<ParsedSource> {
    if source.contains('\0') {
        bail!("正文不能包含 NUL 字符");
    }
    let document = parse_html(source, unit_id)?;
    document.validate().context("正文块结构无效")?;
    let canonical_source = serialize_source(&document)?;
    Ok(ParsedSource {
        canonical_source,
        document,
    })
}

pub fn serialize_source(document: &BlockDocument) -> Result<String> {
    document.validate().context("无法序列化无效的正文块结构")?;
    let mut output = String::new();
    write_html_blocks(&document.blocks, &mut output);
    Ok(output.trim().to_string())
}

/// Produces a display fragment to embed in an XHTML body. Stored HTML remains
/// HTML: normalize its already-cleaned DOM only at the XML display boundary,
/// including documents persisted before this projection was introduced.
pub fn serialize_xhtml(document: &BlockDocument) -> Result<String> {
    let html = serialize_source(document)?;
    ensure_xml_characters(&html)?;
    let dom = parse_fragment(
        RcDom::default(),
        Default::default(),
        QualName::new(None, XHTML_NAMESPACE.into(), "body".into()),
        Vec::new(),
    )
    .one(html);
    let root = find_html_element(&dom.document, "html").context("无法生成 XHTML 正文片段")?;
    ensure_html_dom_depth(&root)?;
    let mut output = String::new();
    for child in root.children.borrow().iter() {
        write_xhtml_node(child, 0, &mut output)?;
    }
    Ok(output)
}

/// Block-level element names whose visible text forms one translatable run.
/// This is the same candidate set the reader's translation layer matches, so
/// the persisted译文 and the rendered chapter stay aligned.
const TRANSLATABLE_BLOCK_TAGS: [&str; 11] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "blockquote",
    "td",
    "th",
];
const SKIPPED_TEXT_TAGS: [&str; 4] = ["script", "style", "noscript", "template"];

/// Extracts the visible text of every innermost block-level element of an HTML
/// fragment, in document order.
///
/// EPUB chapters are persisted as one preserved `RawHtml` subtree per unit, so
/// their paragraph structure only exists in the HTML source. Returning one
/// whitespace-normalized entry per element lets the translation job and the
/// reader's text-matching layer agree without re-parsing the AST, and keeps
/// nested containers (a list item wrapping a paragraph) from being translated
/// twice.
pub fn block_texts_from_html(source: &str) -> Result<Vec<String>> {
    Ok(translation_blocks_from_html(source)?
        .into_iter()
        .map(|block| block.text)
        .collect())
}

/// Extracts text leaves without losing the boundaries of original formatting.
/// The model translates only these leaves; the reader retains the source DOM.
/// Code is included in the matching text but never becomes a translation slot.
pub fn translation_blocks_from_html(source: &str) -> Result<Vec<TranslationSource>> {
    if source.contains('\0') {
        bail!("正文不能包含 NUL 字符");
    }
    let dom = parse_fragment(
        RcDom::default(),
        Default::default(),
        QualName::new(None, XHTML_NAMESPACE.into(), "body".into()),
        Vec::new(),
    )
    .one(source);
    let mut output = Vec::new();
    collect_translation_sources(&dom.document, 0, &mut output)?;
    Ok(output)
}

/// Extracts from the exact full document served by the EPUB reader. Metadata in
/// the head is never considered part of a chapter's translation or slot indices.
pub fn translation_blocks_from_document_html(source: &str) -> Result<Vec<TranslationSource>> {
    if source.contains('\0') {
        bail!("正文不能包含 NUL 字符");
    }
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    let body = find_html_element(&dom.document, "body").context("阅读章节没有正文元素")?;
    let mut output = Vec::new();
    collect_translation_sources(&body, 0, &mut output)?;
    Ok(output)
}

fn is_translatable_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if TRANSLATABLE_BLOCK_TAGS.contains(&name.local.as_ref()))
}

fn is_skipped_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if SKIPPED_TEXT_TAGS.contains(&name.local.as_ref()))
}

fn is_code_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if matches!(name.local.as_ref(), "pre" | "code"))
}

fn is_line_break_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if name.local.as_ref() == "br")
}

/// Line endings only a statement ends a line with. Full-width CJK punctuation is
/// deliberately absent: `；`/`：` end prose or quotes, never code.
const CODE_LINE_ENDINGS: [char; 3] = [';', '{', '}'];
/// Comment and preprocessor markers only a code line starts with.
const CODE_LINE_PREFIXES: [&str; 8] = [
    "//", "/*", "*/", "#!", "#include", "#define", "#pragma", "<!--",
];
/// Operators prose does not contain. A bare `=` is excluded: formulas and prose
/// use it too.
const CODE_OPERATORS: [&str; 16] = [
    "=>", "->", "::", ":=", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "</", "/>",
];

/// ECMAScript whitespace plus the invisible format characters EPUBs use to indent
/// code lines, which `trim` alone would keep in front of `//` or `#`.
fn trim_code_line(value: &str) -> &str {
    value.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '\u{200b}' | '\u{feff}' | '\u{00ad}')
    })
}

fn is_code_line(line: &str) -> bool {
    let trimmed = trim_code_line(line);
    if trimmed.is_empty() {
        return false;
    }
    trimmed.ends_with(CODE_LINE_ENDINGS)
        // A whole line of markup is source, not prose.
        || (trimmed.starts_with('<') && trimmed.ends_with('>'))
        || CODE_LINE_PREFIXES
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        || CODE_OPERATORS
            .iter()
            .any(|operator| trimmed.contains(operator))
}

/// Whether one whole block is source code rather than prose.
///
/// Books that mark code up with `pre`/`code` never reach this: those subtrees are
/// skipped before any block is formed. Conversions that keep every code line in an
/// ordinary paragraph (Calibre/Word exports, and the ZWSP-indented lines of the
/// acceptance EPUB) leave only the text shape, so a block counts as code when one
/// of its own lines ends with `;`/`{`/`}`, starts with a comment marker, or
/// contains a code operator. Prose never matches; a false negative only translates
/// a code line as before, and a false positive keeps that block in its original
/// language. `translations.js` implements the same rule and the same tables.
fn looks_like_source_code(lines: &str) -> bool {
    lines.lines().any(is_code_line)
}

fn collect_translation_sources(
    node: &Handle,
    depth: usize,
    output: &mut Vec<TranslationSource>,
) -> Result<()> {
    ensure_html_depth(depth)?;
    if is_skipped_element(node) || is_code_element(node) {
        return Ok(());
    }
    if is_translatable_element(node) && !has_translatable_descendant(node, depth)? {
        let mut text = String::new();
        let mut segments = Vec::new();
        let mut lines = String::new();
        collect_translation_leaves(node, depth, false, &mut text, &mut segments, &mut lines)?;
        // Code never becomes a translation block: the task must not send it, and
        // the reader must not look for a translation of it.
        if !segments.is_empty() && !looks_like_source_code(&lines) {
            output.push(TranslationSource {
                text: normalize_source_text(&text),
                segments,
            });
        }
        return Ok(());
    }
    for child in node.children.borrow().iter() {
        let child_depth = if matches!(&child.data, NodeData::Element { .. }) {
            depth + 1
        } else {
            depth
        };
        collect_translation_sources(child, child_depth, output)?;
    }
    Ok(())
}

fn collect_translation_leaves(
    node: &Handle,
    depth: usize,
    within_code: bool,
    text: &mut String,
    segments: &mut Vec<String>,
    lines: &mut String,
) -> Result<()> {
    ensure_html_depth(depth)?;
    if is_skipped_element(node) {
        return Ok(());
    }
    let within_code = within_code || is_code_element(node);
    if let NodeData::Text { contents } = &node.data {
        let value = contents.borrow();
        text.push_str(&value);
        if !within_code && has_visible_text(&value) {
            segments.push(value.to_string());
            // Line breaks only exist as `<br>` in the source, so the shape text
            // keeps them; the matching text stays exactly what the page sees.
            lines.push_str(&value);
        }
    }
    // Match DOM textContent: <br> has no text, while its original node remains
    // responsible for rendering the line break in the translated projection.
    for child in node.children.borrow().iter() {
        if !within_code && is_line_break_element(child) {
            lines.push('\n');
        }
        let child_depth = if matches!(&child.data, NodeData::Element { .. }) {
            depth + 1
        } else {
            depth
        };
        collect_translation_leaves(child, child_depth, within_code, text, segments, lines)?;
    }
    Ok(())
}

fn has_translatable_descendant(node: &Handle, depth: usize) -> Result<bool> {
    fn scan(node: &Handle, depth: usize) -> Result<bool> {
        ensure_html_depth(depth)?;
        for child in node.children.borrow().iter() {
            if is_skipped_element(child) || is_code_element(child) {
                continue;
            }
            if is_translatable_element(child) {
                return Ok(true);
            }
            let child_depth = if matches!(&child.data, NodeData::Element { .. }) {
                depth + 1
            } else {
                depth
            };
            if scan(child, child_depth)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    scan(node, depth)
}

const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

fn ensure_xml_characters(value: &str) -> Result<()> {
    if let Some(character) = value.chars().find(|character| {
        !matches!(*character, '\u{9}' | '\u{a}' | '\u{d}')
            && !matches!(*character as u32, 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
    }) {
        bail!(
            "正文包含 XHTML 不允许的字符 U+{:04X}，请在源码模式中删除或替换该字符",
            character as u32
        );
    }
    Ok(())
}

fn write_xhtml_value(value: &str, attribute: bool, output: &mut String) -> Result<()> {
    ensure_xml_characters(value)?;
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' if attribute => output.push_str("&quot;"),
            '\r' => output.push_str("&#13;"),
            '\n' if attribute => output.push_str("&#10;"),
            '\t' if attribute => output.push_str("&#9;"),
            _ => output.push(character),
        }
    }
    Ok(())
}

fn ensure_xhtml_local_name(name: &str) -> Result<()> {
    let mut characters = name.chars();
    let valid = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        });
    if !valid {
        bail!("正文包含无法安全转换为 XHTML 的元素或属性名称");
    }
    Ok(())
}

fn write_xhtml_node(node: &Handle, depth: usize, output: &mut String) -> Result<()> {
    match &node.data {
        NodeData::Text { contents } => write_xhtml_value(&contents.borrow(), false, output)?,
        NodeData::Element { name, attrs, .. } => {
            ensure_html_depth(depth)?;
            if name.ns.as_ref() != XHTML_NAMESPACE {
                bail!("正文包含无法安全转换为 XHTML 的元素命名空间");
            }
            let tag = name.local.as_ref();
            ensure_xhtml_local_name(tag)?;
            output.push('<');
            output.push_str(tag);
            for attribute in attrs.borrow().iter() {
                ensure_xhtml_local_name(attribute.name.local.as_ref())?;
                let prefix = match attribute.name.ns.as_ref() {
                    "" => "",
                    XML_NAMESPACE => "xml:",
                    _ => bail!("正文包含无法安全转换为 XHTML 的属性命名空间"),
                };
                output.push(' ');
                output.push_str(prefix);
                output.push_str(attribute.name.local.as_ref());
                output.push_str("=\"");
                write_xhtml_value(&attribute.value, true, output)?;
                output.push('"');
            }
            if matches!(
                tag,
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            ) {
                output.push_str("/>");
            } else {
                output.push('>');
                for child in node.children.borrow().iter() {
                    let child_depth = if matches!(&child.data, NodeData::Element { .. }) {
                        depth + 1
                    } else {
                        depth
                    };
                    write_xhtml_node(child, child_depth, output)?;
                }
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
        }
        // Comments and document declarations are not editable body content.
        NodeData::Comment { .. }
        | NodeData::Doctype { .. }
        | NodeData::ProcessingInstruction { .. } => {}
        NodeData::Document => bail!("XHTML 正文片段包含意外的文档根节点"),
    }
    Ok(())
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
    let blocks = parser.children(&body, 0)?;
    Ok(BlockDocument::new(blocks))
}

struct HtmlAstParser<'a> {
    unit_id: &'a str,
    block_index: usize,
}

impl HtmlAstParser<'_> {
    fn children(&mut self, node: &Handle, depth: usize) -> Result<Vec<Block>> {
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

    fn flush_inlines(&mut self, inlines: &mut Vec<Inline>, blocks: &mut Vec<Block>) {
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

fn has_html_tag(node: &Handle, tag: &str) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == tag)
}

fn has_html_attribute(node: &Handle, attribute: &str) -> bool {
    matches!(&node.data, NodeData::Element { attrs, .. } if attrs.borrow().iter().any(|item| item.name.local.as_ref() == attribute))
}

fn html_task_marker(item: &Handle) -> Option<bool> {
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

fn html_table(node: &Handle, depth: usize) -> Result<Option<(Option<TableRow>, Vec<TableRow>)>> {
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
    let mut output = Vec::new();
    for child in node.children.borrow().iter() {
        collect_html_inline(child, &mut output, depth + 1)?;
    }
    coalesce_inline_text(&mut output);
    Ok(output)
}

fn coalesce_inline_text(inlines: &mut Vec<Inline>) {
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

fn collect_html_inline(node: &Handle, output: &mut Vec<Inline>, depth: usize) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn xhtml_body(fragment: &str) -> String {
        format!("<body xmlns=\"{XHTML_NAMESPACE}\">{fragment}</body>")
    }

    #[test]
    fn block_texts_extract_innermost_blocks_and_ignore_hidden_text() {
        let source = r#"<div id="sbo-rt-content">
            <h1>Rust Brain Teasers</h1>
            <script>ignored()</script>
            <p>Copyright 2022
               second line</p>
            <ul><li><p>Nested item</p></li></ul>
            <table><tbody><tr><td>单元格</td></tr></tbody></table>
        </div>"#;
        assert_eq!(
            block_texts_from_html(source).unwrap(),
            vec![
                "Rust Brain Teasers",
                "Copyright 2022 second line",
                "Nested item",
                "单元格"
            ],
        );
        // An image-only chapter has nothing to translate.
        assert!(
            block_texts_from_html(r#"<div id="Cover"><img src="moye-asset:cover"></div>"#)
                .unwrap()
                .is_empty()
        );
        assert!(block_texts_from_html("a\0b").is_err());
    }

    #[test]
    fn translation_sources_keep_formatting_slots_breaks_and_code_context() {
        let source = "<h2>Chapter <em>one</em></h2>\
            <p>Read <strong>very <em>carefully</em></strong>: \
            <code>call(&quot;x&quot;)</code><br>Next <sup>2</sup>\u{a0}line.</p>\
            <p><code>untranslated()</code></p>\
            <pre><code>never translate this</code></pre>";
        let blocks = translation_blocks_from_html(source).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "Chapter one");
        assert_eq!(blocks[0].segments, ["Chapter ", "one"]);
        assert_eq!(
            blocks[1].text,
            "Read very carefully: call(\"x\")Next 2 line."
        );
        assert_eq!(
            blocks[1].segments,
            [
                "Read ",
                "very ",
                "carefully",
                ": ",
                "Next ",
                "2",
                "\u{a0}line."
            ]
        );
    }

    #[test]
    fn translation_sources_exclude_inert_descendants_and_blank_leaves() {
        let source = "<p>Alpha<script>secret()</script><style>ignored{}</style>\
            <noscript>fallback</noscript><template><p>inert</p></template>\
            <strong>Beta</strong><span>\u{feff}\u{a0}</span>Gamma</p>\
            <p>\u{feff}\u{a0}</p>";
        let blocks = translation_blocks_from_html(source).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "AlphaBeta Gamma");
        assert_eq!(blocks[0].segments, ["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn translation_sources_skip_leaves_that_are_only_invisible_format_characters() {
        // 现场回归：代码行用 ZWSP 缩进，ZWSP 不属于 ECMAScript `\s`，旧过滤把它当成
        // 翻译槽；模型只能回空白，整块被 `empty_segment_text` 拒绝并停在 96 号块。
        // 该块本身是代码，现在整块不再进入翻译（见 code_blocks_are_never_translated）。
        let code_line = "<p><span>\u{200b}\u{200b}</span><span>const</span>\
            <span> THREE_AND_A_BIT : f32 = 3.4028236;</span></p>";
        assert!(translation_blocks_from_html(code_line).unwrap().is_empty());
        // 正文块保留同一保护：只由不可见格式字符组成的叶子不成为翻译槽，而不可见字符
        // 仍留在匹配文本里（读者侧靠这段文本定位块级元素），所以只能排除翻译槽。
        let prose = "<p><span>\u{200b}\u{200b}</span><span>甲</span><span> 乙</span></p>";
        let blocks = translation_blocks_from_html(prose).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "\u{200b}\u{200b}甲 乙");
        assert_eq!(blocks[0].segments, ["甲", " 乙"]);
    }

    #[test]
    fn code_blocks_are_never_translated() {
        // Code that is not marked up with pre/code is still code: Calibre/Word
        // exports and the acceptance EPUB keep every code line in its own `<p>`.
        for source in [
            "<p>const THREE_AND_A_BIT : f32 = 3.4028236;</p>",
            "<p>fn main() {</p><p>}</p>",
            "<p>let a = 1<br>return a;</p>",
            "<p>// 注释行</p>",
            "<p>#include &lt;stdio.h&gt;</p>",
            "<p>count =&gt; count + 1</p>",
            "<p>total += 1</p>",
            "<p>&lt;div class=\"code\"&gt;</p>",
        ] {
            assert!(
                translation_blocks_from_html(source).unwrap().is_empty(),
                "{source} must not be translated"
            );
        }
        // Prose stays translatable, including the punctuation a code line carries
        // and inline code that merely appears inside a sentence.
        let prose = "<p>见上文（注 1）。</p><p>A note (see above)</p>\
            <p>Read <code>x = 1;</code> now</p><p>https://example.test/a/b</p>\
            <p>第一章：起步</p><p>a &lt; b 的关系</p>";
        let blocks = translation_blocks_from_html(prose).unwrap();
        assert_eq!(blocks.len(), 6);
        assert_eq!(blocks[2].segments, ["Read ", " now"]);
        assert_eq!(blocks[5].segments, ["a < b 的关系"]);
    }

    #[test]
    fn translation_sources_preserve_repeated_blocks_and_nested_cell_order() {
        let source = "<ul><li><p>Same <b>text</b></p></li><li>Same <b>text</b></li></ul>\
            <blockquote><p>Quote</p></blockquote>\
            <table><tr><th>Title</th><td><p>Cell <i>value</i></p></td></tr></table>";
        let blocks = translation_blocks_from_html(source).unwrap();
        assert_eq!(
            blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            ["Same text", "Same text", "Quote", "Title", "Cell value"]
        );
        assert_eq!(blocks[0].segments, blocks[1].segments);
        assert_eq!(blocks[4].segments, ["Cell ", "value"]);
    }

    #[test]
    fn translation_sources_reject_invalid_or_excessively_deep_source() {
        assert!(translation_blocks_from_html("<p>bad\0source</p>").is_err());
        let source = format!(
            "<p>{}text{}</p>",
            "<span>".repeat(MAX_DOCUMENT_DEPTH + 1),
            "</span>".repeat(MAX_DOCUMENT_DEPTH + 1)
        );
        assert!(translation_blocks_from_html(&source).is_err());
    }

    #[test]
    fn reader_document_translation_slots_follow_original_body_without_ast_rewriting() {
        let source = r#"<!doctype html><html><head><title>Metadata</title>
            <script>ignore()</script></head><body>
            <p>Read <a href="https://example.test">this</a> now</p>
            <p><span>One</span><span>two</span></p>
            <ul><li>First <em>item</em></li></ul></body></html>"#;
        let blocks = translation_blocks_from_document_html(source).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].segments, ["Read ", "this", " now"]);
        assert_eq!(blocks[1].text, "Onetwo");
        assert_eq!(blocks[1].segments, ["One", "two"]);
        assert_eq!(blocks[2].segments, ["First ", "item"]);
    }

    #[test]
    fn xhtml_projection_preserves_controlled_media_and_task_attributes() {
        let document = BlockDocument::new(vec![
            Block::Image {
                id: "image".into(),
                asset_id: "image-asset".into(),
                alt: "图示 <A> & B".into(),
                title: Some("带\"引号\"".into()),
                caption: vec![Inline::text("图注")],
            },
            Block::Paragraph {
                id: "paragraph".into(),
                content: vec![
                    Inline::text("第一行"),
                    Inline::HardBreak,
                    Inline::Image {
                        asset_id: "inline-image".into(),
                        alt: "行内图".into(),
                        title: None,
                    },
                ],
            },
            Block::ThematicBreak { id: "rule".into() },
            Block::BulletList {
                id: "tasks".into(),
                items: vec![
                    ListItem::task(true, vec![Block::paragraph("done", "已完成")]),
                    ListItem::task(false, vec![Block::paragraph("todo", "待办")]),
                ],
            },
            Block::Audio {
                id: "audio".into(),
                asset_id: "audio-asset".into(),
                title: None,
                caption: Vec::new(),
            },
            Block::Video {
                id: "video".into(),
                asset_id: "video-asset".into(),
                poster_asset_id: Some("poster-asset".into()),
                title: None,
                caption: Vec::new(),
            },
        ]);
        let original_html = serialize_source(&document).unwrap();
        let fragment = serialize_xhtml(&document).unwrap();
        let body = xhtml_body(&fragment);
        let xml = resvg::usvg::roxmltree::Document::parse(&body).expect("strict XHTML");
        let images = xml
            .descendants()
            .filter(|node| node.has_tag_name((XHTML_NAMESPACE, "img")))
            .collect::<Vec<_>>();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].attribute("alt"), Some("图示 <A> & B"));
        assert_eq!(images[0].attribute("title"), Some("带\"引号\""));
        assert_eq!(images[1].attribute("src"), Some("moye-asset:inline-image"));
        for tag in ["br", "hr"] {
            assert!(xml.descendants().any(|node| node.has_tag_name(tag)));
        }
        let tasks = xml
            .descendants()
            .filter(|node| node.has_tag_name("input"))
            .collect::<Vec<_>>();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].attribute("checked"), Some(""));
        assert_eq!(tasks[1].attribute("checked"), None);
        assert!(tasks.iter().all(|node| node.has_attribute("disabled")));
        for tag in ["audio", "video"] {
            let media = xml
                .descendants()
                .find(|node| node.has_tag_name(tag))
                .unwrap();
            assert_eq!(media.attribute("controls"), Some(""));
        }
        let video = xml
            .descendants()
            .find(|node| node.has_tag_name("video"))
            .unwrap();
        assert_eq!(video.attribute("poster"), Some("moye-asset:poster-asset"));
        assert_eq!(serialize_source(&document).unwrap(), original_html);
    }

    #[test]
    fn xhtml_projection_preserves_cleaned_raw_html_entities_and_quoted_attributes() {
        let raw = r#"<div title="A &quot;B&quot; &lt;tag> &amp; C">A&nbsp;B &amp; C<picture><source src="moye-asset:source"><img src="moye-asset:raw-image" alt="raw"></picture><hr><script>unsafe()</script></div>"#;
        let inline = r#"<span title="x < y &amp; &quot;q&quot;">inline&nbsp;value<br></span>"#;
        let document = BlockDocument::new(vec![
            Block::RawHtml {
                id: "raw".into(),
                source: raw.into(),
                plain_text: raw_html_plain_text(raw).unwrap(),
            },
            Block::Paragraph {
                id: "inline".into(),
                content: vec![Inline::RawHtml {
                    source: inline.into(),
                    plain_text: raw_html_plain_text(inline).unwrap(),
                }],
            },
        ]);
        let body = xhtml_body(&serialize_xhtml(&document).unwrap());
        let xml = resvg::usvg::roxmltree::Document::parse(&body).expect("strict XHTML");
        let div = xml
            .descendants()
            .find(|node| node.has_tag_name("div"))
            .unwrap();
        assert_eq!(div.attribute("title"), Some("A \"B\" <tag> & C"));
        assert_eq!(div.text(), Some("A\u{a0}B & C"));
        let span = xml
            .descendants()
            .find(|node| node.has_tag_name("span"))
            .unwrap();
        assert_eq!(span.attribute("title"), Some("x < y & \"q\""));
        assert_eq!(span.text(), Some("inline\u{a0}value"));
        let source = xml
            .descendants()
            .find(|node| node.has_tag_name("source"))
            .unwrap();
        assert_eq!(source.attribute("src"), Some("moye-asset:source"));
        assert!(xml.descendants().any(|node| node.has_tag_name("img")));
        assert!(xml.descendants().any(|node| node.has_tag_name("br")));
        assert!(xml.descendants().any(|node| node.has_tag_name("hr")));
        assert!(!xml.descendants().any(|node| node.has_tag_name("script")));
        assert!(!body.contains("unsafe()"));
    }

    #[test]
    fn xhtml_projection_keeps_text_markup_literal() {
        let text = r#"显示 <img src="https://example.invalid/a"> & &#160; >"#;
        let document = BlockDocument::new(vec![Block::paragraph("text", text)]);
        let body = xhtml_body(&serialize_xhtml(&document).unwrap());
        let xml = resvg::usvg::roxmltree::Document::parse(&body).expect("strict XHTML");
        let paragraph = xml
            .descendants()
            .find(|node| node.has_tag_name("p"))
            .unwrap();
        assert_eq!(paragraph.text(), Some(text));
        assert!(!xml.descendants().any(|node| node.has_tag_name("img")));
    }

    #[test]
    fn xhtml_projection_rejects_xml_forbidden_characters() {
        for character in ['\u{1}', '\u{b}', '\u{fffe}', '\u{ffff}'] {
            let document = BlockDocument::new(vec![Block::paragraph(
                "invalid-text",
                format!("前{character}后"),
            )]);
            let error = serialize_xhtml(&document).expect_err("XML must reject this character");
            assert!(
                error
                    .to_string()
                    .contains(&format!("U+{:04X}", character as u32))
            );
        }
    }

    #[test]
    fn xhtml_projection_obeys_the_existing_html_depth_limit() {
        let source = format!(
            "{}正文{}",
            "<div>".repeat(MAX_DOCUMENT_DEPTH),
            "</div>".repeat(MAX_DOCUMENT_DEPTH)
        );
        let document = BlockDocument::new(vec![Block::RawHtml {
            id: "nested".into(),
            plain_text: raw_html_plain_text(&source).unwrap(),
            source,
        }]);
        let body = xhtml_body(&serialize_xhtml(&document).unwrap());
        resvg::usvg::roxmltree::Document::parse(&body).expect("bounded XHTML remains valid");

        let mut combined = document;
        for index in 0..3 {
            combined = BlockDocument::new(vec![Block::BlockQuote {
                id: format!("quote-{index}"),
                blocks: combined.blocks,
            }]);
        }
        assert!(serialize_xhtml(&combined).is_err());
    }

    #[test]
    fn html_structured_blocks_round_trip_with_typed_semantics() {
        let source = r#"<h1>标题</h1><blockquote><p>说明 <em>内容</em></p></blockquote><ol start="3"><li>第一项 <strong>加粗</strong><ul><li><input type="checkbox" checked>已完成</li><li><input type="checkbox">待办</li></ul></li></ol><pre><code class="language-rust">a &lt; b
line 2</code></pre><table><thead><tr><th>名称</th><th>值</th></tr></thead><tbody><tr><td><p>A</p><p>第二行</p></td><td><del>旧</del></td></tr></tbody></table>"#;
        let parsed = parse_source_for_unit(source, "structured").unwrap();
        let reparsed = parse_source_for_unit(&parsed.canonical_source, "structured").unwrap();
        assert_eq!(parsed.document, reparsed.document);
        assert_eq!(parsed.canonical_source, reparsed.canonical_source);
        assert!(matches!(
            &parsed.document.blocks[1],
            Block::BlockQuote { .. }
        ));
        let Block::OrderedList { start, items, .. } = &parsed.document.blocks[2] else {
            panic!("expected an ordered list");
        };
        assert_eq!(*start, 3);
        let Block::BulletList { items, .. } = &items[0].blocks[1] else {
            panic!("expected a nested task list");
        };
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[1].checked, Some(false));
        assert!(
            matches!(&parsed.document.blocks[3], Block::CodeBlock { language: Some(language), code, .. } if language == "rust" && code == "a < b\nline 2")
        );
        let Block::Table { header, rows, .. } = &parsed.document.blocks[4] else {
            panic!("expected a typed table");
        };
        assert_eq!(header.as_ref().unwrap().cells[0].plain_text(), "名称");
        assert_eq!(rows[0].cells[0].plain_text(), "A\n第二行");
    }

    #[test]
    fn html_complex_tables_preserve_their_cleaned_structure() {
        for source in [
            "<table><tr><td colspan=\"2\">merged</td></tr></table>",
            "<table><caption>caption</caption><tr><td>body</td></tr></table>",
            "<table><tr><td><ul><li>nested</li></ul></td></tr></table>",
            "<table><tr><td>first</td><td>second</td></tr><tr><td>short row</td></tr></table>",
        ] {
            let first = parse_source_for_unit(source, "complex-table").unwrap();
            assert!(matches!(&first.document.blocks[0], Block::RawHtml { .. }));
            let second = parse_source_for_unit(&first.canonical_source, "complex-table").unwrap();
            assert_eq!(first.document, second.document);
        }
    }

    #[test]
    fn html_preformatted_text_and_empty_containers_remain_saveable() {
        for (source, expected) in [
            (
                "<pre>before<code>x</code>after<code>y</code></pre>",
                "beforexaftery",
            ),
            (
                "<pre><code>\r\n&lt;a&gt;\r\nline 2</code></pre>",
                "\n<a>\nline 2",
            ),
        ] {
            let first = parse_source_for_unit(source, "pre").unwrap();
            assert!(
                matches!(&first.document.blocks[0], Block::CodeBlock { code, .. } if code == expected)
            );
            let second = parse_source_for_unit(&first.canonical_source, "pre").unwrap();
            assert_eq!(first.document, second.document);
        }
        for source in [
            "<pre><code>before<br>after</code></pre>",
            "<ul></ul>",
            "<ul><li></li></ul>",
            "<blockquote></blockquote>",
            "<table></table>",
            "<ol start=\"0\"><li>zero</li></ol>",
        ] {
            let first = parse_source_for_unit(source, "container").unwrap();
            let second = parse_source_for_unit(&first.canonical_source, "container").unwrap();
            assert_eq!(first.document, second.document, "{source}");
        }
    }

    #[test]
    fn html_source_keeps_markdown_punctuation_as_literal_text() {
        let parsed = parse_source_for_unit("# title **literal**", "literal").unwrap();
        assert_eq!(parsed.canonical_source, "<p># title **literal**</p>");
        assert!(matches!(
            &parsed.document.blocks[0],
            Block::Paragraph { .. }
        ));
    }

    #[test]
    fn html_loose_tasks_and_list_adjacent_text_survive_saving() {
        let parsed = parse_source_for_unit(
            "<ul><li><p><input type=\"checkbox\" checked>done</p></li></ul>",
            "tasks",
        )
        .unwrap();
        let Block::BulletList { items, .. } = &parsed.document.blocks[0] else {
            panic!("expected task list");
        };
        assert_eq!(items[0].checked, Some(true));
        let reparsed = parse_source_for_unit(&parsed.canonical_source, "tasks").unwrap();
        assert_eq!(parsed.document, reparsed.document);

        let parsed = parse_source_for_unit("<ul>prefix<li>x</li>suffix</ul>", "list").unwrap();
        assert!(matches!(&parsed.document.blocks[0], Block::RawHtml { .. }));
        assert!(parsed.document.plain_text().contains("prefix"));
        assert!(parsed.document.plain_text().contains("suffix"));
        let reparsed = parse_source_for_unit(&parsed.canonical_source, "list").unwrap();
        assert_eq!(parsed.document, reparsed.document);
    }

    #[test]
    fn html_removes_scripts_handlers_and_network_resources() {
        let parsed = parse_source_for_unit(
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
        let first = parse_source_for_unit(source, "unit-html").unwrap();
        let second = parse_source_for_unit(&first.canonical_source, "unit-html").unwrap();

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
            &nested_emphasis(MAX_DOCUMENT_DEPTH - 1),
            "unit-at-depth-limit",
        )
        .expect("a visible HTML leaf at the shared depth limit remains accepted");

        let error = parse_source_for_unit(&nested_emphasis(4_096), "unit-over-depth-limit")
            .expect_err("extreme HTML nesting must return an error instead of recursing deeply");
        assert!(error.to_string().contains("HTML nesting exceeds"));
    }

    #[test]
    fn unsafe_html_link_loses_its_navigation_but_keeps_text() {
        let parsed = parse_source_for_unit(
            r#"<p><a href="javascript:alert(1)">保留文字</a></p>"#,
            "unit-1",
        )
        .unwrap();
        assert_eq!(parsed.document.plain_text(), "保留文字");
        assert!(!parsed.canonical_source.contains("javascript"));
    }

    #[test]
    fn html_links_keep_local_navigation_and_inert_network_labels() {
        let source = r##"<p>before<a href="https://example.test/">network</a>after <a href="#section">local</a> <a href="next.xhtml">next</a></p>"##;
        let first = parse_source_for_unit(source, "links").unwrap();
        assert!(!first.canonical_source.contains("https:"));
        assert!(first.canonical_source.contains("beforenetworkafter"));
        assert!(first.canonical_source.contains("href=\"#section\""));
        assert!(first.canonical_source.contains("href=\"next.xhtml\""));
        let second = parse_source_for_unit(&first.canonical_source, "links").unwrap();
        assert_eq!(first.document, second.document);
    }

    #[test]
    fn preserved_html_image_keeps_its_inert_alt_projection() {
        let parsed =
            parse_source_for_unit(r#"<img src="images/figure.png" alt="替代文字">"#, "unit-1")
                .expect("a safe relative image remains representable as inert raw HTML");
        assert!(parsed.document.plain_text().contains("替代文字"));
    }

    #[test]
    fn preserved_html_image_canonicalizes_alt_whitespace() {
        let parsed = parse_source_for_unit(
            "<img src=\"images/figure.png\" alt=\"替代   文字  \n第二行\">",
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
        let html = serialize_source(&document).unwrap();
        assert!(html.contains("moye-asset:video-asset"));
        assert!(html.contains("moye-asset:poster-asset"));
        assert!(!html.contains("blake3/"));
    }

    #[test]
    fn block_media_metadata_survives_html_round_trips() {
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
        let source = serialize_source(&document).unwrap();
        assert!(source.contains("<figcaption>图片说明</figcaption>"));
        assert!(source.contains("<figcaption>音频说明</figcaption>"));
        assert!(source.contains("<figcaption>视频说明</figcaption>"));

        let first = parse_source_for_unit(&source, "unit-media").unwrap();
        let second = parse_source_for_unit(&first.canonical_source, "unit-media").unwrap();
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
