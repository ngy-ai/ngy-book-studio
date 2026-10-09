//! Reading one answered message: badges and the selectable text projection.

use super::*;

pub(crate) fn reference_key(reference: &AiReferenceHint) -> AiReferenceKey {
    (
        reference.book_id.clone(),
        reference.unit_id.clone(),
        reference
            .locator
            .as_ref()
            .and_then(|locator| serde_json::to_string(locator).ok()),
    )
}

pub(crate) fn valid_reference_hint_locator(reference: &AiReferenceHint) -> bool {
    reference.locator.as_ref().is_none_or(|locator| {
        locator.book_id == reference.book_id
            && locator.unit_id == reference.unit_id
            && locator.validate().is_ok()
    })
}

pub(crate) fn allowed_source_link(allowed: &HashSet<String>, source: &AiSourceLink) -> bool {
    // Web citations have no book identity and are never scoped by the current
    // library; they are shown whenever the answer used them.
    if source.url.is_some() {
        return true;
    }
    allowed.contains(&source.book_id)
        && !source.unit_id.trim().is_empty()
        && source
            .locator
            .as_ref()
            .is_none_or(|_| source.validated_locator().is_some())
}

pub(crate) fn should_show_no_knowledge_base_source_warning(message: &AiMessage) -> bool {
    message.role == AiMessageRole::Assistant
        && message.source_status == Some(AgentAnswerSourceStatus::NoVerifiedSources)
}

/// A short basis badge shown above a grounded answer, telling the user whether
/// the answer came from the current books, a web search, or both.
pub(crate) fn source_basis_badge(message: &AiMessage) -> Option<(&'static str, &'static str)> {
    if message.role != AiMessageRole::Assistant {
        return None;
    }
    match message.source_status? {
        AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources => {
            Some(("已基于当前图书内容回答", "回答引用均来自本次选中的图书。"))
        }
        AgentAnswerSourceStatus::VerifiedWebSources => Some((
            "已基于联网搜索结果回答",
            "当前图书中未检索到相关内容，回答引用来自联网搜索。",
        )),
        AgentAnswerSourceStatus::VerifiedMixedSources => Some((
            "已结合图书与联网搜索回答",
            "回答同时引用图书内容和联网搜索结果。",
        )),
        AgentAnswerSourceStatus::NoVerifiedSources => None,
    }
}

pub(crate) fn selectable_message_html(message: &str) -> String {
    let mut html = String::with_capacity(message.len().saturating_add(32));
    let mut characters = message.chars().peekable();
    html.push_str("<p>");
    while let Some(character) = characters.next() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
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

// Cache the display projection by message, so repainting the sidebar does not
// reparse every historical answer. The original content remains the copy/save
// value; only assistant messages opt into Markdown formatting.
#[inline(never)]
pub(crate) fn selectable_message_text(
    id: impl Into<SharedString>,
    role: AiMessageRole,
    content: &str,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    let id = id.into();
    let cache = window.use_keyed_state(SharedString::from(format!("{id}/display")), cx, |_, _| {
        None::<(AiMessageRole, String, SharedString)>
    });
    if cache
        .read(cx)
        .as_ref()
        .is_none_or(|(cached_role, original, _)| *cached_role != role || original != content)
    {
        let display = if role == AiMessageRole::Assistant {
            markdown::assistant_display_markdown(content)
        } else {
            selectable_message_html(content)
        };
        cache.update(cx, |cache, _| {
            *cache = Some((role, content.to_owned(), display.into()));
        });
    }
    let display = cache.read(cx).as_ref().unwrap().2.clone();
    let text = if role == AiMessageRole::Assistant {
        TextView::markdown(SharedString::from(format!("{id}/markdown")), display)
            .style(TextViewStyle::default().paragraph_gap(rems(0.5)))
    } else {
        TextView::html(SharedString::from(format!("{id}/plain")), display)
            .style(TextViewStyle::default().paragraph_gap(rems(0.)))
    };
    // Move the whole naturally sized view with the outer conversation scroller.
    // TextView's internal virtual scrolling invalidates selection coordinates.
    text.selectable(true)
        .scrollable(false)
        .w(width)
        .h_auto()
        .min_w(px(0.))
}
