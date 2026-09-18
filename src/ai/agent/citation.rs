//! Citations, the registry that validates them and the answer shape.

use super::*;

/// Where a served source came from.
///
/// Book sources carry a verifiable [`DocumentLocator`] plus document/unit
/// revisions, so a persisted citation can be re-resolved and marked stale.
/// Web sources carry only a URL and a title: they have no locator, no unit and
/// no revision, so they are never treated as a book passage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCitationSourceKind {
    #[default]
    Book,
    Web,
}

impl AgentCitationSourceKind {
    pub fn is_web(self) -> bool {
        matches!(self, Self::Web)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCitation {
    pub citation_id: String,
    /// Empty for web sources; web identity lives in [`Self::url`].
    pub book_id: String,
    pub book_title: String,
    pub unit_id: String,
    pub unit_title: String,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub quote: String,
    pub locator: DocumentLocator,
    #[serde(default)]
    pub source_kind: AgentCitationSourceKind,
    /// Absolute http(s) URL, populated only for web sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Human-readable origin (result title or site name) for web sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_title: Option<String>,
}

impl AgentCitation {
    /// Builds a web source. Web results have no book/unit/locator identity, so
    /// those fields stay empty and must never be resolved as a passage.
    pub fn web(citation_id: String, title: String, url: String, snippet: String) -> Self {
        Self {
            citation_id,
            book_id: String::new(),
            book_title: title.clone(),
            unit_id: String::new(),
            unit_title: String::new(),
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            quote: snippet,
            // Web results have no book/unit coordinate. The locator is an
            // intentionally empty struct so it can never be resolved to a
            // passage; identity comes from `url` alone.
            locator: DocumentLocator::unit("", ""),
            source_kind: AgentCitationSourceKind::Web,
            url: Some(url),
            source_title: Some(title),
        }
    }

    pub fn is_web(&self) -> bool {
        self.source_kind.is_web()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAnswerSourceStatus {
    /// Every retained citation was issued by the host for this question and
    /// points at the authorized knowledge base.
    VerifiedKnowledgeBaseSources,
    /// Every retained citation came from a host-performed web search.
    VerifiedWebSources,
    /// The answer retained both book and web citations.
    VerifiedMixedSources,
    /// The final answer retained no host-verified citation. It may still be a
    /// useful general-model answer, but callers must not present it as grounded
    /// in the authorized knowledge base or in retrieved web pages.
    NoVerifiedSources,
}

impl AgentAnswerSourceStatus {
    /// Short, user-facing description of what the answer is grounded in.
    pub fn label(&self) -> &'static str {
        match self {
            Self::VerifiedKnowledgeBaseSources => "基于当前图书内容回答",
            Self::VerifiedWebSources => "基于联网搜索结果回答",
            Self::VerifiedMixedSources => "基于当前图书与联网搜索结果回答",
            Self::NoVerifiedSources => "基于模型自身知识回答（未在图书或联网结果中找到依据）",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAnswer {
    pub markdown: String,
    pub citations: Vec<AgentCitation>,
    pub source_status: AgentAnswerSourceStatus,
}

#[derive(Clone, Debug, Default)]
pub struct CitationRegistry {
    pub(crate) served_by_marker: BTreeMap<String, AgentCitation>,
}

impl CitationRegistry {
    pub fn record_citation(&mut self, citation: AgentCitation) -> AgentResult<()> {
        let marker = citation.citation_id.clone();
        self.record_citation_for_marker(marker, citation)
    }

    /// Registers a source under the exact turn-local marker shown to the
    /// model. Selection aliases deliberately differ from the stable citation
    /// ID returned to callers and persisted by the host.
    pub(crate) fn record_citation_for_marker(
        &mut self,
        marker: impl Into<String>,
        citation: AgentCitation,
    ) -> AgentResult<()> {
        let marker = marker.into();
        validate_identifier("citation marker", &marker)?;
        if let Some(previous) = self.served_by_marker.get(&marker) {
            if previous != &citation {
                return Err(AgentError::InvalidArguments(
                    "citation marker was served with conflicting source metadata".to_string(),
                ));
            }
            return Ok(());
        }
        self.served_by_marker.insert(marker, citation);
        Ok(())
    }

    pub fn record(&mut self, execution: &ToolExecution) -> AgentResult<()> {
        for citation in &execution.citations {
            self.record_citation(citation.clone())?;
        }
        Ok(())
    }

    /// Removes passage sources omitted from the accepted model request after
    /// context reduction. Frozen selection aliases and web sources are kept
    /// because their source text is carried outside passage tool messages.
    pub(crate) fn retain_passage_markers(&mut self, markers: &HashSet<String>) {
        self.served_by_marker
            .retain(|marker, _| !marker.starts_with("passage:") || markers.contains(marker));
    }

    pub fn finish(
        &self,
        markdown: impl Into<String>,
        citation_ids: &[String],
    ) -> AgentResult<AgentAnswer> {
        let mut seen = HashSet::new();
        let mut citations = Vec::new();
        for id in citation_ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let citation = self
                .served_by_marker
                .get(id)
                .ok_or_else(|| AgentError::UnknownCitation(id.clone()))?;
            citations.push(citation.clone());
        }
        let mut web = 0usize;
        for citation in &citations {
            if citation.is_web() {
                web += 1;
            }
        }
        let source_status = if citations.is_empty() {
            AgentAnswerSourceStatus::NoVerifiedSources
        } else if web == 0 {
            AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
        } else if web == citations.len() {
            AgentAnswerSourceStatus::VerifiedWebSources
        } else {
            AgentAnswerSourceStatus::VerifiedMixedSources
        };
        Ok(AgentAnswer {
            markdown: markdown.into(),
            citations,
            source_status,
        })
    }
}
