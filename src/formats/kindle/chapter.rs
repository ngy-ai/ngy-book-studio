//! Splitting the imported sections into chapters.

use super::*;

/// One chapter of the rebuilt book.
pub(crate) struct KindleChapter {
    /// Title taken from the chapter's level-one heading, empty when the chapter
    /// opens without one.
    pub(crate) title: String,
    /// Href of the first section that fed this chapter.
    pub(crate) href: String,
    pub(crate) html: String,
}

/// Regroups the sections `ebook-rs` produced into the chapters the book has.
///
/// `ebook-rs` does not read a MOBI table of contents: it splits the merged
/// markup at every `<h1>`/`<h2>`/`<h3>` and then labels each piece
/// `Section <n>`, which for a converted document turns a chapter into hundreds
/// of pieces, each displayed under a meaningless title. The real chapter list
/// lives on the level-one headings — a converted book writes one `<h1>` per
/// chapter, and the `_Toc…` bookmarks its navigation points at sit inside them —
/// so only a level-one heading that carries text opens a new chapter, and the
/// rest of the headings stay with the chapter they belong to.
pub(crate) fn kindle_chapters(sections: &[ebook_rs::Section]) -> Vec<KindleChapter> {
    let mut chapters: Vec<KindleChapter> = Vec::new();
    for section in sections {
        if let Some(title) = leading_heading(&section.raw_html, "h1") {
            chapters.push(KindleChapter {
                title,
                href: section.href.clone(),
                html: section.raw_html.clone(),
            });
            continue;
        }
        match chapters.last_mut() {
            Some(current) => {
                current.html.push('\n');
                current.html.push_str(&section.raw_html);
            }
            None => chapters.push(KindleChapter {
                title: String::new(),
                href: section.href.clone(),
                html: section.raw_html.clone(),
            }),
        }
    }
    chapters
}

/// Text of the heading that opens this markup, when it is `tag`.
pub(crate) fn leading_heading(html: &str, tag: &str) -> Option<String> {
    let trimmed = html.trim_start();
    let open = format!("<{tag}");
    if !trimmed.get(..open.len())?.eq_ignore_ascii_case(&open) {
        return None;
    }
    let rest = &trimmed[open.len()..];
    if !rest.starts_with(|character: char| {
        character == '>' || character == '/' || character.is_ascii_whitespace()
    }) {
        return None;
    }
    let close = format!("</{tag}>");
    let body_start = trimmed.find('>')? + 1;
    let body_end = find_ignore_case(&trimmed[body_start..], &close)? + body_start;
    let text = heading_text(&trimmed[body_start..body_end]);
    (!text.is_empty()).then_some(text)
}

/// Visible text of a markup fragment: tags removed, entities decoded and
/// whitespace collapsed.
pub(crate) fn heading_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find('<') {
        let start = cursor + offset;
        text.push_str(&html[cursor..start]);
        let name_start = start + 1;
        let opens_tag = html[name_start..].starts_with(|character: char| {
            character.is_ascii_alphabetic()
                || character == '/'
                || character == '!'
                || character == '?'
        });
        if !opens_tag {
            // A bare `<` in running text is content, not a tag.
            text.push('<');
            cursor = name_start;
            continue;
        }
        let Some(end) = html[start..].find('>') else {
            break;
        };
        cursor = start + end + 1;
    }
    text.push_str(&html[cursor..]);
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
