//! Matching, labelling and copying a note.

use super::*;

pub(crate) fn note_matches(note: &AnnotationOverview, query: &str) -> bool {
    query.is_empty()
        || [
            note.book_title.as_str(),
            note.chapter_title.as_deref().unwrap_or_default(),
            note.annotation.anchor.quote.as_str(),
            note.annotation.comment.as_deref().unwrap_or_default(),
        ]
        .iter()
        .any(|text| text.to_lowercase().contains(query))
}

pub(crate) fn plain_text_html(text: &str) -> String {
    let mut html = String::with_capacity(text.len().saturating_add(16));
    html.push_str("<p>");
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                html.push_str("</p><p>");
            }
            '\n' => html.push_str("</p><p>"),
            _ => html.push(character),
        }
    }
    html.push_str("</p>");
    html
}

pub(crate) fn kind_label(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Highlight => "马克笔",
        AnnotationKind::Wavy => "波浪线",
        AnnotationKind::Underline => "直线",
        AnnotationKind::HumanComment => "人工想法",
        AnnotationKind::AiComment => "AI 想法",
    }
}

pub(crate) fn chapter_label(note: &AnnotationOverview) -> String {
    match note.chapter_index {
        None => "章节已移除".into(),
        Some(index) => note
            .chapter_title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("第 {} 章", index + 1)),
    }
}

pub(crate) fn copy_text(note: &AnnotationOverview) -> String {
    let mut text = format!(
        "《{}》 · {}\n{}\n{}",
        note.book_title,
        chapter_label(note),
        kind_label(note.annotation.kind),
        note.annotation.anchor.quote
    );
    if let Some(comment) = &note.annotation.comment {
        text.push_str("\n\n");
        text.push_str(comment);
    }
    text
}
