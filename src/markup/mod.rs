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

use crate::ai::translation::{TranslationSource, has_visible_text, normalize_source_text};
use crate::document::{
    Block, BlockDocument, Inline, ListItem, MAX_DOCUMENT_DEPTH, TableCell, TableRow,
    deterministic_id, raw_html_plain_text,
};

mod html_out;

mod html_parse;

mod html_read;

mod translation;

mod xml;

pub(crate) use html_out::*;
pub(crate) use html_parse::*;
pub(crate) use html_read::*;
pub(crate) use xml::*;

// The public surface of this module, unchanged by the split.
pub use html_parse::sanitize_html;
pub use translation::{
    block_texts_from_html, translation_blocks_from_document_html, translation_blocks_from_html,
};
pub use xml::{XmlSafeHtml, xml_safe_entities};

#[cfg(test)]
mod tests;
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
