//! Reference hints and the source links a citation navigates to.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiReferenceHint {
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
    /// What the reader actually displayed for that frozen selection, when it
    /// differs from the stored book text (a reading-time translation). Only the
    /// explanation question uses it: citations and notes stay on `frozen_text`.
    pub displayed_text: Option<String>,
    pub revision: Option<u64>,
}

impl AiReferenceHint {
    pub(crate) fn chapter(
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
            displayed_text: None,
            revision: None,
        }
    }
}

/// The configured explanation question, or the same request naming the译文 the
/// reader displayed.
///
/// `prompt` 是设置里的「AI 解释提示词」；把译文附在它后面，而不是替换成一句固定
/// 文案：用户改提示词（换语言、加要求）时，译文解释请求也应当跟着改。
///
/// The question must stay inside the sidebar's own byte budget even when the
/// reader's selection cap allows more, so an oversized translation is truncated
/// at a character boundary instead of rejecting the whole request.
pub(crate) fn selection_explanation_question(reference: &AiReferenceHint, prompt: &str) -> String {
    let displayed = reference
        .displayed_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let Some(displayed) = displayed else {
        return prompt.to_string();
    };
    let mut question = String::from(prompt);
    question.push_str(SELECTION_TRANSLATION_QUESTION_SUFFIX);
    let budget = MAX_QUESTION_BYTES
        .saturating_sub(question.len() + SELECTION_TRANSLATION_QUESTION_TRUNCATED.len());
    if displayed.len() <= budget {
        question.push_str(displayed);
        return question;
    }
    let mut end = budget;
    while end > 0 && !displayed.is_char_boundary(end) {
        end -= 1;
    }
    question.push_str(&displayed[..end]);
    question.push_str(SELECTION_TRANSLATION_QUESTION_TRUNCATED);
    question
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiSourceLink {
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
pub(crate) struct AiCitationNavigationTarget {
    pub unit_index: usize,
    pub locator: DocumentLocator,
    pub source: Option<SourceLocator>,
    pub focus: Option<AiCitationTextFocus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AiCitationTextFocus {
    pub(crate) text: String,
    pub(crate) before: String,
    pub(crate) after: String,
}

pub(crate) const CITATION_CONTEXT_CHARACTERS: usize = 96;
pub(crate) const MAX_CITATION_FOCUS_BYTES: usize = 16 * 1024;

impl AiSourceLink {
    pub(crate) fn validated_locator(&self) -> Option<&DocumentLocator> {
        self.locator.as_ref().filter(|locator| {
            locator.book_id == self.book_id
                && locator.unit_id == self.unit_id
                && locator.validate().is_ok()
        })
    }

    pub(crate) fn current_unit_index(&self, document: &BookDocument) -> Option<usize> {
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

    pub(super) fn selection_matches_current_ast(&self, unit: &ContentUnit) -> bool {
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

    pub(crate) fn mark_current_status(&mut self, document: Option<&BookDocument>) {
        self.stale = false;
        self.stale = document
            .and_then(|document| self.current_unit_index(document))
            .is_none();
    }

    pub(crate) fn mark_restored_status(&mut self, document: Option<&BookDocument>) {
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

    pub(crate) fn current_navigation_target(
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

pub(crate) fn non_empty_citation_quote(quote: Option<&str>) -> Option<&str> {
    quote.filter(|quote| !quote.trim().is_empty())
}

pub(crate) fn citation_text_focus(
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
