//! The book scope a thread is scoped to.

use super::*;

/// Host-computed book authority persisted with a conversation.
///
/// Model-provided book IDs never update this value. A question may narrow this
/// scope through the Agent API, but it cannot widen it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatScope {
    pub book_ids: Vec<String>,
}

impl ChatScope {
    pub fn new<I, S>(book_ids: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut ids = BTreeSet::new();
        for id in book_ids {
            let id = id.into();
            validate_id("book ID", &id)?;
            ids.insert(id);
        }
        ensure!(
            ids.len() <= MAX_SCOPE_BOOKS,
            "chat scope exceeds {MAX_SCOPE_BOOKS} books"
        );
        Ok(Self {
            book_ids: ids.into_iter().collect(),
        })
    }

    pub fn contains(&self, book_id: &str) -> bool {
        self.book_ids
            .binary_search_by(|candidate| candidate.as_str().cmp(book_id))
            .is_ok()
    }

    pub(crate) fn normalized(&self) -> Result<Self> {
        Self::new(self.book_ids.iter().cloned())
    }
}
