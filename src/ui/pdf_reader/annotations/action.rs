//! The note and annotation actions a page can raise.

use super::*;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PdfNoteAction {
    List,
    /// The viewer's mounted reading window. Pages outside it may not author
    /// notes, which replaces "only the single rendered page may".
    PagesRendered,
    Highlight,
    Wavy,
    Underline,
    RemoveMark,
    HumanComment,
    AiExplain,
    Update,
    Delete,
    DraftChanged,
    RetryAiSave,
    DiscardAiSave,
}

/// Typed notes request from the bundled viewer's private origin.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct PdfAnnotationAction {
    pub(crate) action: PdfNoteAction,
    pub(crate) session: String,
    pub(crate) revision: u64,
    pub(crate) request_id: u64,
    #[serde(default)]
    pub(crate) page: u32,
    /// Pages this request concerns: the mounted reading window for a
    /// declaration, or the pages whose notes are requested in one reply.
    #[serde(default)]
    pub(crate) pages: Vec<u32>,
    #[serde(default)]
    pub(crate) anchor: Option<TextAnchor>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) dirty: bool,
}

impl PdfAnnotationAction {
    pub(crate) fn valid(&self) -> bool {
        !self.session.is_empty()
            && self.session.len() <= 128
            // Every request names the page it was authored on. `handle` then
            // compares it with the pages the viewer declared as rendered.
            && self.page >= 1
            && self.page <= MAX_ANNOTATION_PAGE
            && (1..=(1_u64 << 53) - 1).contains(&self.request_id)
            && self
                .id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 256)
            && self
                .content
                .as_ref()
                .is_none_or(|text| text.len() <= 64 * 1024)
            && self.anchor.as_ref().is_none_or(|anchor| {
                anchor.start < anchor.end
                    && !anchor.quote.trim().is_empty()
                    && anchor.quote.len() <= MAX_PDF_SELECTION_BYTES
            })
            // A page may never author an AI thought or delete a thought by ID
            // through the mark path.
            && (self.action != PdfNoteAction::RemoveMark
                || (self.anchor.is_some() && self.id.is_none() && self.content.is_none()))
            && self.pages.len() <= MAX_ANNOTATION_PAGES
            && self
                .pages
                .iter()
                .all(|page| (1..=MAX_ANNOTATION_PAGE).contains(page))
            // An empty window would disable every note the viewer could write.
            && (self.action != PdfNoteAction::PagesRendered || !self.pages.is_empty())
    }
}
