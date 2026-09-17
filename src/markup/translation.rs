//! Extracting the translatable text of a chapter, including the code
//! detection that keeps blocks in their original language.

use super::*;

/// Block-level element names whose visible text forms one translatable run.
/// This is the same candidate set the reader's translation layer matches, so
/// the persisted译文 and the rendered chapter stay aligned.
pub(crate) const TRANSLATABLE_BLOCK_TAGS: [&str; 11] = [
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

pub(crate) const SKIPPED_TEXT_TAGS: [&str; 4] = ["script", "style", "noscript", "template"];

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

pub(crate) fn is_translatable_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if TRANSLATABLE_BLOCK_TAGS.contains(&name.local.as_ref()))
}

pub(crate) fn is_skipped_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if SKIPPED_TEXT_TAGS.contains(&name.local.as_ref()))
}

pub(crate) fn is_code_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if matches!(name.local.as_ref(), "pre" | "code"))
}

pub(crate) fn is_line_break_element(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. }
        if name.local.as_ref() == "br")
}

/// Line endings only a statement ends a line with. Full-width CJK punctuation is
/// deliberately absent: `；`/`：` end prose or quotes, never code.
pub(crate) const CODE_LINE_ENDINGS: [char; 3] = [';', '{', '}'];

/// Comment and preprocessor markers only a code line starts with.
pub(crate) const CODE_LINE_PREFIXES: [&str; 8] = [
    "//", "/*", "*/", "#!", "#include", "#define", "#pragma", "<!--",
];

/// Operators prose does not contain. A bare `=` is excluded: formulas and prose
/// use it too.
pub(crate) const CODE_OPERATORS: [&str; 16] = [
    "=>", "->", "::", ":=", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "</", "/>",
];

/// Keywords that open a statement. Prose opens sentences with words from the
/// same list too ("let me explain …", "use the following …"), so a keyword only
/// counts together with a code shape on the same line — see `has_code_keyword`.
pub(crate) const CODE_LINE_KEYWORDS: [&str; 25] = [
    "def",
    "fn",
    "func",
    "impl",
    "struct",
    "enum",
    "trait",
    "class",
    "let",
    "var",
    "const",
    "static",
    "public",
    "private",
    "protected",
    "pub",
    "use",
    "import",
    "package",
    "export",
    "return",
    "println",
    "printf",
    "console",
    "system",
];

/// Shapes of a string literal that only source code writes. A quote on its own
/// never counts: prose wraps quoted words in spaces and punctuation (`He said
/// "hello" to me.`), so treating any quote as code would keep every paragraph
/// containing a quotation in the original language. The last four entries are the
/// call and index forms: `println("x")`, `func('a')`.
pub(crate) const CODE_STRING_SHAPES: [&str; 8] =
    ["+\"", "\"+", "+ \"", "\" +", "(\"", "\")", "('", "')"];

/// ECMAScript whitespace plus the invisible format characters EPUBs use to indent
/// code lines, which `trim` alone would keep in front of `//` or `#`.
pub(crate) fn trim_code_line(value: &str) -> &str {
    value.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '\u{200b}' | '\u{feff}' | '\u{00ad}')
    })
}

/// Whether one line carries a string literal the way source code does:
/// concatenation, an argument list, or an escape sequence.
pub(crate) fn has_code_string_shape(line: &str) -> bool {
    CODE_STRING_SHAPES.iter().any(|shape| line.contains(shape)) || has_escape_sequence(line)
}

/// 转义序列（`\n`、`\"`、`\\` …）。散文把换行写成真正的换行，不会写这两个字符。
pub(crate) fn has_escape_sequence(line: &str) -> bool {
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            continue;
        }
        if matches!(
            characters.next(),
            Some('n' | 't' | 'r' | 'b' | 'f' | '0' | '\\' | '\'' | '"')
        ) {
            return true;
        }
    }
    false
}

/// 语句关键字开头的行。关键字本身不足以判定：英文句子也可能以 `let` / `use` /
/// `return` 开头，所以同一行还必须出现调用、赋值、花括号或下标。
pub(crate) fn has_code_keyword(line: &str) -> bool {
    let word = line
        .split_once(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
        .map_or(line, |(word, _)| word);
    if !CODE_LINE_KEYWORDS.contains(&word) {
        return false;
    }
    let rest = &line[word.len()..];
    rest.contains('(') || rest.contains('=') || rest.contains('{') || rest.contains('[')
}

/// 无空格的类型注解（`a:Int`、`):Int`、`x:String`）。散文的冒号后面跟着空格
/// （`Note: the file`），`https://example.test` 这类 URL 的冒号后是斜杠。
pub(crate) fn has_type_annotation(line: &str) -> bool {
    let mut previous = '\0';
    for character in line.chars() {
        if previous == ':' && character.is_ascii_alphabetic() {
            return true;
        }
        previous = character;
    }
    false
}

pub(crate) fn is_code_line(line: &str) -> bool {
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
        || has_code_string_shape(trimmed)
        || has_code_keyword(trimmed)
        || has_type_annotation(trimmed)
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
pub(crate) fn looks_like_source_code(lines: &str) -> bool {
    lines.lines().any(is_code_line)
}

pub(crate) fn collect_translation_sources(
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

pub(crate) fn collect_translation_leaves(
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

pub(crate) fn has_translatable_descendant(node: &Handle, depth: usize) -> Result<bool> {
    pub(crate) fn scan(node: &Handle, depth: usize) -> Result<bool> {
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
