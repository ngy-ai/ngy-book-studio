use super::*;
use gpui::{EventEmitter, Pixels, rems};
use gpui_component::{
    clipboard::Clipboard,
    text::{TextView, TextViewStyle},
};
use moye_epub_editor::{
    agent::AgentAnswerSourceStatus,
    ai::normalize_provider_base_url,
    document::{BookDocument, ContentUnit, DocumentLocator, Revision, SourceLocator},
    services::{ModelRole, ProviderSettings},
};
use serde::Serialize;
use std::net::IpAddr;

pub(super) mod markdown;
#[cfg(test)]
mod markdown_ui_tests;

pub(super) const AI_SIDEBAR_WIDTH: f32 = 360.;
pub(super) const AI_SIDEBAR_MIN_WIDTH: f32 = 280.;
pub(super) const AI_SIDEBAR_MAX_WIDTH: f32 = 640.;
pub(super) const AI_SIDEBAR_COLLAPSED_WIDTH: f32 = 44.;
const AI_SIDEBAR_NARROW_THRESHOLD: f32 = 1080.;
const AI_REFERENCE_VISIBLE_ROWS: usize = 3;
const AI_REFERENCE_ROW_HEIGHT: f32 = 28.;
const MAX_QUESTION_BYTES: usize = 32 * 1024;
const SELECTION_EXPLANATION_QUESTION: &str = "用汉语详细解释一下";
const NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE: &str = "没有可验证的知识库来源";
const NO_KNOWLEDGE_BASE_SOURCE_WARNING_BODY: &str =
    "以下回答由大模型根据自身能力生成，不能视为基于所选图书的回答，请自行核实。";
const AI_ENDPOINT_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
pub(super) const MAX_SELECTED_REFERENCES: usize = 16;
// Used by the response-side adapter API, which is deliberately not wired to a
// concrete backend from this UI module.
#[allow(dead_code)]
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

fn clamp_ai_sidebar_width(width: Pixels) -> Pixels {
    width.clamp(px(AI_SIDEBAR_MIN_WIDTH), px(AI_SIDEBAR_MAX_WIDTH))
}

fn reference_list_max_height(reference_count: usize) -> Pixels {
    px(AI_REFERENCE_ROW_HEIGHT * reference_count.min(AI_REFERENCE_VISIBLE_ROWS) as f32)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiThreadOption {
    pub id: String,
    pub title: String,
    pub book_ids: Vec<String>,
}

impl AiThreadOption {
    pub(super) fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        book_ids: Vec<String>,
    ) -> Self {
        let title = title.into();
        Self {
            id: id.into(),
            title: non_empty_label(title, "新对话"),
            book_ids,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AiEndpointStatus {
    Local,
    Remote { hostname: String },
    Unavailable,
}

impl AiEndpointStatus {
    fn from_base_url(base_url: &str) -> Self {
        let Ok(url) = normalize_provider_base_url(base_url) else {
            return Self::Unavailable;
        };
        let Some(hostname) = url
            .host_str()
            .map(|host| host.trim_matches(['[', ']']).to_string())
        else {
            return Self::Unavailable;
        };
        let loopback = hostname.eq_ignore_ascii_case("localhost")
            || hostname
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if loopback {
            Self::Local
        } else {
            Self::Remote { hostname }
        }
    }

    fn current(services: &AppServices) -> Self {
        services
            .provider_settings()
            .map(|settings| Self::from_settings(&settings))
            .unwrap_or(Self::Unavailable)
    }

    fn from_settings(settings: &ProviderSettings) -> Self {
        let mut remote = Vec::new();
        for (role, label) in [
            (ModelRole::Chat, "对话"),
            (ModelRole::Embedding, "Embedding"),
            (ModelRole::Vision, "视觉"),
        ] {
            let Ok(endpoint) = settings.endpoint_for(role) else {
                return Self::Unavailable;
            };
            match Self::from_base_url(&endpoint.base_url) {
                Self::Local => {}
                Self::Remote { hostname } => remote.push(format!("{label}：{hostname}")),
                Self::Unavailable => return Self::Unavailable,
            }
        }
        if remote.is_empty() {
            Self::Local
        } else {
            Self::Remote {
                hostname: remote.join("；"),
            }
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Local => "本地".to_string(),
            Self::Remote { hostname } => format!("远程 · {hostname}"),
            Self::Unavailable => "端点不可用".to_string(),
        }
    }

    fn is_warning(&self) -> bool {
        !matches!(self, Self::Local)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiBookOption {
    pub id: String,
    pub title: String,
}

impl AiBookOption {
    pub(super) fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        let id = id.into();
        let title = title.into();
        Self {
            id,
            title: if title.trim().is_empty() {
                "未命名图书".to_string()
            } else {
                title
            },
        }
    }
}

/// Scope is supplied by the owning window. This component never queries the
/// library or database itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AiSidebarScope {
    Library {
        label: String,
        books: Vec<AiBookOption>,
    },
    Book {
        current: AiBookOption,
        available: Vec<AiBookOption>,
        additional_ids: Vec<String>,
    },
}

impl AiSidebarScope {
    pub(super) fn library(label: impl Into<String>, books: Vec<AiBookOption>) -> Self {
        Self::Library {
            label: non_empty_label(label.into(), "当前书架"),
            books: normalize_books(books),
        }
    }

    pub(super) fn book(current: AiBookOption, available: Vec<AiBookOption>) -> Self {
        let current = AiBookOption::new(current.id, current.title);
        let mut available = normalize_books(available);
        if !available.iter().any(|book| book.id == current.id) {
            available.insert(0, current.clone());
        }
        Self::Book {
            current,
            available,
            additional_ids: Vec::new(),
        }
    }

    pub(super) fn book_ids(&self) -> Vec<String> {
        match self {
            Self::Library { books, .. } => books.iter().map(|book| book.id.clone()).collect(),
            Self::Book {
                current,
                additional_ids,
                ..
            } => std::iter::once(current.id.clone())
                .chain(additional_ids.iter().cloned())
                .collect(),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Library { label, .. } => label.clone(),
            Self::Book {
                current,
                additional_ids,
                ..
            } => {
                if additional_ids.is_empty() {
                    current.title.clone()
                } else {
                    format!("{} 等 {} 本", current.title, additional_ids.len() + 1)
                }
            }
        }
    }

    fn selected_books(&self) -> Vec<AiBookOption> {
        match self {
            Self::Library { books, .. } => books.clone(),
            Self::Book {
                current,
                available,
                additional_ids,
            } => {
                let mut selected = vec![current.clone()];
                for id in additional_ids {
                    if let Some(book) = available.iter().find(|book| book.id == *id) {
                        selected.push(book.clone());
                    }
                }
                selected
            }
        }
    }

    fn available_books_to_add(&self) -> Vec<AiBookOption> {
        let Self::Book {
            current,
            available,
            additional_ids,
        } = self
        else {
            return Vec::new();
        };
        available
            .iter()
            .filter(|book| book.id != current.id && !additional_ids.contains(&book.id))
            .cloned()
            .collect()
    }

    fn add_book(&mut self, book_id: &str) -> bool {
        let Self::Book {
            current,
            available,
            additional_ids,
        } = self
        else {
            return false;
        };
        if book_id == current.id
            || additional_ids.iter().any(|id| id == book_id)
            || !available.iter().any(|book| book.id == book_id)
        {
            return false;
        }
        additional_ids.push(book_id.to_string());
        true
    }

    fn remove_book(&mut self, book_id: &str) -> bool {
        let Self::Book {
            current,
            additional_ids,
            ..
        } = self
        else {
            return false;
        };
        if book_id == current.id {
            return false;
        }
        let before = additional_ids.len();
        additional_ids.retain(|id| id != book_id);
        before != additional_ids.len()
    }
}

fn normalize_books(books: Vec<AiBookOption>) -> Vec<AiBookOption> {
    let mut seen = HashSet::new();
    books
        .into_iter()
        .filter_map(|book| {
            let id = book.id.trim().to_string();
            if id.is_empty() || !seen.insert(id.clone()) {
                return None;
            }
            Some(AiBookOption::new(id, book.title))
        })
        .collect()
}

fn non_empty_label(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiReferenceHint {
    pub book_id: String,
    pub unit_id: String,
    pub unit_index: Option<usize>,
    /// Optional host-owned coordinate for a page/block-specific reference.
    /// The controller validates it against the selected book and content unit
    /// before exposing it to the agent snapshot.
    pub locator: Option<DocumentLocator>,
    pub label: String,
    /// A host-frozen selection. `None` means the backend should resolve the
    /// persisted unit after it receives the typed request.
    pub frozen_text: Option<String>,
    pub revision: Option<u64>,
}

impl AiReferenceHint {
    pub(super) fn chapter(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        unit_index: usize,
        label: impl Into<String>,
    ) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            unit_index: Some(unit_index),
            locator: None,
            label: label.into(),
            frozen_text: None,
            revision: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiSourceLink {
    pub citation_id: String,
    pub book_id: String,
    pub unit_id: String,
    pub unit_index: Option<usize>,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    /// Exact locator recorded by the citation registry and persisted with the
    /// chat message. `None` keeps older unit-only UI flows working.
    pub locator: Option<DocumentLocator>,
    pub label: String,
    pub quote: Option<String>,
    /// True only for a host-frozen editor selection. Persisted rows recover
    /// this provenance from the absence of a search chunk ID; visual/search
    /// passages remain false and are not revalidated against the text AST.
    pub selection_snapshot: bool,
    /// Set after the host compares the stored versions and locator with the
    /// current canonical document. Stale links remain visible for auditability
    /// but every window must refuse to navigate them.
    pub stale: bool,
    /// For web-sourced citations, the absolute http(s) URL to open. Book
    /// citations leave this `None` and navigate by locator instead.
    pub url: Option<String>,
}

/// A version-checked citation target that is safe for a host window to open.
/// The target text is derived again from the current block AST instead of
/// trusting the model-provided quote. A unit-only selection can still carry a
/// quote, but DOM navigation will reject it if it is absent or ambiguous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiCitationNavigationTarget {
    pub unit_index: usize,
    pub locator: DocumentLocator,
    pub source: Option<SourceLocator>,
    pub focus: Option<AiCitationTextFocus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct AiCitationTextFocus {
    text: String,
    before: String,
    after: String,
}

const CITATION_CONTEXT_CHARACTERS: usize = 96;
const MAX_CITATION_FOCUS_BYTES: usize = 16 * 1024;

impl AiSourceLink {
    pub(super) fn validated_locator(&self) -> Option<&DocumentLocator> {
        self.locator.as_ref().filter(|locator| {
            locator.book_id == self.book_id
                && locator.unit_id == self.unit_id
                && locator.validate().is_ok()
        })
    }

    pub(super) fn current_unit_index(&self, document: &BookDocument) -> Option<usize> {
        if self.stale || document.id != self.book_id || document.revision != self.document_revision
        {
            return None;
        }
        let index = document
            .units
            .iter()
            .position(|unit| unit.id == self.unit_id)?;
        let unit = &document.units[index];
        if unit.revision != self.unit_revision
            || self.unit_index.is_some_and(|candidate| candidate != index)
            || self
                .locator
                .as_ref()
                .is_some_and(|locator| document.validate_locator(locator).is_err())
        {
            return None;
        }
        Some(index)
    }

    fn selection_matches_current_ast(&self, unit: &ContentUnit) -> bool {
        let Some(locator) = self.validated_locator() else {
            return false;
        };
        if matches!(
            locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        ) || locator
            .source
            .as_ref()
            .is_some_and(|source| unit.source_locator.as_ref() != Some(source))
        {
            return false;
        }
        // Editor selections are textual. Region citations come from visual
        // search passages and must not be judged against reflowed AST text.
        if locator.region.is_some() {
            return false;
        }
        let Some(quote) = non_empty_citation_quote(self.quote.as_deref()) else {
            return false;
        };
        let text = match locator.block_id.as_deref() {
            Some(block_id) => match unit.find_block(block_id) {
                Some(block) => block.plain_text(),
                None => return false,
            },
            None => unit.plain_text(),
        };
        match locator.text_range {
            Some(range) => {
                let Ok(start) = usize::try_from(range.start_byte) else {
                    return false;
                };
                let Ok(end) = usize::try_from(range.end_byte) else {
                    return false;
                };
                text.get(start..end) == Some(quote)
            }
            None => text.match_indices(quote).take(2).count() == 1,
        }
    }

    pub(super) fn mark_current_status(&mut self, document: Option<&BookDocument>) {
        self.stale = false;
        self.stale = document
            .and_then(|document| self.current_unit_index(document))
            .is_none();
    }

    pub(super) fn mark_restored_status(&mut self, document: Option<&BookDocument>) {
        self.mark_current_status(document);
        if self.stale || !self.selection_snapshot {
            return;
        }
        let Some(document) = document else {
            self.stale = true;
            return;
        };
        let Some(unit) = document.units.iter().find(|unit| unit.id == self.unit_id) else {
            self.stale = true;
            return;
        };
        self.stale = !self.selection_matches_current_ast(unit);
    }

    pub(super) fn current_navigation_target(
        &self,
        document: &BookDocument,
    ) -> std::result::Result<AiCitationNavigationTarget, String> {
        let unit_index = self
            .current_unit_index(document)
            .ok_or_else(|| "引用对应的文档或内容版本已失效".to_string())?;
        let unit = &document.units[unit_index];
        let locator = self
            .validated_locator()
            .cloned()
            .unwrap_or_else(|| DocumentLocator::unit(&self.book_id, &self.unit_id));
        document
            .validate_locator(&locator)
            .map_err(|error| format!("引用定位已失效：{error}"))?;
        if locator.region.is_some() {
            return Err("该引用是视觉区域定位，当前可重排正文无法精确显示该区域".to_string());
        }
        if let Some(source) = locator.source.as_ref()
            && unit.source_locator.as_ref() != Some(source)
        {
            return Err("引用的原始来源位置与当前内容单元不匹配".to_string());
        }

        let focus = match locator.block_id.as_deref() {
            Some(block_id) => {
                let block = unit
                    .find_block(block_id)
                    .ok_or_else(|| "引用对应的正文块已不存在".to_string())?;
                let block_text = block.plain_text();
                let (start, located_text) = if let Some(range) = locator.text_range {
                    let start = usize::try_from(range.start_byte)
                        .map_err(|_| "引用文字起点超出当前平台范围".to_string())?;
                    let end = usize::try_from(range.end_byte)
                        .map_err(|_| "引用文字终点超出当前平台范围".to_string())?;
                    let located = block_text
                        .get(start..end)
                        .ok_or_else(|| "引用文字范围已失效".to_string())?;
                    (start, located)
                } else if let Some(quote) = non_empty_citation_quote(self.quote.as_deref()) {
                    let mut matches = block_text.match_indices(quote);
                    let start = matches
                        .next()
                        .map(|(start, _)| start)
                        .ok_or_else(|| "引用文字与当前正文块不匹配".to_string())?;
                    if matches.next().is_some() {
                        return Err("引用文字在当前正文块中不唯一，无法精确定位".to_string());
                    }
                    (start, quote)
                } else {
                    (0, block_text.as_str())
                };
                let focus_text = match non_empty_citation_quote(self.quote.as_deref()) {
                    Some(quote) if located_text.starts_with(quote) => quote,
                    Some(_) => return Err("引用文字与当前正文范围不匹配".to_string()),
                    None => located_text,
                };
                Some(citation_text_focus(&block_text, start, focus_text)?)
            }
            None => non_empty_citation_quote(self.quote.as_deref())
                .map(|quote| citation_text_focus(quote, 0, quote))
                .transpose()?,
        };

        Ok(AiCitationNavigationTarget {
            unit_index,
            source: locator.source.clone(),
            locator,
            focus,
        })
    }
}

fn non_empty_citation_quote(quote: Option<&str>) -> Option<&str> {
    quote.filter(|quote| !quote.trim().is_empty())
}

fn citation_text_focus(
    block_text: &str,
    start: usize,
    focus_text: &str,
) -> std::result::Result<AiCitationTextFocus, String> {
    if focus_text.is_empty() {
        return Err("引用文字为空，无法精确定位".to_string());
    }
    if focus_text.len() > MAX_CITATION_FOCUS_BYTES {
        return Err("引用文字超过精确定位上限".to_string());
    }
    let end = start
        .checked_add(focus_text.len())
        .filter(|end| block_text.get(start..*end) == Some(focus_text))
        .ok_or_else(|| "引用文字边界已失效".to_string())?;
    let before_start = block_text[..start]
        .char_indices()
        .rev()
        .nth(CITATION_CONTEXT_CHARACTERS)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let after_end = block_text[end..]
        .char_indices()
        .nth(CITATION_CONTEXT_CHARACTERS)
        .map(|(index, _)| end + index)
        .unwrap_or(block_text.len());
    Ok(AiCitationTextFocus {
        text: focus_text.to_string(),
        before: block_text[before_start..start].to_string(),
        after: block_text[end..after_end].to_string(),
    })
}

/// Generates a host-owned script that selects one unambiguous DOM text range.
/// It never falls back to the start of the page when the text cannot be mapped.
/// The caller supplies an IPC envelope containing its own page identity.
pub(super) fn citation_dom_navigation_script(
    focus: &AiCitationTextFocus,
    result_envelope: serde_json::Value,
) -> std::result::Result<String, String> {
    #[derive(Serialize)]
    struct Payload<'a> {
        focus: &'a AiCitationTextFocus,
        result: serde_json::Value,
    }

    let payload = serde_json::to_string(&Payload {
        focus,
        result: result_envelope,
    })
    .map_err(|error| format!("无法编码引用定位请求：{error}"))?;
    Ok(format!(
        r#"(() => {{
  "use strict";
  const payload = {payload};
  const finish = (found, reason) => {{
    try {{
      window.ipc?.postMessage(JSON.stringify({{ ...payload.result, found, reason }}));
    }} catch {{}}
  }};
  const run = () => {{
    const root = document.querySelector('.ProseMirror') || document.body;
    if (!root) {{ finish(false, 'missing_root'); return; }}
    const chars = [];
    const positions = [];
    let pendingSpace = null;
    let hasPendingSpace = false;
    const emitSpace = (position) => {{
      if (chars.length > 0 && !hasPendingSpace) {{
        pendingSpace = position;
        hasPendingSpace = true;
      }}
    }};
    const emitText = (node) => {{
      const value = node.data || '';
      for (let index = 0; index < value.length; index += 1) {{
        const character = value[index];
        if (/\s/u.test(character)) {{
          emitSpace([node, index, index + 1]);
          continue;
        }}
        if (hasPendingSpace) {{
          chars.push(' ');
          positions.push(pendingSpace);
          pendingSpace = null;
          hasPendingSpace = false;
        }}
        chars.push(character);
        positions.push([node, index, index + 1]);
      }}
    }};
    const blockTags = new Set(['ADDRESS','ARTICLE','ASIDE','BLOCKQUOTE','DIV','DL','FIELDSET','FIGCAPTION','FIGURE','FOOTER','FORM','H1','H2','H3','H4','H5','H6','HEADER','HR','LI','MAIN','NAV','OL','P','PRE','SECTION','TABLE','TD','TH','TR','UL']);
    const excludedTags = new Set(['HEAD','SCRIPT','STYLE','NOSCRIPT','TEMPLATE']);
    const walk = (node) => {{
      if (node.nodeType === Node.TEXT_NODE) {{ emitText(node); return; }}
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      if (excludedTags.has(node.tagName)) return;
      if (node.tagName === 'BR') {{ emitSpace(null); return; }}
      const block = blockTags.has(node.tagName);
      if (block) emitSpace(null);
      for (const child of node.childNodes) walk(child);
      if (block) emitSpace(null);
    }};
    walk(root);
    const normalize = (value) => String(value || '').replace(/\s+/gu, ' ').trim();
    const text = normalize(payload.focus.text);
    if (!text) {{ finish(false, 'empty_text'); return; }}
    const context = normalize(`${{payload.focus.before}}${{payload.focus.text}}${{payload.focus.after}}`);
    const haystack = chars.join('');
    let matchStart = haystack.indexOf(context);
    if (matchStart < 0 || haystack.indexOf(context, matchStart + 1) >= 0) {{
      finish(false, matchStart < 0 ? 'not_found' : 'ambiguous');
      return;
    }}
    const throughFocus = normalize(`${{payload.focus.before}}${{payload.focus.text}}`);
    const focusStart = matchStart + Math.max(0, throughFocus.length - text.length);
    const focusEnd = focusStart + text.length;
    const concrete = positions.slice(focusStart, focusEnd).filter(Boolean);
    if (concrete.length === 0) {{ finish(false, 'not_found'); return; }}
    const first = concrete[0];
    const last = concrete[concrete.length - 1];
    try {{
      const range = document.createRange();
      range.setStart(first[0], first[1]);
      range.setEnd(last[0], last[2]);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      (first[0].parentElement || root).scrollIntoView({{ block: 'center', inline: 'nearest' }});
      finish(true, '');
    }} catch {{
      finish(false, 'invalid_range');
    }}
  }};
  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', run, {{ once: true }});
  }} else {{
    run();
  }}
}})();"#
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AiRestoredRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiRestoredMessage {
    pub role: AiRestoredRole,
    pub content: String,
    pub sources: Vec<AiSourceLink>,
    pub source_status: Option<AgentAnswerSourceStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiQuestionRequest {
    pub request_id: u64,
    pub question: String,
    pub book_ids: Vec<String>,
    pub book_titles: Vec<(String, String)>,
    pub reference_hints: Vec<AiReferenceHint>,
    /// Compatibility bridge for the Editor's exact WebView snapshot barrier.
    /// The controller only lets this replace the matching item in
    /// `reference_hints`; it never widens the request scope.
    pub reference: Option<AiReferenceHint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AiSidebarEvent {
    Submit(AiQuestionRequest),
    Cancel { request_id: u64 },
    NewSession,
    SwitchSession { thread_id: String },
    DeleteSession { thread_id: String },
    ScopeChanged { book_ids: Vec<String> },
    OpenSource(AiSourceLink),
}

/// Published once for an accepted current request after the backend has
/// committed the final answer to the conversation store. Consumers must match
/// `request_id` against their own frozen action before creating derived data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiAnswerCompleted {
    pub request_id: u64,
    pub markdown: String,
}

/// Binds an explanation's frozen selection to its exact request generation.
/// Emitted immediately before the ordinary `AiSidebarEvent::Submit` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiExplanationSubmitted {
    pub request_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AiExplanationFailed {
    /// `None` means the fresh explanation session failed before submission.
    pub request_id: Option<u64>,
    pub error: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AiMessageRole {
    User,
    Assistant,
    Status,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AiMessage {
    id: u64,
    role: AiMessageRole,
    content: String,
    sources: Vec<AiSourceLink>,
    source_status: Option<AgentAnswerSourceStatus>,
    request_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestState {
    Idle,
    Sending { request_id: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SessionPickerState {
    threads: Vec<AiThreadOption>,
    active_thread_id: Option<String>,
    open: bool,
    busy: bool,
    error: Option<String>,
    pending_delete: Option<PendingSessionDeletion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingSessionDeletion {
    thread_id: String,
    title: String,
}

impl SessionPickerState {
    fn active_title(&self) -> &str {
        self.active_thread_id
            .as_deref()
            .and_then(|active| {
                self.threads
                    .iter()
                    .find(|thread| thread.id == active)
                    .map(|thread| thread.title.as_str())
            })
            .unwrap_or("新对话")
    }

    fn begin(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.open = false;
        self.error = None;
        self.pending_delete = None;
        true
    }

    fn apply(&mut self, active_thread_id: Option<String>, threads: Vec<AiThreadOption>) {
        self.threads = normalized_thread_options(threads);
        self.active_thread_id = active_thread_id.filter(|active| {
            self.threads
                .iter()
                .any(|thread| thread.id == active.as_str())
        });
        self.busy = false;
        self.open = false;
        self.error = None;
        self.pending_delete = None;
    }

    fn refresh(&mut self, active_thread_id: Option<String>, threads: Vec<AiThreadOption>) {
        let was_open = self.open;
        self.apply(active_thread_id, threads);
        self.open = was_open;
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.busy = false;
        self.error = Some(error.into());
    }
}

fn normalized_thread_options(threads: Vec<AiThreadOption>) -> Vec<AiThreadOption> {
    let mut seen = HashSet::new();
    threads
        .into_iter()
        .filter(|thread| !thread.id.trim().is_empty() && seen.insert(thread.id.clone()))
        .collect()
}

fn thread_scope_is_authorized(thread: &AiThreadOption, allowed: &HashSet<String>) -> bool {
    thread
        .book_ids
        .iter()
        .all(|book_id| allowed.contains(book_id))
}

fn authorized_thread_options(
    threads: Vec<AiThreadOption>,
    allowed: &HashSet<String>,
) -> Vec<AiThreadOption> {
    normalized_thread_options(threads)
        .into_iter()
        .filter(|thread| thread_scope_is_authorized(thread, allowed))
        .collect()
}

type AiReferenceKey = (String, String, Option<String>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutomaticReferenceState {
    key: AiReferenceKey,
    frozen_text: String,
    revision: Option<u64>,
    included: bool,
}

impl AutomaticReferenceState {
    fn from_reference(reference: &AiReferenceHint) -> Option<Self> {
        let frozen_text = reference.frozen_text.as_ref()?;
        if frozen_text.trim().is_empty() {
            return None;
        }
        Some(Self {
            key: reference_key(reference),
            frozen_text: frozen_text.clone(),
            revision: reference.revision,
            included: true,
        })
    }

    fn same_selection(&self, other: &Self) -> bool {
        self.key == other.key
            && self.frozen_text == other.frozen_text
            && self.revision == other.revision
    }
}

#[derive(Clone, Debug)]
struct ConversationState {
    scope: AiSidebarScope,
    reference_hints: Vec<AiReferenceHint>,
    automatic_reference: Option<AutomaticReferenceState>,
    included_references: HashSet<AiReferenceKey>,
    messages: Vec<AiMessage>,
    request: RequestState,
    next_id: u64,
}

impl ConversationState {
    fn new(scope: AiSidebarScope) -> Self {
        Self {
            scope,
            reference_hints: Vec::new(),
            automatic_reference: None,
            included_references: HashSet::new(),
            messages: Vec::new(),
            request: RequestState::Idle,
            next_id: 1,
        }
    }

    fn set_scope(&mut self, mut scope: AiSidebarScope) -> bool {
        // Reader/editor refreshes keep explicit additions that are still
        // offered by the host. The fixed current book can never be removed.
        if let (
            AiSidebarScope::Book {
                current: old_current,
                additional_ids: old_additional,
                ..
            },
            AiSidebarScope::Book {
                current,
                available,
                additional_ids,
            },
        ) = (&self.scope, &mut scope)
            && old_current.id == current.id
        {
            *additional_ids = old_additional
                .iter()
                .filter(|id| available.iter().any(|book| book.id == id.as_str()))
                .cloned()
                .collect();
        }
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.prune_references_to_scope();
        true
    }

    fn set_reference_hints(&mut self, references: Vec<AiReferenceHint>) {
        let allowed = self.scope.book_ids().into_iter().collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        self.reference_hints = references
            .into_iter()
            .filter(|reference| allowed.contains(&reference.book_id))
            .filter(valid_reference_hint_locator)
            .filter(|reference| seen.insert(reference_key(reference)))
            .collect();
        let available = self
            .reference_hints
            .iter()
            .map(reference_key)
            .collect::<HashSet<_>>();
        self.included_references
            .retain(|reference| available.contains(reference));

        let previous = self.automatic_reference.take();
        self.automatic_reference = self
            .reference_hints
            .iter()
            .find_map(AutomaticReferenceState::from_reference)
            .map(|mut automatic| {
                if let Some(previous) = previous.as_ref()
                    && previous.same_selection(&automatic)
                {
                    // A routine Reader/Editor refresh must not re-enable the
                    // same selection after the user explicitly excluded it.
                    automatic.included = previous.included;
                }
                if automatic.included
                    && !self.included_references.contains(&automatic.key)
                    && self.included_references.len() >= MAX_SELECTED_REFERENCES
                {
                    automatic.included = false;
                }
                automatic
            });
    }

    fn prune_references_to_scope(&mut self) {
        let allowed = self.scope.book_ids().into_iter().collect::<HashSet<_>>();
        self.reference_hints.retain(|reference| {
            allowed.contains(&reference.book_id) && valid_reference_hint_locator(reference)
        });
        let available = self
            .reference_hints
            .iter()
            .map(reference_key)
            .collect::<HashSet<_>>();
        self.included_references
            .retain(|reference| available.contains(reference));
        if self
            .automatic_reference
            .as_ref()
            .is_some_and(|automatic| !available.contains(&automatic.key))
        {
            self.automatic_reference = None;
        }
        for message in &mut self.messages {
            message
                .sources
                .retain(|source| allowed_source_link(&allowed, source));
        }
    }

    fn toggle_reference(&mut self, reference: &AiReferenceHint) -> bool {
        let key = reference_key(reference);
        if self
            .automatic_reference
            .as_ref()
            .is_some_and(|automatic| automatic.key == key)
        {
            if self.reference_is_included_by_key(&key) {
                // One row represents the deduplicated effective reference. A
                // click removes both ways that exact row could be included;
                // unrelated manual references remain untouched.
                self.included_references.remove(&key);
                if let Some(automatic) = self.automatic_reference.as_mut() {
                    automatic.included = false;
                }
                return true;
            }
            if self.selected_reference_count() >= MAX_SELECTED_REFERENCES {
                return false;
            }
            if let Some(automatic) = self.automatic_reference.as_mut() {
                automatic.included = true;
            }
            return true;
        }
        if self.included_references.remove(&key) {
            return true;
        }
        if self.selected_reference_count() >= MAX_SELECTED_REFERENCES
            || !self
                .reference_hints
                .iter()
                .any(|available| reference_key(available) == key)
        {
            return false;
        }
        self.included_references.insert(key)
    }

    fn reference_is_included(&self, reference: &AiReferenceHint) -> bool {
        self.reference_is_included_by_key(&reference_key(reference))
    }

    fn reference_is_included_by_key(&self, key: &AiReferenceKey) -> bool {
        self.included_references.contains(key)
            || self
                .automatic_reference
                .as_ref()
                .is_some_and(|automatic| automatic.included && automatic.key == *key)
    }

    fn selected_reference_count(&self) -> usize {
        self.included_references.len()
            + usize::from(self.automatic_reference.as_ref().is_some_and(|automatic| {
                automatic.included && !self.included_references.contains(&automatic.key)
            }))
    }

    fn begin(&mut self, question: &str) -> Option<AiQuestionRequest> {
        if !matches!(self.request, RequestState::Idle) {
            return None;
        }
        let question = question.trim();
        if question.is_empty() || question.len() > MAX_QUESTION_BYTES {
            return None;
        }
        let request_id = self.take_id();
        let message_id = self.take_id();
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::User,
            content: question.to_string(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
        });
        self.request = RequestState::Sending { request_id };
        let reference_hints = self
            .reference_hints
            .iter()
            .filter(|reference| self.reference_is_included(reference))
            .cloned()
            .collect::<Vec<_>>();
        Some(AiQuestionRequest {
            request_id,
            question: question.to_string(),
            book_ids: self.scope.book_ids(),
            book_titles: self
                .scope
                .selected_books()
                .into_iter()
                .map(|book| (book.id, book.title))
                .collect(),
            // Editor currently freezes its one selected chapter through this
            // field after its exact href/revision/request-id acknowledgement.
            reference: reference_hints.first().cloned(),
            reference_hints,
        })
    }

    /// Called only after the controller has successfully started a fresh
    /// backend conversation. Never inherit manual references or an excluded
    /// automatic selection from the previous conversation.
    fn begin_selection_explanation(
        &mut self,
        reference: AiReferenceHint,
    ) -> Option<AiQuestionRequest> {
        if self.is_sending()
            || !self.scope.book_ids().contains(&reference.book_id)
            || !valid_reference_hint_locator(&reference)
            || reference
                .frozen_text
                .as_ref()
                .is_none_or(|text| text.trim().is_empty())
        {
            return None;
        }
        self.messages.clear();
        self.included_references.clear();
        self.automatic_reference = None;
        self.set_reference_hints(vec![reference]);
        self.begin(SELECTION_EXPLANATION_QUESTION)
    }

    fn cancel(&mut self) -> Option<u64> {
        let RequestState::Sending { request_id } = self.request else {
            return None;
        };
        self.discard_answer_message(request_id);
        self.request = RequestState::Idle;
        let message_id = self.take_id();
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::Status,
            content: "已取消本次回答。".to_string(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
        });
        Some(request_id)
    }

    #[allow(dead_code)]
    fn append_delta(&mut self, request_id: u64, delta: &str) -> bool {
        if !self.is_current(request_id) || delta.is_empty() {
            return false;
        }
        if let Some(message) = self.messages.iter_mut().rev().find(|message| {
            message.request_id == Some(request_id) && message.role == AiMessageRole::Assistant
        }) {
            if message.content.len().saturating_add(delta.len()) > MAX_MESSAGE_BYTES {
                return false;
            }
            message.content.push_str(delta);
        } else {
            if delta.len() > MAX_MESSAGE_BYTES {
                return false;
            }
            let message_id = self.take_id();
            self.messages.push(AiMessage {
                id: message_id,
                role: AiMessageRole::Assistant,
                content: delta.to_string(),
                sources: Vec::new(),
                source_status: None,
                request_id: Some(request_id),
            });
        }
        true
    }

    #[allow(dead_code)]
    fn reset_answer(&mut self, request_id: u64) -> bool {
        if !self.is_current(request_id) {
            return false;
        }
        self.discard_answer_message(request_id)
    }

    #[allow(dead_code)]
    fn finish(
        &mut self,
        request_id: u64,
        sources: Vec<AiSourceLink>,
        source_status: AgentAnswerSourceStatus,
    ) -> bool {
        if !self.is_current(request_id) {
            return false;
        }
        let allowed = self.scope.book_ids().into_iter().collect::<HashSet<_>>();
        let sources = sources
            .into_iter()
            .filter(|source| allowed_source_link(&allowed, source))
            .collect();
        if let Some(message) = self.messages.iter_mut().rev().find(|message| {
            message.request_id == Some(request_id) && message.role == AiMessageRole::Assistant
        }) {
            message.sources = sources;
            message.source_status = Some(source_status);
        } else {
            let message_id = self.take_id();
            self.messages.push(AiMessage {
                id: message_id,
                role: AiMessageRole::Assistant,
                content: "模型没有返回文本。".to_string(),
                sources,
                source_status: Some(source_status),
                request_id: Some(request_id),
            });
        }
        self.request = RequestState::Idle;
        true
    }

    #[allow(dead_code)]
    fn fail(&mut self, request_id: u64, message: impl Into<String>) -> bool {
        if !self.is_current(request_id) {
            return false;
        }
        self.discard_answer_message(request_id);
        self.request = RequestState::Idle;
        let message_id = self.take_id();
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::Error,
            content: message.into(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
        });
        true
    }

    fn discard_answer_message(&mut self, request_id: u64) -> bool {
        let before = self.messages.len();
        self.messages.retain(|message| {
            message.request_id != Some(request_id) || message.role != AiMessageRole::Assistant
        });
        self.messages.len() != before
    }

    #[allow(dead_code)]
    fn is_current(&self, request_id: u64) -> bool {
        self.request == RequestState::Sending { request_id }
    }

    fn is_sending(&self) -> bool {
        matches!(self.request, RequestState::Sending { .. })
    }

    fn retain_current_request_messages(&mut self) {
        match self.request {
            RequestState::Sending { request_id } => self
                .messages
                .retain(|message| message.request_id == Some(request_id)),
            RequestState::Idle => self.messages.clear(),
        }
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

fn reference_key(reference: &AiReferenceHint) -> AiReferenceKey {
    (
        reference.book_id.clone(),
        reference.unit_id.clone(),
        reference
            .locator
            .as_ref()
            .and_then(|locator| serde_json::to_string(locator).ok()),
    )
}

fn valid_reference_hint_locator(reference: &AiReferenceHint) -> bool {
    reference.locator.as_ref().is_none_or(|locator| {
        locator.book_id == reference.book_id
            && locator.unit_id == reference.unit_id
            && locator.validate().is_ok()
    })
}

fn allowed_source_link(allowed: &HashSet<String>, source: &AiSourceLink) -> bool {
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

fn should_show_no_knowledge_base_source_warning(message: &AiMessage) -> bool {
    message.role == AiMessageRole::Assistant
        && message.source_status == Some(AgentAnswerSourceStatus::NoVerifiedSources)
}

/// A short basis badge shown above a grounded answer, telling the user whether
/// the answer came from the current books, a web search, or both.
fn source_basis_badge(message: &AiMessage) -> Option<(&'static str, &'static str)> {
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

fn selectable_message_html(message: &str) -> String {
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
fn selectable_message_text(
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
        TextView::markdown(
            SharedString::from(format!("{id}/markdown")),
            display,
            window,
            cx,
        )
        .style(TextViewStyle::default().paragraph_gap(rems(0.5)))
    } else {
        TextView::html(
            SharedString::from(format!("{id}/plain")),
            display,
            window,
            cx,
        )
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

fn message_text_width(sidebar_width: Pixels, rem_size: Pixels) -> Pixels {
    // Two horizontal padding pairs (3 rem), the copy-button gap (0.25 rem),
    // the 24 px copy column and 4 px for borders/rounding. TextView 0.5.1's
    // clipped list rows lose short text on Windows when width is only a percent
    // inside a flex item. Resolve it before laying out the Markdown tree.
    (sidebar_width - rems(3.25).to_pixels(rem_size) - px(28.)).max(px(0.))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResponsiveSidebar {
    collapsed: bool,
    narrow: bool,
    manual_override: bool,
}

impl ResponsiveSidebar {
    fn new(width: Pixels) -> Self {
        let narrow = width < px(AI_SIDEBAR_NARROW_THRESHOLD);
        Self {
            collapsed: narrow,
            narrow,
            manual_override: false,
        }
    }

    fn resize(&mut self, width: Pixels) -> bool {
        let narrow = width < px(AI_SIDEBAR_NARROW_THRESHOLD);
        if narrow == self.narrow {
            return false;
        }
        self.narrow = narrow;
        if self.manual_override || self.collapsed == narrow {
            return false;
        }
        self.collapsed = narrow;
        true
    }

    fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
        self.manual_override = true;
    }
}

pub(super) struct AiSidebar {
    conversation: ConversationState,
    sessions: SessionPickerState,
    selection_explanation_request: Option<u64>,
    pending_scope_sync: bool,
    layout: ResponsiveSidebar,
    expanded_width: Pixels,
    endpoint_status: AiEndpointStatus,
    input: Entity<InputState>,
    _endpoint_refresh_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AiSidebarEvent> for AiSidebar {}
impl EventEmitter<AiAnswerCompleted> for AiSidebar {}
impl EventEmitter<AiExplanationSubmitted> for AiSidebar {}
impl EventEmitter<AiExplanationFailed> for AiSidebar {}

impl AiSidebar {
    pub(super) fn new(
        scope: AiSidebarScope,
        services: Arc<AppServices>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("向这些图书提问…（Ctrl+Enter 发送）")
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.observe_window_bounds(window, |this, window, cx| {
                if this.layout.resize(window.viewport_size().width) {
                    cx.notify();
                }
            }),
        ];
        let endpoint_status = AiEndpointStatus::current(&services);
        let endpoint_services = Arc::clone(&services);
        let endpoint_refresh_task = cx.spawn(async move |view, cx| {
            loop {
                Timer::after(AI_ENDPOINT_REFRESH_INTERVAL).await;
                let endpoint_status = AiEndpointStatus::current(&endpoint_services);
                if view
                    .update(cx, |this, cx| {
                        if this.endpoint_status != endpoint_status {
                            this.endpoint_status = endpoint_status;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            conversation: ConversationState::new(scope),
            sessions: SessionPickerState::default(),
            selection_explanation_request: None,
            pending_scope_sync: false,
            layout: ResponsiveSidebar::new(window.viewport_size().width),
            expanded_width: px(AI_SIDEBAR_WIDTH),
            endpoint_status,
            input,
            _endpoint_refresh_task: endpoint_refresh_task,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn set_expanded_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        let width = clamp_ai_sidebar_width(width);
        if self.expanded_width != width {
            self.expanded_width = width;
            cx.notify();
        }
    }

    pub(super) fn expanded_width(&self) -> Pixels {
        self.expanded_width
    }

    pub(super) fn is_collapsed(&self) -> bool {
        self.layout.collapsed
    }

    pub(super) fn set_scope(&mut self, scope: AiSidebarScope, cx: &mut Context<Self>) {
        if self.conversation.set_scope(scope) {
            self.begin_scope_sync(cx);
        }
    }

    pub(super) fn authorized_book_ids(&self) -> Vec<String> {
        self.conversation.scope.book_ids()
    }

    pub(super) fn set_reference_hint(
        &mut self,
        reference: Option<AiReferenceHint>,
        cx: &mut Context<Self>,
    ) {
        let references = reference.into_iter().collect::<Vec<_>>();
        if self.conversation.reference_hints != references {
            self.conversation.set_reference_hints(references);
            cx.notify();
        }
    }

    pub(super) fn set_reference_hints(
        &mut self,
        references: Vec<AiReferenceHint>,
        cx: &mut Context<Self>,
    ) {
        if self.conversation.reference_hints != references {
            self.conversation.set_reference_hints(references);
            cx.notify();
        }
    }

    pub(super) fn begin_session_operation(&mut self, cx: &mut Context<Self>) -> bool {
        if self.conversation.is_sending() || self.pending_scope_sync || !self.sessions.begin() {
            return false;
        }
        cx.notify();
        true
    }

    pub(super) fn show_explanation_error(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.layout.collapsed = false;
        self.layout.manual_override = true;
        // A rejected context-menu action must not clear another operation's
        // busy flag or cancel the answer currently visible in the sidebar.
        self.sessions.error = Some(error.into());
        cx.notify();
    }

    pub(super) fn begin_selection_explanation(&mut self, cx: &mut Context<Self>) -> bool {
        self.layout.collapsed = false;
        self.layout.manual_override = true;
        if !self.begin_session_operation(cx) {
            self.show_explanation_error("AI 正在处理当前请求，请完成或取消后再使用 AI 解释。", cx);
            return false;
        }
        true
    }

    pub(super) fn complete_selection_explanation(
        &mut self,
        threads: Vec<AiThreadOption>,
        reference: AiReferenceHint,
        cx: &mut Context<Self>,
    ) {
        if !self.apply_session(None, threads, Vec::new(), cx)
            || self.sessions.busy
            || self.pending_scope_sync
        {
            self.reject_selection_explanation(
                "AI 图书范围正在更新，请更新完成后再次使用 AI 解释。",
                cx,
            );
            return;
        }
        let Some(request) = self.conversation.begin_selection_explanation(reference) else {
            self.reject_selection_explanation(
                "所选文本已不在当前 AI 范围内，请重新选择后再试。",
                cx,
            );
            return;
        };
        self.selection_explanation_request = Some(request.request_id);
        cx.emit(AiExplanationSubmitted {
            request_id: request.request_id,
        });
        cx.emit(AiSidebarEvent::Submit(request));
        cx.notify();
    }

    fn reject_selection_explanation(&mut self, error: impl Into<String>, cx: &mut Context<Self>) {
        let error = error.into();
        self.show_explanation_error(error.clone(), cx);
        cx.emit(AiExplanationFailed {
            request_id: None,
            error,
        });
    }

    pub(super) fn fail_selection_explanation_session(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let error = error.into();
        self.fail_session_operation(error.clone(), cx);
        cx.emit(AiExplanationFailed {
            request_id: None,
            error,
        });
    }

    fn fail_selection_explanation_request(
        &mut self,
        request_id: u64,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if self.selection_explanation_request == Some(request_id) {
            self.selection_explanation_request = None;
            cx.emit(AiExplanationFailed {
                request_id: Some(request_id),
                error,
            });
        }
    }

    fn begin_scope_sync(&mut self, cx: &mut Context<Self>) {
        if self.conversation.is_sending() || self.sessions.busy {
            self.pending_scope_sync = true;
            cx.notify();
            return;
        }

        let book_ids = self.conversation.scope.book_ids();
        let allowed = book_ids.iter().cloned().collect::<HashSet<_>>();
        let active_is_unauthorized =
            self.sessions
                .active_thread_id
                .as_deref()
                .is_some_and(|active| {
                    self.sessions
                        .threads
                        .iter()
                        .find(|thread| thread.id == active)
                        .is_none_or(|thread| !thread_scope_is_authorized(thread, &allowed))
                });
        if active_is_unauthorized {
            self.sessions.active_thread_id = None;
            self.conversation.messages.clear();
            self.conversation.included_references.clear();
        }

        self.pending_scope_sync = false;
        if self.sessions.begin() {
            cx.emit(AiSidebarEvent::ScopeChanged { book_ids });
        }
        cx.notify();
    }

    fn resume_pending_scope_sync(&mut self, cx: &mut Context<Self>) {
        if self.pending_scope_sync && !self.conversation.is_sending() && !self.sessions.busy {
            self.begin_scope_sync(cx);
        }
    }

    /// Replaces the selected persisted conversation and its already-authorized
    /// transcript. Restoration is ignored while a request is in flight.
    pub(super) fn apply_session(
        &mut self,
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
        messages: Vec<AiRestoredMessage>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.conversation.is_sending() {
            return false;
        }
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let requested_active = active_thread_id.clone();
        self.sessions.apply(
            active_thread_id,
            authorized_thread_options(threads, &allowed),
        );
        self.conversation.included_references.clear();
        self.conversation.messages.clear();
        let selected_active = self.sessions.active_thread_id.as_deref();
        if requested_active.as_deref() == selected_active {
            for message in messages {
                if message.content.is_empty() || message.content.len() > MAX_MESSAGE_BYTES {
                    continue;
                }
                let id = self.conversation.take_id();
                self.conversation.messages.push(AiMessage {
                    id,
                    role: match message.role {
                        AiRestoredRole::User => AiMessageRole::User,
                        AiRestoredRole::Assistant => AiMessageRole::Assistant,
                    },
                    content: message.content,
                    sources: message
                        .sources
                        .into_iter()
                        .filter(|source| allowed_source_link(&allowed, source))
                        .collect(),
                    source_status: message.source_status,
                    request_id: None,
                });
            }
        }
        cx.notify();
        self.resume_pending_scope_sync(cx);
        true
    }

    pub(super) fn refresh_sessions(
        &mut self,
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
        cx: &mut Context<Self>,
    ) {
        let previous_active = self.sessions.active_thread_id.clone();
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        self.sessions.refresh(
            active_thread_id,
            authorized_thread_options(threads, &allowed),
        );
        if previous_active != self.sessions.active_thread_id {
            self.conversation.retain_current_request_messages();
            self.conversation.included_references.clear();
        }
        cx.notify();
        self.resume_pending_scope_sync(cx);
    }

    pub(super) fn fail_session_operation(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.sessions.fail(error);
        cx.notify();
        self.resume_pending_scope_sync(cx);
    }

    /// Backend adapters call this for every streamed text delta. A false
    /// return means the request was cancelled or superseded.
    #[allow(dead_code)]
    pub(super) fn append_answer_delta(
        &mut self,
        request_id: u64,
        delta: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.conversation.append_delta(request_id, delta);
        if changed {
            cx.notify();
        }
        changed
    }

    /// Removes provisional deltas after a streamed response turns into a tool
    /// call or before an uncommitted/error response is replaced.
    pub(super) fn reset_answer(&mut self, request_id: u64, cx: &mut Context<Self>) -> bool {
        let changed = self.conversation.reset_answer(request_id);
        if changed {
            cx.notify();
        }
        changed
    }

    /// The caller supplies the already-persisted final Markdown, never a
    /// provisional stream buffer or an answer awaiting citation validation.
    pub(super) fn finish_persisted_answer(
        &mut self,
        request_id: u64,
        markdown: String,
        sources: Vec<AiSourceLink>,
        source_status: AgentAnswerSourceStatus,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.conversation.finish(request_id, sources, source_status);
        if changed {
            if self.selection_explanation_request == Some(request_id) {
                self.selection_explanation_request = None;
            }
            cx.emit(AiAnswerCompleted {
                request_id,
                markdown,
            });
            cx.notify();
        }
        self.resume_pending_scope_sync(cx);
        changed
    }

    #[allow(dead_code)]
    pub(super) fn fail_answer(
        &mut self,
        request_id: u64,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let error = error.into();
        let changed = self.conversation.fail(request_id, error.clone());
        if changed {
            self.fail_selection_explanation_request(request_id, error, cx);
            cx.notify();
        }
        self.resume_pending_scope_sync(cx);
        changed
    }

    /// Called before the owner releases a Reader/Editor WebView or removes the
    /// library window. The returned ID lets a future adapter cancel its task;
    /// stale callbacks are rejected by the state machine either way.
    pub(super) fn cancel_for_window_close(&mut self, cx: &mut Context<Self>) -> Option<u64> {
        let request_id = self.conversation.cancel();
        if let Some(request_id) = request_id {
            cx.emit(AiSidebarEvent::Cancel { request_id });
            self.fail_selection_explanation_request(request_id, "本次 AI 解释已取消。".into(), cx);
            cx.notify();
            Some(request_id)
        } else {
            None
        }
    }

    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::PressEnter { secondary: true }) {
            self.submit(window, cx);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync {
            return;
        }
        let question = self.input.read(cx).value().to_string();
        let Some(request) = self.conversation.begin(&question) else {
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(AiSidebarEvent::Submit(request));
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.conversation.cancel() else {
            return;
        };
        cx.emit(AiSidebarEvent::Cancel { request_id });
        self.fail_selection_explanation_request(request_id, "本次 AI 解释已取消。".into(), cx);
        cx.notify();
    }

    fn toggle_session_picker(&mut self, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync || self.conversation.is_sending() {
            return;
        }
        self.sessions.open = !self.sessions.open;
        self.sessions.error = None;
        self.sessions.pending_delete = None;
        cx.notify();
    }

    fn new_session(&mut self, cx: &mut Context<Self>) {
        if !self.begin_session_operation(cx) {
            return;
        }
        cx.emit(AiSidebarEvent::NewSession);
    }

    fn switch_session(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.sessions.active_thread_id.as_deref() == Some(thread_id) {
            self.sessions.open = false;
            cx.notify();
            return;
        }
        if !self.begin_session_operation(cx) {
            return;
        }
        cx.emit(AiSidebarEvent::SwitchSession {
            thread_id: thread_id.to_string(),
        });
    }

    fn confirm_delete_session(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync || self.conversation.is_sending() {
            return;
        }
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let Some(thread) =
            self.sessions.threads.iter().find(|thread| {
                thread.id == thread_id && thread_scope_is_authorized(thread, &allowed)
            })
        else {
            self.sessions.error = Some("该会话已不存在或不在当前图书范围内。".to_string());
            cx.notify();
            return;
        };
        self.sessions.pending_delete = Some(PendingSessionDeletion {
            thread_id: thread.id.clone(),
            title: thread.title.clone(),
        });
        self.sessions.error = None;
        cx.notify();
    }

    fn cancel_delete_session(&mut self, cx: &mut Context<Self>) {
        if self.sessions.pending_delete.take().is_some() {
            cx.notify();
        }
    }

    fn delete_session(&mut self, thread_id: &str, cx: &mut Context<Self>) -> bool {
        if !self.begin_session_operation(cx) {
            return false;
        }
        cx.emit(AiSidebarEvent::DeleteSession {
            thread_id: thread_id.to_string(),
        });
        true
    }

    fn toggle_reference(&mut self, reference: &AiReferenceHint, cx: &mut Context<Self>) {
        if self.conversation.toggle_reference(reference) {
            cx.notify();
        }
    }

    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.layout.toggle();
        cx.notify();
    }

    fn add_book(&mut self, book_id: &str, cx: &mut Context<Self>) {
        if self.conversation.scope.add_book(book_id) {
            self.begin_scope_sync(cx);
        }
    }

    fn remove_book(&mut self, book_id: &str, cx: &mut Context<Self>) {
        if self.conversation.scope.remove_book(book_id) {
            self.conversation.prune_references_to_scope();
            self.begin_scope_sync(cx);
        }
    }

    fn render_endpoint_status(&self) -> gpui::AnyElement {
        let warning = self.endpoint_status.is_warning();
        let (background, foreground, icon) = if warning {
            (rgb(0xf8e2de), rgb(DANGER), IconName::TriangleAlert)
        } else {
            (rgb(0xe3efe5), rgb(0x376441), IconName::CircleCheck)
        };
        div()
            .v_flex()
            .min_h(px(34.))
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(background)
            .text_xs()
            .text_color(foreground)
            .child(
                div()
                    .h_flex()
                    .min_w(px(0.))
                    .gap_1p5()
                    .child(Icon::new(icon).small())
                    .child("OpenAI-compatible"),
            )
            .child(
                div()
                    .w_full()
                    .font_semibold()
                    .child(self.endpoint_status.label()),
            )
            .into_any_element()
    }

    fn render_collapsed(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let warning = self.endpoint_status.is_warning();
        let (status_color, status_text) = if warning {
            (DANGER, "远")
        } else {
            (0x376441, "本")
        };
        div()
            .v_flex()
            .w(px(AI_SIDEBAR_COLLAPSED_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .border_l_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SIDEBAR))
            .py_3()
            .child(
                Button::new("ai-sidebar-expand")
                    .ghost()
                    .icon(IconName::PanelRightOpen)
                    .tooltip("展开 AI 问答")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle_collapsed(cx));
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .text_color(rgb(ACCENT))
                    .child(Icon::new(IconName::Bot).small()),
            )
            .child(
                div()
                    .v_flex()
                    .mt_3()
                    .items_center()
                    .gap_1()
                    .text_color(rgb(status_color))
                    .child(
                        Icon::new(if warning {
                            IconName::TriangleAlert
                        } else {
                            IconName::CircleCheck
                        })
                        .small(),
                    )
                    .child(div().text_xs().font_semibold().child(status_text)),
            )
            .into_any_element()
    }

    fn render_scope(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let selected = self.conversation.scope.selected_books();
        let is_book_scope = matches!(self.conversation.scope, AiSidebarScope::Book { .. });
        let chips = selected
            .iter()
            .enumerate()
            .take(4)
            .map(|(index, book)| {
                let removable = is_book_scope && index > 0;
                let book_id = book.id.clone();
                Button::new(SharedString::from(format!("ai-scope-book-{book_id}")))
                    .xsmall()
                    .ghost()
                    .label(book.title.clone())
                    .when(removable, |button| button.icon(IconName::Close))
                    .when(removable, |button| {
                        let view = view.clone();
                        button.on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.remove_book(&book_id, cx));
                        })
                    })
            })
            .collect::<Vec<_>>();
        let hidden = selected.len().saturating_sub(4);
        let books_to_add = self.conversation.scope.available_books_to_add();
        let add_button = (!books_to_add.is_empty()).then(|| {
            let menu_view = view.clone();
            Button::new("ai-scope-add-book")
                .xsmall()
                .outline()
                .icon(IconName::Plus)
                .label("添加其它图书")
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for book in &books_to_add {
                        let view = menu_view.clone();
                        let book_id = book.id.clone();
                        menu = menu.item(PopupMenuItem::new(book.title.clone()).on_click(
                            move |_, _, cx| {
                                view.update(cx, |this, cx| this.add_book(&book_id, cx));
                            },
                        ));
                    }
                    menu
                })
        });

        div()
            .v_flex()
            .gap_2()
            .px_3()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("图书范围")
                    .child(format!("{} 本", self.conversation.scope.book_ids().len())),
            )
            .child(div().h_flex().flex_wrap().gap_1().children(chips))
            .when(hidden > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!("另有 {hidden} 本图书")),
                )
            })
            .when_some(add_button, |this, button| this.child(button))
            .into_any_element()
    }

    fn render_references(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.conversation.reference_hints.is_empty() {
            return None;
        }
        let view = cx.entity().clone();
        let selected_count = self.conversation.selected_reference_count();
        let selection_full = selected_count >= MAX_SELECTED_REFERENCES;
        let reference_count = self.conversation.reference_hints.len();
        let rows = self
            .conversation
            .reference_hints
            .iter()
            .enumerate()
            .map(|(index, reference)| {
                let included = self.conversation.reference_is_included(reference);
                let automatic = self
                    .conversation
                    .automatic_reference
                    .as_ref()
                    .is_some_and(|automatic| automatic.key == reference_key(reference));
                let reference_for_click = reference.clone();
                let view = view.clone();
                div()
                    .h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_sm()
                            .text_color(rgb(INK))
                            .child(reference.label.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("ai-toggle-reference-{index}")))
                            .xsmall()
                            .when(included, |button| button.primary())
                            .when(!included, |button| button.outline())
                            .label(if included { "已引用" } else { "引用" })
                            .disabled(selection_full && !included)
                            .tooltip(if selection_full && !included {
                                format!("最多选择 {MAX_SELECTED_REFERENCES} 个引用")
                            } else if automatic && included {
                                "取消本次高亮引用".to_string()
                            } else if automatic {
                                "重新加入本次高亮引用".to_string()
                            } else if included {
                                "取消引用".to_string()
                            } else {
                                "加入本次问题".to_string()
                            })
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.toggle_reference(&reference_for_click, cx)
                                });
                            }),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .v_flex()
                .gap_2()
                .py_2()
                .border_b_1()
                .border_color(rgb(BORDER))
                .bg(rgb(0xfaf8f4))
                .child(
                    div()
                        .h_flex()
                        .justify_between()
                        .gap_2()
                        .px_3()
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(
                                    Icon::new(IconName::BookOpen)
                                        .small()
                                        .text_color(rgb(ACCENT)),
                                )
                                .child("可选引用"),
                        )
                        .child(
                            div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "已选 {selected_count}/{}",
                                MAX_SELECTED_REFERENCES
                            )),
                        ),
                )
                .child(
                    div()
                        .v_flex()
                        .max_h(reference_list_max_height(reference_count))
                        .overflow_y_scrollbar()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn render_session_picker(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.sessions.open && !self.sessions.busy && self.sessions.error.is_none() {
            return None;
        }

        let view = cx.entity().clone();
        let active_thread_id = self.sessions.active_thread_id.as_deref();
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let session_locked =
            self.sessions.busy || self.pending_scope_sync || self.conversation.is_sending();
        let pending_delete = self.sessions.pending_delete.clone();
        let rows = self
            .sessions
            .threads
            .iter()
            .filter(|thread| thread_scope_is_authorized(thread, &allowed))
            .map(|thread| {
                let thread_id = thread.id.clone();
                let thread_title = thread.title.clone();
                let active = active_thread_id == Some(thread.id.as_str());
                let switch_view = view.clone();
                let delete_view = view.clone();
                let delete_thread_id = thread.id.clone();
                let delete_title = thread_title.clone();
                div()
                    .h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!(
                            "ai-session-option-{}",
                            thread.id
                        )))
                        .ghost()
                        .small()
                        .flex_1()
                        .min_w(px(0.))
                        .when(active, |this| this.icon(IconName::Check))
                        .label(thread_title.clone())
                        .tooltip(thread_title)
                        .disabled(session_locked || active)
                        .on_click(move |_, _, cx| {
                            switch_view.update(cx, |this, cx| {
                                this.switch_session(&thread_id, cx);
                            });
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "ai-session-delete-{}",
                            thread.id
                        )))
                        .ghost()
                        .small()
                        .icon(IconName::Delete)
                        .tooltip(format!("删除会话“{delete_title}”"))
                        .disabled(session_locked)
                        .on_click(move |_, _, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.confirm_delete_session(&delete_thread_id, cx);
                            });
                        }),
                    )
            })
            .collect::<Vec<_>>();

        Some(
            div()
                .v_flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(rgb(BORDER))
                .bg(rgb(SURFACE))
                .when(self.sessions.busy, |this| {
                    this.child(
                        div()
                            .h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(Icon::new(IconName::LoaderCircle).xsmall())
                            .child("正在处理会话…"),
                    )
                })
                .when_some(self.sessions.error.clone(), |this, error| {
                    this.child(div().text_xs().text_color(rgb(DANGER)).child(error))
                })
                .when(self.sessions.open, |this| {
                    this.when_some(pending_delete, |this, pending| {
                        let cancel_view = view.clone();
                        let delete_view = view.clone();
                        let thread_id = pending.thread_id.clone();
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .gap_2()
                                .p_3()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(DANGER))
                                .bg(rgb(0xfff3f0))
                                .child(div().font_semibold().child("删除 AI 会话？"))
                                .child(div().text_sm().child(format!("会话：“{}”", pending.title)))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(MUTED))
                                        .child("问题、回答和引用都会被永久删除，且无法恢复。"),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("ai-session-delete-cancel")
                                                .ghost()
                                                .small()
                                                .label("取消")
                                                .on_click(move |_, _, cx| {
                                                    cancel_view.update(cx, |this, cx| {
                                                        this.cancel_delete_session(cx);
                                                    });
                                                }),
                                        )
                                        .child(
                                            Button::new("ai-session-delete-confirm")
                                                .small()
                                                .danger()
                                                .label("永久删除")
                                                .on_click(move |_, _, cx| {
                                                    delete_view.update(cx, |this, cx| {
                                                        this.delete_session(&thread_id, cx);
                                                    });
                                                }),
                                        ),
                                ),
                        )
                    })
                    .when(self.sessions.pending_delete.is_none(), |this| {
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .max_h(px(184.))
                                .overflow_y_scrollbar()
                                .when(rows.is_empty(), |this| {
                                    this.child(
                                        div().px_2().py_3().text_xs().text_color(rgb(MUTED)).child(
                                            "还没有历史会话。发送第一条消息后会保存在这里。",
                                        ),
                                    )
                                })
                                .children(rows),
                        )
                    })
                })
                .into_any_element(),
        )
    }

    fn render_message(
        &self,
        message: &AiMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let user = message.role == AiMessageRole::User;
        let show_no_source_warning = should_show_no_knowledge_base_source_warning(message);
        let (background, foreground): (gpui::Hsla, gpui::Hsla) = match message.role {
            AiMessageRole::User => (rgb(ACCENT).into(), white()),
            AiMessageRole::Assistant => (white(), rgb(INK).into()),
            AiMessageRole::Status => (rgb(0xe9e5de).into(), rgb(MUTED).into()),
            AiMessageRole::Error => (rgb(0xf7e1df).into(), rgb(0x9f302c).into()),
        };
        let sources = message
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let source = source.clone();
                let source_event = source.clone();
                let mut label = if source.label.trim().is_empty() {
                    format!("来源 {}", index + 1)
                } else {
                    source.label.clone()
                };
                if source.stale {
                    label.push_str("（引用已失效）");
                }
                Button::new(SharedString::from(format!(
                    "ai-message-source-{}-{index}",
                    message.id
                )))
                .xsmall()
                .ghost()
                .icon(IconName::BookOpen)
                .label(label)
                .tooltip(if source.stale {
                    "该来源对应的文档或内容版本已变化，无法继续跳转。".to_string()
                } else {
                    source.quote.unwrap_or_else(|| "打开来源".to_string())
                })
                .on_click({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |_, cx| {
                            cx.emit(AiSidebarEvent::OpenSource(source_event.clone()));
                        });
                    }
                })
            })
            .collect::<Vec<_>>();
        let message_max_width = (self.expanded_width - px(24.)).max(px(0.));

        div()
            .v_flex()
            .w_full()
            .min_w(px(0.))
            .items_start()
            .when(user, |this| this.items_end())
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .min_w(px(0.))
                    .max_w(message_max_width)
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if user { rgb(ACCENT) } else { rgb(BORDER) })
                    .bg(background)
                    .text_sm()
                    .text_color(foreground)
                    .when(show_no_source_warning, |this| {
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .gap_1()
                                .px_2()
                                .py_2()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(0xe3c26f))
                                .bg(rgb(0xfff4d6))
                                .text_color(rgb(0x6f5314))
                                .child(
                                    div()
                                        .h_flex()
                                        .gap_1()
                                        .font_semibold()
                                        .child(Icon::new(IconName::TriangleAlert).small())
                                        .child(NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .line_height(gpui::relative(1.5))
                                        .child(NO_KNOWLEDGE_BASE_SOURCE_WARNING_BODY),
                                ),
                        )
                    })
                    .when_some(source_basis_badge(message), |this, (title, body)| {
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .gap_0p5()
                                .px_2()
                                .py_2()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(0xb9cfe6))
                                .bg(rgb(0xe9f1fa))
                                .text_color(rgb(0x2c5c86))
                                .child(
                                    div()
                                        .h_flex()
                                        .gap_1()
                                        .font_semibold()
                                        .child(Icon::new(IconName::CircleCheck).small())
                                        .child(title),
                                )
                                .child(
                                    div().text_xs().line_height(gpui::relative(1.5)).child(body),
                                ),
                        )
                    })
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .items_start()
                            .gap_1()
                            .line_height(gpui::relative(1.5))
                            .child(div().min_w(px(0.)).flex_1().child(selectable_message_text(
                                SharedString::from(format!("ai-message-text-{}", message.id)),
                                message.role,
                                &message.content,
                                message_text_width(self.expanded_width, window.rem_size()),
                                window,
                                cx,
                            )))
                            .child(
                                div().w(px(24.)).flex_none().child(
                                    Clipboard::new(SharedString::from(format!(
                                        "ai-message-copy-{}",
                                        message.id
                                    )))
                                    .value(message.content.clone()),
                                ),
                            ),
                    )
                    .when(!sources.is_empty(), |this| {
                        this.child(div().v_flex().items_start().gap_1().children(sources))
                    }),
            )
            .into_any_element()
    }
}

impl Render for AiSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.layout.collapsed {
            return self.render_collapsed(cx);
        }

        let view = cx.entity().clone();
        let collapse_view = view.clone();
        let sessions_view = view.clone();
        let new_session_view = view.clone();
        let sending = self.conversation.is_sending();
        let session_busy = self.sessions.busy || self.pending_scope_sync;
        let active_session_title = self.sessions.active_title().to_string();
        let session_picker_tooltip = format!("切换会话 · {}", self.conversation.scope.label());
        let question_empty = self.input.read(cx).value().trim().is_empty();
        let messages = self
            .conversation
            .messages
            .iter()
            .map(|message| self.render_message(message, window, cx))
            .collect::<Vec<_>>();
        let send_view = view.clone();
        let cancel_view = view.clone();

        div()
            .v_flex()
            .w(self.expanded_width)
            .h_full()
            .min_h(px(0.))
            .flex_none()
            .border_l_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SIDEBAR))
            .child(
                div()
                    .h_flex()
                    .h(px(52.))
                    .flex_none()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .gap_2()
                            .text_color(rgb(INK))
                            .child(Icon::new(IconName::Bot).small().text_color(rgb(ACCENT)))
                            .child(
                                div()
                                    .v_flex()
                                    .min_w(px(0.))
                                    .child(div().text_sm().font_semibold().child("AI 问答"))
                                    .child(
                                        div()
                                            .max_w(px(250.))
                                            .truncate()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(active_session_title),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .child(
                                Button::new("ai-session-list")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::GalleryVerticalEnd)
                                    .tooltip(session_picker_tooltip)
                                    .disabled(sending || session_busy)
                                    .on_click(move |_, _, cx| {
                                        sessions_view
                                            .update(cx, |this, cx| this.toggle_session_picker(cx));
                                    }),
                            )
                            .child(
                                Button::new("ai-new-session")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Plus)
                                    .tooltip("新建会话")
                                    .disabled(sending || session_busy)
                                    .on_click(move |_, _, cx| {
                                        new_session_view
                                            .update(cx, |this, cx| this.new_session(cx));
                                    }),
                            )
                            .child(
                                Button::new("ai-sidebar-collapse")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::PanelRightClose)
                                    .tooltip("收起 AI 问答")
                                    .on_click(move |_, _, cx| {
                                        collapse_view
                                            .update(cx, |this, cx| this.toggle_collapsed(cx));
                                    }),
                            ),
                    ),
            )
            .when_some(self.render_session_picker(cx), |this, picker| {
                this.child(picker)
            })
            .child(self.render_endpoint_status())
            .child(self.render_scope(cx))
            .when_some(self.render_references(cx), |this, references| {
                this.child(references)
            })
            .child(
                div()
                    .id("ai-message-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scrollbar()
                    .p_3()
                    .when(messages.is_empty(), |this| {
                        this.child(
                            div()
                                .v_flex()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .py_8()
                                .text_center()
                                .text_color(rgb(MUTED))
                                .child(Icon::new(IconName::Bot).large())
                                .child(div().text_sm().child("围绕当前图书范围提问"))
                                .child(div().text_xs().line_height(gpui::relative(1.5)).child(
                                    "回答将显示可点击来源；图书内容只通过宿主授权的只读工具读取。",
                                )),
                        )
                    })
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .min_w(px(0.))
                            .gap_3()
                            .children(messages),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_none()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(Input::new(&self.input).disabled(sending || session_busy))
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .gap_2()
                            .child(div().text_xs().text_color(rgb(MUTED)).child(if sending {
                                "正在生成，可随时取消"
                            } else if session_busy {
                                "正在切换会话"
                            } else {
                                "Ctrl+Enter 发送"
                            }))
                            .when(!sending, |this| {
                                this.child(
                                    Button::new("ai-send")
                                        .small()
                                        .primary()
                                        .icon(IconName::ArrowUp)
                                        .label("发送")
                                        .disabled(question_empty || session_busy)
                                        .on_click(move |_, window, cx| {
                                            send_view
                                                .update(cx, |this, cx| this.submit(window, cx));
                                        }),
                                )
                            })
                            .when(sending, |this| {
                                this.child(
                                    Button::new("ai-cancel")
                                        .small()
                                        .outline()
                                        .icon(IconName::CircleX)
                                        .label("取消")
                                        .on_click(move |_, _, cx| {
                                            cancel_view.update(cx, |this, cx| this.cancel(cx));
                                        }),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use gpui_component::Root;
    use moye_epub_editor::document::{
        Block, BlockDocument, ContentUnit, ContentUnitKind, NormalizedRect,
    };
    use std::{cell::RefCell, rc::Rc};

    fn book(id: &str) -> AiBookOption {
        AiBookOption::new(id, format!("Book {id}"))
    }

    #[gpui::test]
    fn explanation_events_publish_only_current_persisted_answers_and_report_recoverable_failures(
        cx: &mut TestAppContext,
    ) {
        fn begin(sidebar: &mut AiSidebar, cx: &mut Context<AiSidebar>) -> u64 {
            assert!(sidebar.begin_selection_explanation(cx));
            let mut reference = AiReferenceHint::chapter("book", "unit", 0, "Chapter");
            reference.frozen_text = Some("frozen selected words".to_string());
            sidebar.complete_selection_explanation(Vec::new(), reference, cx);
            let RequestState::Sending { request_id } = sidebar.conversation.request else {
                panic!("explanation should submit a new request")
            };
            request_id
        }

        let temp = tempfile::tempdir().unwrap();
        let services = Arc::new(AppServices::open(temp.path()).unwrap());
        cx.update(gpui_component::init);
        let mut sidebar = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                AiSidebar::new(
                    AiSidebarScope::book(book("book"), Vec::new()),
                    Arc::clone(&services),
                    window,
                    cx,
                )
            });
            sidebar = Some(view.clone());
            Root::new(view, window, cx)
        });
        let sidebar = sidebar.unwrap();
        let submission_order = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&submission_order);
        let _explanation_subscription = sidebar.update(visual, |_, cx| {
            cx.subscribe(&sidebar, move |_, _, event: &AiExplanationSubmitted, _| {
                recorded
                    .borrow_mut()
                    .push(("explanation", event.request_id));
            })
        });
        let recorded = Rc::clone(&submission_order);
        let _submission_subscription = sidebar.update(visual, |_, cx| {
            cx.subscribe(&sidebar, move |_, _, event: &AiSidebarEvent, _| {
                if let AiSidebarEvent::Submit(request) = event {
                    recorded.borrow_mut().push(("submit", request.request_id));
                }
            })
        });
        let completed = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&completed);
        let _completion_subscription = sidebar.update(visual, |_, cx| {
            cx.subscribe(&sidebar, move |_, _, event: &AiAnswerCompleted, _| {
                recorded.borrow_mut().push(event.clone());
            })
        });
        let failed = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&failed);
        let _failure_subscription = sidebar.update(visual, |_, cx| {
            cx.subscribe(&sidebar, move |_, _, event: &AiExplanationFailed, _| {
                recorded.borrow_mut().push(event.clone());
            })
        });
        let failed_id = sidebar.update(visual, |sidebar, cx| {
            let request_id = begin(sidebar, cx);
            sidebar.append_answer_delta(request_id, "uncommitted provisional answer", cx);
            assert!(sidebar.fail_answer(request_id, "fixture persistence failure", cx));
            assert!(!sidebar.finish_persisted_answer(
                request_id,
                "late failed answer".into(),
                Vec::new(),
                AgentAnswerSourceStatus::NoVerifiedSources,
                cx,
            ));
            request_id
        });
        let cancelled_id = sidebar.update(visual, |sidebar, cx| {
            let request_id = begin(sidebar, cx);
            sidebar.append_answer_delta(request_id, "cancelled provisional answer", cx);
            assert_eq!(sidebar.cancel_for_window_close(cx), Some(request_id));
            request_id
        });
        visual.run_until_parked();
        assert!(completed.borrow().is_empty());
        assert_eq!(failed.borrow()[0].request_id, Some(failed_id));
        assert_eq!(failed.borrow()[1].request_id, Some(cancelled_id));
        assert_eq!(failed.borrow().len(), 2);

        let successful_id = sidebar.update(visual, |sidebar, cx| {
            let request_id = begin(sidebar, cx);
            assert!(!sidebar.finish_persisted_answer(
                cancelled_id,
                "old generation".into(),
                Vec::new(),
                AgentAnswerSourceStatus::NoVerifiedSources,
                cx,
            ));
            sidebar.append_answer_delta(request_id, "stream projection", cx);
            assert!(sidebar.finish_persisted_answer(
                request_id,
                "## Final persisted Markdown\n\n**final answer**".into(),
                Vec::new(),
                AgentAnswerSourceStatus::NoVerifiedSources,
                cx,
            ));
            assert!(!sidebar.finish_persisted_answer(
                request_id,
                "duplicate result".into(),
                Vec::new(),
                AgentAnswerSourceStatus::NoVerifiedSources,
                cx,
            ));
            request_id
        });
        visual.run_until_parked();
        assert_eq!(
            *completed.borrow(),
            vec![AiAnswerCompleted {
                request_id: successful_id,
                markdown: "## Final persisted Markdown\n\n**final answer**".into(),
            }]
        );
        assert_eq!(failed.borrow().len(), 2);

        sidebar.update(visual, |sidebar, cx| {
            sidebar.fail_session_operation("ordinary session failure", cx);
            let ordinary = sidebar.conversation.begin("ordinary question").unwrap();
            assert!(sidebar.fail_answer(ordinary.request_id, "ordinary answer failure", cx));
        });
        visual.run_until_parked();
        assert_eq!(
            failed.borrow().len(),
            2,
            "ordinary chat is not an explanation"
        );
        sidebar.update(visual, |sidebar, cx| {
            sidebar.fail_selection_explanation_session("fresh session failed", cx);
            let reference = AiReferenceHint::chapter("book", "unit", 0, "missing selection");
            sidebar.complete_selection_explanation(Vec::new(), reference, cx);
        });
        visual.run_until_parked();
        assert_eq!(failed.borrow().len(), 4);
        assert_eq!(failed.borrow()[2].request_id, None);
        assert_eq!(failed.borrow()[3].request_id, None);
        assert_eq!(completed.borrow().len(), 1);
        assert_eq!(
            *submission_order.borrow(),
            [failed_id, cancelled_id, successful_id]
                .into_iter()
                .flat_map(|id| [("explanation", id), ("submit", id)])
                .collect::<Vec<_>>(),
            "the exact explanation generation is bound before submission"
        );
    }

    fn cited_document() -> (BookDocument, AiSourceLink) {
        let text = "前文 引用目标 后文";
        let start = text.find("引用目标").unwrap();
        let mut unit = ContentUnit::new(
            "unit-1",
            ContentUnitKind::Chapter,
            "Chapter",
            "<p>前文 引用目标 后文</p>",
            BlockDocument::new(vec![Block::paragraph("block-1", text)]),
        )
        .with_source_locator(SourceLocator::epub("Text/chapter.xhtml#source-anchor"));
        unit.revision = Revision::new(7);
        let mut document = BookDocument::created("book", "Book");
        document.revision = Revision::new(3);
        document.units.push(unit);
        let locator = DocumentLocator::text(
            "book",
            "unit-1",
            "block-1",
            start as u64,
            (start + "引用目标".len()) as u64,
        )
        .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor"));
        let source = AiSourceLink {
            citation_id: "passage:one".to_string(),
            book_id: "book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_index: None,
            document_revision: Revision::new(3),
            unit_revision: Revision::new(7),
            locator: Some(locator),
            label: "Book · Chapter".to_string(),
            quote: Some("引用目标".to_string()),
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        (document, source)
    }

    #[test]
    fn citation_navigation_rederives_the_exact_text_from_the_current_ast() {
        let (document, source) = cited_document();
        let target = source.current_navigation_target(&document).unwrap();
        assert_eq!(target.unit_index, 0);
        assert_eq!(
            target.source,
            Some(SourceLocator::epub("Text/chapter.xhtml#source-anchor"))
        );
        assert_eq!(target.focus.as_ref().unwrap().text, "引用目标");
        assert_eq!(target.focus.as_ref().unwrap().before, "前文 ");
        assert_eq!(target.focus.as_ref().unwrap().after, " 后文");
    }

    #[test]
    fn citation_navigation_rejects_stale_versions_and_mismatched_sources() {
        let (document, mut source) = cited_document();
        source.document_revision = Revision::new(2);
        assert!(
            source
                .current_navigation_target(&document)
                .unwrap_err()
                .contains("版本已失效")
        );

        let (_, mut source) = cited_document();
        source.locator.as_mut().unwrap().source = Some(SourceLocator::epub("Text/other.xhtml"));
        assert!(
            source
                .current_navigation_target(&document)
                .unwrap_err()
                .contains("原始来源位置")
        );
    }

    #[test]
    fn citation_navigation_rejects_a_repeated_quote_without_a_text_range() {
        let (mut document, mut source) = cited_document();
        document.units[0].document = BlockDocument::new(vec![Block::paragraph(
            "block-1",
            "重复文字，中间内容，重复文字",
        )]);
        source.locator = Some(
            DocumentLocator::block("book", "unit-1", "block-1")
                .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor")),
        );
        source.quote = Some("重复文字".to_string());

        assert!(
            source
                .current_navigation_target(&document)
                .unwrap_err()
                .contains("不唯一")
        );
    }

    #[test]
    fn unit_quote_keeps_no_invented_context_so_dom_duplicates_are_ambiguous() {
        let (document, mut source) = cited_document();
        source.locator = Some(
            DocumentLocator::unit("book", "unit-1")
                .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor")),
        );
        source.quote = Some("引用目标".to_string());

        let target = source.current_navigation_target(&document).unwrap();
        let focus = target.focus.unwrap();
        assert_eq!(focus.text, "引用目标");
        assert!(focus.before.is_empty());
        assert!(focus.after.is_empty());
        let script = citation_dom_navigation_script(
            &focus,
            serde_json::json!({"type": "citation_navigation_result", "request_id": 2}),
        )
        .unwrap();
        assert!(script.contains("haystack.indexOf(context, matchStart + 1) >= 0"));
        assert!(script.contains("finish(false, matchStart < 0 ? 'not_found' : 'ambiguous')"));
    }

    #[test]
    fn restored_selection_snapshot_requires_a_unique_current_ast_quote() {
        let (mut document, mut source) = cited_document();
        source.selection_snapshot = true;
        source.locator = Some(
            DocumentLocator::unit("book", "unit-1")
                .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor")),
        );

        source.quote = Some("仅存在于未保存编辑器中的正文".to_string());
        source.mark_restored_status(Some(&document));
        assert!(
            source.stale,
            "missing unsaved text cannot be restored as current"
        );

        source.quote = Some("引用目标".to_string());
        source.mark_restored_status(Some(&document));
        assert!(!source.stale, "a unique current AST match is provable");

        document.units[0].document = BlockDocument::new(vec![Block::paragraph(
            "block-1",
            "重复文字，中间内容，重复文字",
        )]);
        source.quote = Some("重复文字".to_string());
        source.mark_restored_status(Some(&document));
        assert!(source.stale, "a unit-only repeated quote is ambiguous");

        let (document, mut visual_passage) = cited_document();
        visual_passage.locator = Some(
            DocumentLocator::unit("book", "unit-1")
                .with_source(SourceLocator::epub("Text/chapter.xhtml#source-anchor"))
                .with_region(NormalizedRect::new(10, 10, 200, 200)),
        );
        visual_passage.quote = Some("OCR text is not required in the reflowed AST".to_string());
        visual_passage.selection_snapshot = false;
        visual_passage.mark_restored_status(Some(&document));
        assert!(
            !visual_passage.stale,
            "visual passage provenance must bypass selection AST verification"
        );
    }

    #[test]
    fn dom_navigation_preserves_virtual_block_spaces_and_rejects_duplicate_quotes() {
        let focus = AiCitationTextFocus {
            text: "重复文字".to_string(),
            before: String::new(),
            after: String::new(),
        };
        let script = citation_dom_navigation_script(
            &focus,
            serde_json::json!({"type": "citation_navigation_result", "request_id": 1}),
        )
        .unwrap();
        assert!(script.contains("let hasPendingSpace = false"));
        assert!(script.contains("emitSpace(null)"));
        assert!(script.contains("positions.push(pendingSpace)"));
        assert!(script.contains("haystack.indexOf(context, matchStart + 1) >= 0"));
        assert!(script.contains("'ambiguous'"));
    }

    #[test]
    fn endpoint_status_distinguishes_loopback_and_remote_hosts() {
        for local in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            let status = AiEndpointStatus::from_base_url(local);
            assert_eq!(status, AiEndpointStatus::Local);
            assert_eq!(status.label(), "本地");
            assert!(!status.is_warning());
        }

        let remote = AiEndpointStatus::from_base_url("https://models.example.test/v1");
        assert_eq!(
            remote,
            AiEndpointStatus::Remote {
                hostname: "models.example.test".to_string(),
            }
        );
        assert_eq!(remote.label(), "远程 · models.example.test");
        assert!(remote.is_warning());

        let private_network = AiEndpointStatus::from_base_url("http://192.168.1.10:11434/v1");
        assert_eq!(
            private_network,
            AiEndpointStatus::Remote {
                hostname: "192.168.1.10".to_string(),
            }
        );
        assert!(private_network.is_warning());
    }

    #[test]
    fn endpoint_status_reports_remote_embedding_with_local_chat() {
        let mut settings = ProviderSettings::default();
        let mut endpoint = settings.endpoint_for(ModelRole::Chat).unwrap();
        endpoint.id = "remote-embedding".into();
        endpoint.name = "远程向量".into();
        endpoint.base_url = "https://vectors.example.test/v1".into();
        settings.endpoint_routing.embedding_endpoint_id = endpoint.id.clone();
        settings
            .endpoint_routing
            .additional_endpoints
            .push(endpoint);
        let status = AiEndpointStatus::from_settings(&settings);
        assert!(status.is_warning());
        assert!(status.label().contains("Embedding：vectors.example.test"));
        assert!(!status.label().contains("对话："));
        settings.endpoint_routing.vision_endpoint_id = "missing".into();
        assert_eq!(
            AiEndpointStatus::from_settings(&settings),
            AiEndpointStatus::Unavailable
        );
    }

    #[test]
    fn invalid_endpoint_status_is_explicitly_unavailable() {
        let status = AiEndpointStatus::from_base_url("not a URL");
        assert_eq!(status, AiEndpointStatus::Unavailable);
        assert_eq!(status.label(), "端点不可用");
        assert!(status.is_warning());
    }

    #[test]
    fn library_scope_deduplicates_books_and_preserves_host_order() {
        let scope = AiSidebarScope::library(
            "Group",
            vec![
                book("a"),
                book("b"),
                book("a"),
                AiBookOption::new("", "bad"),
            ],
        );
        assert_eq!(scope.book_ids(), vec!["a", "b"]);
        assert_eq!(scope.label(), "Group");
    }

    #[test]
    fn book_scope_keeps_current_book_and_only_adds_host_options() {
        let mut scope = AiSidebarScope::book(book("current"), vec![book("other")]);
        assert_eq!(scope.book_ids(), vec!["current"]);
        assert!(!scope.remove_book("current"));
        assert!(!scope.add_book("not-offered"));
        assert!(scope.add_book("other"));
        assert!(!scope.add_book("other"));
        assert_eq!(scope.book_ids(), vec!["current", "other"]);
        assert!(scope.remove_book("other"));
        assert_eq!(scope.book_ids(), vec!["current"]);
    }

    #[test]
    fn narrow_windows_start_collapsed_until_user_overrides() {
        let narrow = ResponsiveSidebar::new(px(900.));
        assert!(narrow.collapsed);

        let mut wide = ResponsiveSidebar::new(px(1400.));
        assert!(!wide.collapsed);
        assert!(wide.resize(px(900.)));
        assert!(wide.collapsed);
        wide.toggle();
        assert!(!wide.collapsed);
        assert!(!wide.resize(px(1400.)));
        assert!(!wide.resize(px(900.)));
        assert!(!wide.collapsed);
    }

    #[test]
    fn expanded_sidebar_width_is_clamped_to_usable_bounds() {
        assert_eq!(
            clamp_ai_sidebar_width(px(AI_SIDEBAR_MIN_WIDTH - 1.)),
            px(AI_SIDEBAR_MIN_WIDTH)
        );
        assert_eq!(clamp_ai_sidebar_width(px(420.)), px(420.));
        assert_eq!(
            clamp_ai_sidebar_width(px(AI_SIDEBAR_MAX_WIDTH + 1.)),
            px(AI_SIDEBAR_MAX_WIDTH)
        );
    }

    #[test]
    fn reference_list_shows_at_most_three_rows_without_dropping_candidates() {
        assert_eq!(reference_list_max_height(1), px(28.));
        assert_eq!(reference_list_max_height(2), px(56.));
        assert_eq!(reference_list_max_height(3), px(84.));
        assert_eq!(reference_list_max_height(6), px(84.));
    }

    #[test]
    fn selectable_message_html_keeps_untrusted_chat_content_inert() {
        let message =
            "<img src=\"https://example.test/tracker\"> & [link](https://example.test)\r\n'quoted'";
        let html = selectable_message_html(message);

        assert_eq!(
            html,
            "<p>&lt;img src=&quot;https://example.test/tracker&quot;&gt; &amp; [link](https://example.test)</p><p>&#39;quoted&#39;</p>"
        );
        assert!(!html.contains("<br>"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<a"));
    }

    #[test]
    fn no_source_warning_appears_only_after_an_explicit_final_status() {
        let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
        let request = state.begin("question").unwrap();
        assert!(state.append_delta(request.request_id, "model answer"));

        let provisional = state
            .messages
            .iter()
            .find(|message| message.role == AiMessageRole::Assistant)
            .unwrap();
        assert_eq!(provisional.source_status, None);
        assert!(!should_show_no_knowledge_base_source_warning(provisional));

        assert!(state.finish(
            request.request_id,
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
        ));
        let finished = state
            .messages
            .iter()
            .find(|message| message.role == AiMessageRole::Assistant)
            .unwrap();
        assert!(should_show_no_knowledge_base_source_warning(finished));
        assert_eq!(finished.content, "model answer");
        assert!(
            !finished
                .content
                .contains(NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE)
        );
        assert!(
            !selectable_message_html(&finished.content)
                .contains(NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE)
        );

        let mut grounded_without_displayable_links = finished.clone();
        grounded_without_displayable_links.source_status =
            Some(AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources);
        grounded_without_displayable_links.sources.clear();
        assert!(!should_show_no_knowledge_base_source_warning(
            &grounded_without_displayable_links
        ));
    }

    #[test]
    fn source_basis_badge_names_the_knowledge_the_answer_used() {
        fn assistant(status: AgentAnswerSourceStatus) -> AiMessage {
            AiMessage {
                id: 1,
                role: AiMessageRole::Assistant,
                content: "answer".to_string(),
                sources: Vec::new(),
                source_status: Some(status),
                request_id: None,
            }
        }
        assert_eq!(
            source_basis_badge(&assistant(
                AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
            ))
            .unwrap()
            .0,
            "已基于当前图书内容回答"
        );
        assert_eq!(
            source_basis_badge(&assistant(AgentAnswerSourceStatus::VerifiedWebSources))
                .unwrap()
                .0,
            "已基于联网搜索结果回答"
        );
        assert_eq!(
            source_basis_badge(&assistant(AgentAnswerSourceStatus::VerifiedMixedSources))
                .unwrap()
                .0,
            "已结合图书与联网搜索回答"
        );
        // Ungrounded answers keep the existing warning instead of a badge.
        assert!(
            source_basis_badge(&assistant(AgentAnswerSourceStatus::NoVerifiedSources)).is_none()
        );
    }

    #[test]
    fn web_source_links_survive_the_book_scope_filter() {
        let allowed = std::collections::HashSet::from(["book-a".to_string()]);
        let web = AiSourceLink {
            citation_id: "web:0".to_string(),
            book_id: String::new(),
            unit_id: String::new(),
            unit_index: None,
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            locator: None,
            label: "Docs".to_string(),
            quote: Some("snippet".to_string()),
            selection_snapshot: false,
            stale: false,
            url: Some("https://example.test/docs".to_string()),
        };
        assert!(allowed_source_link(&allowed, &web));

        // A book citation outside the current scope is still rejected.
        let mut foreign_book = web.clone();
        foreign_book.url = None;
        foreign_book.book_id = "secret".to_string();
        assert!(!allowed_source_link(&allowed, &foreign_book));
    }

    #[test]
    fn session_picker_tracks_active_thread_and_recovers_from_errors() {
        let first = AiThreadOption::new("thread-a", "First", vec!["book-a".to_string()]);
        let second = AiThreadOption::new("thread-b", "Second", vec!["book-a".to_string()]);
        let mut sessions = SessionPickerState::default();

        assert_eq!(sessions.active_title(), "新对话");
        assert!(sessions.begin());
        assert!(!sessions.begin());
        sessions.apply(
            Some(second.id.clone()),
            vec![first.clone(), second.clone(), second],
        );
        assert_eq!(sessions.active_title(), "Second");
        assert_eq!(sessions.threads.len(), 2);

        assert!(sessions.begin());
        sessions.fail("会话正被其它窗口使用");
        assert!(!sessions.busy);
        assert_eq!(sessions.active_title(), "Second");
        assert_eq!(sessions.error.as_deref(), Some("会话正被其它窗口使用"));
    }

    #[test]
    fn session_delete_confirmation_is_cleared_by_any_started_or_applied_operation() {
        let thread = AiThreadOption::new("thread-a", "First", vec!["book-a".to_string()]);
        let pending = PendingSessionDeletion {
            thread_id: thread.id.clone(),
            title: thread.title.clone(),
        };
        let mut sessions = SessionPickerState {
            threads: vec![thread.clone()],
            active_thread_id: Some(thread.id.clone()),
            open: true,
            pending_delete: Some(pending.clone()),
            ..SessionPickerState::default()
        };

        assert!(sessions.begin());
        assert_eq!(sessions.pending_delete, None);
        sessions.pending_delete = Some(pending);
        sessions.apply(None, vec![thread]);
        assert_eq!(sessions.pending_delete, None);
        assert_eq!(sessions.active_thread_id, None);
    }

    #[test]
    fn session_options_are_hard_filtered_by_the_current_book_scope() {
        let allowed = ["book-a".to_string(), "book-b".to_string()]
            .into_iter()
            .collect::<HashSet<_>>();
        let options = authorized_thread_options(
            vec![
                AiThreadOption::new("a", "A", vec!["book-a".to_string()]),
                AiThreadOption::new(
                    "ab",
                    "A + B",
                    vec!["book-a".to_string(), "book-b".to_string()],
                ),
                AiThreadOption::new("c", "C", vec!["book-c".to_string()]),
            ],
            &allowed,
        );

        assert_eq!(
            options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "ab"]
        );
    }

    #[test]
    fn changing_persisted_threads_keeps_only_the_in_flight_request_projection() {
        let mut conversation =
            ConversationState::new(AiSidebarScope::library("Shelf", vec![book("book-a")]));
        conversation.messages.push(AiMessage {
            id: 99,
            role: AiMessageRole::Assistant,
            content: "old session".to_string(),
            sources: Vec::new(),
            source_status: Some(AgentAnswerSourceStatus::NoVerifiedSources),
            request_id: None,
        });
        let request = conversation.begin("new question").unwrap();
        assert!(conversation.append_delta(request.request_id, "new answer"));

        conversation.retain_current_request_messages();

        assert_eq!(conversation.messages.len(), 2);
        assert!(
            conversation
                .messages
                .iter()
                .all(|message| message.request_id == Some(request.request_id))
        );
        assert!(
            conversation
                .messages
                .iter()
                .all(|message| message.content != "old session")
        );
    }

    #[test]
    fn requests_capture_exact_scope_and_independently_selected_references() {
        let mut state = ConversationState::new(AiSidebarScope::book(
            book("current"),
            vec![book("current"), book("other")],
        ));
        assert!(state.scope.add_book("other"));
        let first = AiReferenceHint::chapter("current", "unit-1", 0, "Chapter 1");
        let second = AiReferenceHint::chapter("other", "unit-2", 1, "Chapter 2");
        state.set_reference_hints(vec![first.clone(), second.clone()]);
        assert!(state.toggle_reference(&first));
        assert!(state.toggle_reference(&second));
        assert!(state.toggle_reference(&first));
        assert!(state.toggle_reference(&first));
        let request = state.begin("  What happened?  ").unwrap();
        assert_eq!(request.question, "What happened?");
        assert_eq!(request.book_ids, vec!["current", "other"]);
        assert_eq!(
            request
                .reference_hints
                .iter()
                .map(|reference| reference.unit_id.as_str())
                .collect::<Vec<_>>(),
            vec!["unit-1", "unit-2"]
        );
        assert_eq!(request.reference.unwrap().unit_id, "unit-1");
        assert!(state.begin("second request").is_none());
    }

    #[test]
    fn active_frozen_reference_enters_the_request_without_a_manual_toggle() {
        let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
        let mut highlighted = AiReferenceHint::chapter("current", "unit-1", 0, "Current highlight");
        highlighted.frozen_text = Some("exact selected words".to_string());
        highlighted.revision = Some(7);

        state.set_reference_hints(vec![highlighted.clone()]);

        assert_eq!(state.selected_reference_count(), 1);
        assert!(state.reference_is_included(&highlighted));
        let request = state.begin("Explain this").expect("valid request");
        assert_eq!(request.reference_hints, vec![highlighted.clone()]);
        assert_eq!(request.reference, Some(highlighted));
    }

    #[test]
    fn selection_explanation_replaces_previous_history_and_reference_choices() {
        let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
        let mut selected = AiReferenceHint::chapter("current", "unit-1", 0, "Selected text");
        selected.frozen_text = Some("exact selected words\n第二行".to_string());
        assert_eq!(
            selected.revision, None,
            "Reader freezes text without an editor revision"
        );
        let manual = AiReferenceHint::chapter("current", "unit-2", 1, "Manual chapter");
        state.set_reference_hints(vec![selected.clone(), manual.clone()]);
        assert!(state.toggle_reference(&selected));
        assert!(state.toggle_reference(&manual));
        let previous = state.begin("Previous question").unwrap();
        assert!(state.append_delta(previous.request_id, "Previous answer"));
        assert!(state.finish(
            previous.request_id,
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
        ));

        let request = state.begin_selection_explanation(selected.clone()).unwrap();

        assert_eq!(request.question, "用汉语详细解释一下");
        assert_eq!(request.reference_hints, vec![selected.clone()]);
        assert_eq!(request.reference, Some(selected.clone()));
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "用汉语详细解释一下");
        assert!(request.request_id > previous.request_id);
        assert!(state.included_references.is_empty());
        assert!(state.reference_is_included(&selected));

        // Selection changes while the answer is running only affect a later
        // question; they must not rewrite the already emitted request.
        let mut changed = selected.clone();
        changed.frozen_text = Some("new live selection".to_string());
        state.set_reference_hints(vec![changed, manual]);
        assert_eq!(request.reference_hints, vec![selected]);
    }

    #[test]
    fn selection_explanation_rejects_busy_or_removed_scope_without_mutating_conversation() {
        let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
        let mut selected = AiReferenceHint::chapter("current", "unit-1", 0, "Selected text");
        selected.frozen_text = Some("selected words".to_string());
        state.set_reference_hints(vec![selected.clone()]);
        let request = state.begin("Keep this request").unwrap();
        assert!(state.append_delta(request.request_id, "Still answering"));
        let messages = state.messages.clone();
        assert!(
            state
                .begin_selection_explanation(selected.clone())
                .is_none()
        );
        assert!(state.is_current(request.request_id));
        assert_eq!(state.messages, messages);
        assert_eq!(state.reference_hints, vec![selected.clone()]);

        assert!(state.finish(
            request.request_id,
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
        ));
        let completed = state.messages.clone();
        selected.book_id = "removed-book".to_string();
        assert!(state.begin_selection_explanation(selected).is_none());
        assert_eq!(state.messages, completed);
    }

    #[test]
    fn automatic_highlight_can_be_excluded_and_cleared_without_losing_manual_references() {
        let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
        let mut highlighted = AiReferenceHint::chapter("current", "unit-1", 0, "Current highlight");
        highlighted.frozen_text = Some("exact selected words".to_string());
        highlighted.revision = Some(7);
        let manual = AiReferenceHint::chapter("current", "unit-2", 1, "Other chapter");
        state.set_reference_hints(vec![highlighted.clone(), manual.clone()]);
        assert!(state.toggle_reference(&manual));

        assert!(state.toggle_reference(&highlighted));
        assert!(!state.reference_is_included(&highlighted));
        assert_eq!(state.selected_reference_count(), 1);

        state.set_reference_hints(vec![highlighted.clone(), manual.clone()]);
        assert!(
            !state.reference_is_included(&highlighted),
            "a redundant host refresh must preserve the user's exclusion"
        );
        assert!(state.toggle_reference(&highlighted));
        assert_eq!(state.selected_reference_count(), 2);

        let mut cleared = highlighted.clone();
        cleared.frozen_text = None;
        state.set_reference_hints(vec![cleared.clone(), manual.clone()]);
        assert!(state.automatic_reference.is_none());
        assert!(!state.reference_is_included(&cleared));
        assert!(state.reference_is_included(&manual));
        assert_eq!(state.selected_reference_count(), 1);
    }

    #[test]
    fn effective_reference_count_deduplicates_manual_and_automatic_keys_and_enforces_the_limit() {
        let mut state = ConversationState::new(AiSidebarScope::book(book("current"), Vec::new()));
        let plain = AiReferenceHint::chapter("current", "unit-0", 0, "Current chapter");
        state.set_reference_hints(vec![plain.clone()]);
        assert!(state.toggle_reference(&plain));

        let mut highlighted = plain.clone();
        highlighted.label = "Current highlight".to_string();
        highlighted.frozen_text = Some("selected".to_string());
        let mut references = vec![highlighted.clone()];
        references.extend((1..=MAX_SELECTED_REFERENCES).map(|index| {
            AiReferenceHint::chapter(
                "current",
                format!("unit-{index}"),
                index,
                format!("Chapter {index}"),
            )
        }));
        state.set_reference_hints(references.clone());

        assert_eq!(state.selected_reference_count(), 1);
        for reference in references.iter().skip(1).take(MAX_SELECTED_REFERENCES - 1) {
            assert!(state.toggle_reference(reference));
        }
        assert_eq!(state.selected_reference_count(), MAX_SELECTED_REFERENCES);
        assert!(!state.toggle_reference(references.last().unwrap()));

        let request = state.begin("Compare").expect("valid request");
        assert_eq!(request.reference_hints.len(), MAX_SELECTED_REFERENCES);
        assert_eq!(
            request
                .reference_hints
                .iter()
                .filter(|reference| reference.unit_id == "unit-0")
                .count(),
            1
        );
    }

    #[test]
    fn page_locators_distinguish_references_within_one_content_unit() {
        let mut state = ConversationState::new(AiSidebarScope::book(book("book"), Vec::new()));
        let mut first = AiReferenceHint::chapter("book", "unit-1", 0, "Page 1");
        first.locator = Some(
            DocumentLocator::unit("book", "unit-1")
                .with_source(SourceLocator::office_rendered_page(1)),
        );
        let mut second = first.clone();
        second.label = "Page 2".to_string();
        second.locator = Some(
            DocumentLocator::unit("book", "unit-1")
                .with_source(SourceLocator::office_rendered_page(2)),
        );

        state.set_reference_hints(vec![first.clone(), second.clone()]);
        assert_eq!(state.reference_hints.len(), 2);
        assert!(state.toggle_reference(&first));
        assert!(state.toggle_reference(&second));
        let request = state.begin("compare pages").expect("valid request");
        assert_eq!(request.reference_hints.len(), 2);
        assert_eq!(request.reference_hints[0].locator, first.locator);
        assert_eq!(request.reference_hints[1].locator, second.locator);
    }

    #[test]
    fn scope_refresh_preserves_only_still_authorized_additions() {
        let mut state = ConversationState::new(AiSidebarScope::book(
            book("current"),
            vec![book("other"), book("removed")],
        ));
        assert!(state.scope.add_book("other"));
        assert!(state.scope.add_book("removed"));

        assert!(state.set_scope(AiSidebarScope::book(
            AiBookOption::new("current", "Renamed current"),
            vec![book("other"), book("new")],
        )));
        assert_eq!(state.scope.book_ids(), vec!["current", "other"]);
        assert_eq!(state.scope.label(), "Renamed current 等 2 本");
        assert!(!state.scope.add_book("removed"));
        assert!(state.scope.add_book("new"));
        assert!(!state.set_scope(AiSidebarScope::book(
            AiBookOption::new("current", "Renamed current"),
            vec![book("other"), book("new")],
        )));
    }

    #[test]
    fn scope_change_drops_only_unauthorized_reference_selections() {
        let mut state =
            ConversationState::new(AiSidebarScope::library("All", vec![book("a"), book("b")]));
        let a = AiReferenceHint::chapter("a", "unit-a", 0, "A");
        let b = AiReferenceHint::chapter("b", "unit-b", 0, "B");
        state.set_reference_hints(vec![a.clone(), b.clone()]);
        assert!(state.toggle_reference(&a));
        assert!(state.toggle_reference(&b));
        state.messages.push(AiMessage {
            id: 99,
            role: AiMessageRole::Assistant,
            content: "answer".to_string(),
            sources: vec![
                AiSourceLink {
                    citation_id: "citation-a".to_string(),
                    book_id: "a".to_string(),
                    unit_id: "unit-a".to_string(),
                    unit_index: None,
                    document_revision: Revision::new(1),
                    unit_revision: Revision::new(1),
                    locator: Some(DocumentLocator::unit("a", "unit-a")),
                    label: "A".to_string(),
                    quote: None,
                    selection_snapshot: false,
                    stale: false,
                    url: None,
                },
                AiSourceLink {
                    citation_id: "citation-b".to_string(),
                    book_id: "b".to_string(),
                    unit_id: "unit-b".to_string(),
                    unit_index: None,
                    document_revision: Revision::new(1),
                    unit_revision: Revision::new(1),
                    locator: Some(DocumentLocator::unit("b", "unit-b")),
                    label: "B".to_string(),
                    quote: None,
                    selection_snapshot: false,
                    stale: false,
                    url: None,
                },
            ],
            source_status: Some(AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources),
            request_id: None,
        });

        state.set_scope(AiSidebarScope::library("Only B", vec![book("b")]));

        assert_eq!(state.reference_hints, vec![b.clone()]);
        assert_eq!(state.included_references.len(), 1);
        assert!(state.included_references.contains(&reference_key(&b)));
        assert_eq!(state.messages[0].sources.len(), 1);
        assert_eq!(state.messages[0].sources[0].book_id, "b");
    }

    #[test]
    fn cancellation_rejects_late_stream_callbacks() {
        let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
        let request = state.begin("question").unwrap();
        assert!(state.append_delta(request.request_id, "partial"));
        assert_eq!(state.cancel(), Some(request.request_id));
        assert!(!state.messages.iter().any(|message| {
            message.request_id == Some(request.request_id)
                && message.role == AiMessageRole::Assistant
        }));
        assert!(!state.append_delta(request.request_id, "late"));
        assert!(!state.finish(
            request.request_id,
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
        ));
        assert!(!state.fail(request.request_id, "late failure"));
        assert!(!state.is_sending());
    }

    #[test]
    fn provisional_answer_can_be_reset_before_a_later_final_stream() {
        let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
        let request = state.begin("question").unwrap();
        assert!(state.append_delta(request.request_id, "temporary tool planning"));
        assert!(state.reset_answer(request.request_id));
        assert!(!state.reset_answer(request.request_id));
        assert!(state.append_delta(request.request_id, "final "));
        assert!(state.append_delta(request.request_id, "answer"));
        assert!(state.finish(
            request.request_id,
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
        ));
        let answer = state
            .messages
            .iter()
            .find(|message| message.role == AiMessageRole::Assistant)
            .unwrap();
        assert_eq!(answer.content, "final answer");
    }

    #[test]
    fn failed_stream_discards_provisional_answer_before_showing_error() {
        let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
        let request = state.begin("question").unwrap();
        assert!(state.append_delta(request.request_id, "unvalidated partial"));
        assert!(state.fail(request.request_id, "invalid citation"));
        assert!(!state.messages.iter().any(|message| {
            message.request_id == Some(request.request_id)
                && message.role == AiMessageRole::Assistant
        }));
        assert_eq!(state.messages.last().unwrap().role, AiMessageRole::Error);
        assert_eq!(state.messages.last().unwrap().content, "invalid citation");
    }

    #[test]
    fn only_current_request_can_receive_sources() {
        let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
        let request = state.begin("question").unwrap();
        assert!(state.append_delta(request.request_id, "answer"));
        let source = AiSourceLink {
            citation_id: "source-1".to_string(),
            book_id: "a".to_string(),
            unit_id: "unit-1".to_string(),
            unit_index: Some(0),
            document_revision: Revision::new(1),
            unit_revision: Revision::new(1),
            locator: None,
            label: "Chapter 1".to_string(),
            quote: Some("evidence".to_string()),
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        let foreign = AiSourceLink {
            citation_id: "source-foreign".to_string(),
            book_id: "secret".to_string(),
            ..source.clone()
        };
        assert!(state.finish(
            request.request_id,
            vec![source.clone(), foreign],
            AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources,
        ));
        assert_eq!(state.messages.last().unwrap().sources, vec![source]);
        assert!(!state.finish(
            request.request_id,
            Vec::new(),
            AgentAnswerSourceStatus::NoVerifiedSources,
        ));
    }

    #[test]
    fn source_versions_are_checked_before_navigation_and_stale_links_remain_identifiable() {
        let mut document = BookDocument::created("book", "Book");
        document.revision = Revision::new(3);
        let mut unit = ContentUnit::empty("unit-1", ContentUnitKind::Chapter, "Chapter");
        unit.revision = Revision::new(7);
        document.units.push(unit);
        let mut source = AiSourceLink {
            citation_id: "source-1".to_string(),
            book_id: "book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_index: Some(0),
            document_revision: Revision::new(3),
            unit_revision: Revision::new(7),
            locator: Some(DocumentLocator::unit("book", "unit-1")),
            label: "Chapter".to_string(),
            quote: None,
            selection_snapshot: false,
            stale: false,
            url: None,
        };
        assert_eq!(source.current_unit_index(&document), Some(0));

        document.units[0].revision = Revision::new(8);
        source.mark_current_status(Some(&document));
        assert!(source.stale);
        assert_eq!(source.current_unit_index(&document), None);
        assert!(source.validated_locator().is_some());
    }
}
