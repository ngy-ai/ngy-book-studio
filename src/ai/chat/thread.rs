//! Stored threads and the shape used to create one.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatThread {
    pub id: String,
    /// Reader/Editor conversations retain their mandatory current book here.
    /// Library conversations use `None` and persist their group-derived scope.
    pub primary_book_id: Option<String>,
    pub title: String,
    pub scope: ChatScope,
    pub window_kind: ChatWindowKind,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatThread {
    pub primary_book_id: Option<String>,
    pub title: String,
    pub scope: ChatScope,
    pub window_kind: ChatWindowKind,
}

impl NewChatThread {
    pub(crate) fn validate(&self) -> Result<ChatScope> {
        validate_title(&self.title)?;
        let scope = self.scope.normalized()?;
        match self.window_kind {
            ChatWindowKind::Library => {}
            ChatWindowKind::Reader | ChatWindowKind::Editor => {
                let primary = self
                    .primary_book_id
                    .as_deref()
                    .context("Reader/Editor chat needs a primary book")?;
                validate_id("primary book ID", primary)?;
                ensure!(
                    scope.contains(primary),
                    "Reader/Editor chat scope must contain its primary book"
                );
            }
        }
        if let Some(primary) = self.primary_book_id.as_deref() {
            validate_id("primary book ID", primary)?;
        }
        Ok(scope)
    }
}
