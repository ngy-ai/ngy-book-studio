//! Dropping stylesheet fragments that have no element left to style.

use super::*;

/// Drops CSS that reached the text stream without its `<style>` wrapper.
///
/// A KF8 container keeps each stylesheet in a *flow* record, and the merged text
/// `ebook-rs` assembles ends with that flow appended verbatim. The flow carries
/// no tags, so by then its rules are ordinary text and would be rendered as body
/// copy. This book ends with `@page Section1 { … }` and
/// `div.Section1 { page:Section1 }`.
///
/// The flow lands after the markup that precedes it, so the run that follows a
/// closing tag is the only run the heuristic may touch. A run opened by an
/// element — `<p>@media (max-width: 600px) { … }</p>` in a CSS tutorial — is
/// body copy and is left alone.
pub(crate) fn strip_orphan_css(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while cursor < html.len() {
        let Some(offset) = html[cursor..].find('>') else {
            break;
        };
        let text_start = cursor + offset + 1;
        let text_end = html[text_start..]
            .find('<')
            .map(|end| text_start + end)
            .unwrap_or(html.len());
        output.push_str(&html[cursor..text_start]);
        let text = &html[text_start..text_end];
        if is_orphan_css_rule(text) && !opens_element(&html[cursor..text_start]) {
            tracing::debug!(
                target: "ngy_import",
                bytes = text.len(),
                snippet = %log_snippet(text, CSS_LOG_SNIPPET_CHARS),
                "Kindle 导入：删除游离样式表文本"
            );
        } else {
            output.push_str(text);
        }
        cursor = text_end;
    }
    output.push_str(&html[cursor..]);
    output
}

/// A whole text run that is nothing but CSS at-rules.
pub(crate) fn is_orphan_css_rule(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.ends_with('}')
        && trimmed.contains('{')
        && CSS_AT_RULES.iter().any(|rule| trimmed.starts_with(rule))
}

/// Whether `markup` — everything up to and including the tag that ends right
/// before a text run — ends with an opening tag, which makes that run element
/// content rather than a stylesheet flow.
pub(crate) fn opens_element(markup: &str) -> bool {
    let Some(open) = markup.rfind('<') else {
        return false;
    };
    markup.as_bytes()[open + 1].is_ascii_alphabetic()
}

/// First `limit` characters of a log snippet, so a stray rule cannot flood the
/// log file.
pub(crate) fn log_snippet(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &trimmed[..end]),
        None => trimmed.to_string(),
    }
}
