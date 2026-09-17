//! XHTML output: the entity rewriting and character checks that make a
//! fragment safe for the reader WebView to parse as XML.

use super::*;

/// The named character references XML predefines, so they need no DTD.
pub(crate) const XML_PREDEFINED_ENTITIES: [&str; 5] = ["amp", "lt", "gt", "quot", "apos"];

/// An HTML fragment rewritten so an XML parser accepts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlSafeHtml<'a> {
    pub html: Cow<'a, str>,
    /// Distinct named references that were rewritten, in first-seen order.
    pub rewritten: Vec<&'a str>,
    /// How many references were rewritten in total.
    pub rewritten_count: usize,
}

/// Rewrites named character references XML does not predefine into numeric
/// character references.
///
/// The reader serves chapters as `application/xhtml+xml`, where only `&amp;`
/// `&lt;` `&gt;` `&quot;` `&apos;` and numeric references resolve; any other
/// name makes WebView2 show its parse-error page ("Entity 'nbsp' not defined")
/// instead of the chapter. HTML serializers produce exactly that: html5ever
/// spells U+00A0 `&nbsp;`, and EPUB sources written for HTML parsers use
/// `&mdash;`, `&ldquo;` and friends. Numbers are always legal and the
/// codepoints come from the same entity table the HTML parser uses, so the
/// rendered text is unchanged and a numeric reference stays valid in an HTML
/// context too.
///
/// A reference whose name is not in that table is left exactly as it is: only a
/// real entity can be rewritten, and guessing at malformed markup would corrupt
/// the text.
pub fn xml_safe_entities(html: &str) -> XmlSafeHtml<'_> {
    if !html.contains('&') {
        return XmlSafeHtml {
            html: Cow::Borrowed(html),
            rewritten: Vec::new(),
            rewritten_count: 0,
        };
    }
    let mut output = String::with_capacity(html.len());
    let mut rewritten: Vec<&str> = Vec::new();
    let mut rewritten_count = 0_usize;
    let mut copied_until = 0_usize;
    let mut cursor = 0_usize;
    while let Some(offset) = html[cursor..].find('&') {
        let amp = cursor + offset;
        let name_start = amp + 1;
        let name_end = html[name_start..]
            .find(|character: char| !character.is_ascii_alphanumeric())
            .map(|length| name_start + length)
            .unwrap_or(html.len());
        let name = &html[name_start..name_end];
        if name.is_empty()
            || !html[name_end..].starts_with(';')
            || XML_PREDEFINED_ENTITIES.contains(&name)
        {
            cursor = name_end.max(amp + 1);
            continue;
        }
        // Keys keep the trailing `;`. The build script also registers every
        // strict prefix of a name as `(0, 0)`, so a zero first codepoint means
        // "prefix of a longer name", never an entity of its own.
        let Some(&(first, second)) =
            html5ever::data::NAMED_ENTITIES.get(&html[name_start..=name_end])
        else {
            cursor = name_end + 1;
            continue;
        };
        if first == 0 {
            cursor = name_end + 1;
            continue;
        }
        output.push_str(&html[copied_until..amp]);
        output.push_str(&format!("&#{first};"));
        if second != 0 {
            output.push_str(&format!("&#{second};"));
        }
        copied_until = name_end + 1;
        if !rewritten.contains(&name) {
            rewritten.push(name);
        }
        rewritten_count += 1;
        cursor = name_end + 1;
    }
    if rewritten_count == 0 {
        return XmlSafeHtml {
            html: Cow::Borrowed(html),
            rewritten,
            rewritten_count,
        };
    }
    output.push_str(&html[copied_until..]);
    XmlSafeHtml {
        html: Cow::Owned(output),
        rewritten,
        rewritten_count,
    }
}

pub(crate) const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";

pub(crate) const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

pub(crate) fn ensure_xml_characters(value: &str) -> Result<()> {
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

pub(crate) fn write_xhtml_value(value: &str, attribute: bool, output: &mut String) -> Result<()> {
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

pub(crate) fn ensure_xhtml_local_name(name: &str) -> Result<()> {
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

pub(crate) fn write_xhtml_node(node: &Handle, depth: usize, output: &mut String) -> Result<()> {
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
