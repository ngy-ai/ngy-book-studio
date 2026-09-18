//! Serving and shrinking passage results.

use super::*;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ServedPassage {
    pub(crate) citation_id: String,
    /// Needed by the model as the `read_passages` argument.
    pub(crate) passage_id: String,
    /// Host-internal identifier. It is deliberately kept out of the tool
    /// payload: an identifier that looks citable but is not a registered
    /// source marker makes the model emit it inside `[[ngy-source:...]]`,
    /// which the host must then reject.
    #[serde(skip_serializing)]
    pub(crate) book_id: String,
    pub(crate) book_title: String,
    #[serde(skip_serializing)]
    pub(crate) unit_id: String,
    pub(crate) unit_title: String,
    #[serde(skip_serializing)]
    pub(crate) document_revision: Revision,
    #[serde(skip_serializing)]
    pub(crate) unit_revision: Revision,
    pub(crate) text: String,
    #[serde(skip_serializing)]
    pub(crate) locator: DocumentLocator,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relevance: Option<f64>,
}

impl ServedPassage {
    pub(super) fn citation(&self) -> AgentCitation {
        AgentCitation {
            citation_id: self.citation_id.clone(),
            book_id: self.book_id.clone(),
            book_title: self.book_title.clone(),
            unit_id: self.unit_id.clone(),
            unit_title: self.unit_title.clone(),
            document_revision: self.document_revision,
            unit_revision: self.unit_revision,
            quote: self.text.clone(),
            locator: self.locator.clone(),
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
        }
    }
}

pub(crate) fn sanitize_passages(
    records: Vec<PassageRecord>,
    host_scope: &AllowedBookScope,
    requested_books: &HashSet<String>,
    requested_passages: Option<&HashSet<String>>,
    limit: usize,
) -> Vec<ServedPassage> {
    let mut seen = HashSet::new();
    records
        .into_iter()
        .filter(|record| {
            host_scope.contains(&record.book_id)
                && requested_books.contains(record.book_id.as_str())
                && requested_passages.is_none_or(|ids| ids.contains(record.passage_id.as_str()))
                && valid_identifier(&record.passage_id)
                && valid_identifier(&record.unit_id)
                && record.locator.book_id == record.book_id
                && record.locator.unit_id == record.unit_id
                && record.locator.validate().is_ok()
                && !matches!(
                    record.locator.source.as_ref(),
                    Some(SourceLocator::OfficeRenderedPage { .. })
                )
                && record.relevance.is_none_or(f64::is_finite)
                && !record.text.trim().is_empty()
                && seen.insert(record.passage_id.clone())
        })
        .take(limit)
        .map(|record| ServedPassage {
            citation_id: format!("passage:{}", record.passage_id),
            passage_id: record.passage_id,
            book_id: record.book_id,
            book_title: truncate_utf8(&record.book_title, 512),
            unit_id: record.unit_id,
            unit_title: truncate_utf8(&record.unit_title, 512),
            document_revision: record.document_revision,
            unit_revision: record.unit_revision,
            text: truncate_utf8(&record.text, 8192),
            locator: record.locator,
            relevance: record.relevance,
        })
        .collect()
}

pub(crate) fn fit_passage_output(
    tool: &str,
    mut records: Vec<ServedPassage>,
    mut truncated: bool,
    budget: usize,
) -> AgentResult<(String, Vec<AgentCitation>)> {
    loop {
        let content = serde_json::to_string(&json!({
            "tool": tool,
            "results": records,
            "truncated": truncated,
        }))
        .map_err(|error| AgentError::Backend(error.to_string()))?;
        if content.len() <= budget {
            let citations = records.iter().map(ServedPassage::citation).collect();
            return Ok((content, citations));
        }

        truncated = true;
        let Some(longest) = records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.text.len() > 256)
            .max_by_key(|(_, record)| record.text.len())
            .map(|(index, _)| index)
        else {
            if records.pop().is_none() {
                return Err(AgentError::LimitExceeded("context size"));
            }
            continue;
        };
        let next_len = (records[longest].text.len() / 2).max(256);
        records[longest].text = truncate_utf8(&records[longest].text, next_len);
    }
}
