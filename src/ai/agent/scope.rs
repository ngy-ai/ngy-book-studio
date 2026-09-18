//! The book scope the agent is allowed to read.

use super::*;

/// The immutable set selected by the host from window state and group rules.
/// Model-provided IDs are never used to construct or enlarge this set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowedBookScope {
    pub(crate) ids: BTreeSet<String>,
}

impl AllowedBookScope {
    pub fn new<I, S>(ids: I) -> AgentResult<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut allowed = BTreeSet::new();
        for id in ids {
            let id = id.into();
            validate_identifier("book_id", &id)?;
            allowed.insert(id);
        }
        Ok(Self { ids: allowed })
    }

    pub fn contains(&self, book_id: &str) -> bool {
        self.ids.contains(book_id)
    }

    pub fn ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.ids.iter().map(String::as_str)
    }

    pub(super) fn narrow(&self, requested: Option<&[String]>) -> AgentResult<Vec<String>> {
        let Some(requested) = requested else {
            return Ok(self.ids.iter().cloned().collect());
        };
        if requested.is_empty() {
            return Err(AgentError::InvalidArguments(
                "book_ids must not be empty when supplied".to_string(),
            ));
        }

        let mut narrowed = BTreeSet::new();
        for id in requested {
            validate_identifier("book_id", id)?;
            if !self.ids.contains(id) {
                return Err(AgentError::ScopeViolation);
            }
            narrowed.insert(id.clone());
        }
        Ok(narrowed.into_iter().collect())
    }
}
