//! Rendering a thought as HTML.

use super::*;

pub(crate) fn thought_display_html(content: &str) -> String {
    let display = crate::ui::ai_sidebar::markdown::assistant_display_markdown(content);
    if let Ok(html) = ::markdown::to_html_with_options(&display, &::markdown::Options::gfm())
        && html.len() <= MAX_NOTE_DISPLAY_HTML_BYTES
    {
        return html;
    }
    let mut html = String::from("<pre>");
    for character in content.chars() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            _ => html.push(character),
        }
    }
    html.push_str("</pre>");
    html
}
