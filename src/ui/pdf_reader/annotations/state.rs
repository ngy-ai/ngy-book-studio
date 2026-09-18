//! Per-page annotation state and the position helpers.

use super::*;

#[derive(Clone)]
pub(crate) struct PendingAiNote {
    pub(crate) action: PdfAnnotationAction,
    pub(crate) draft: AnnotationDraft,
    pub(crate) request_id: Option<u64>,
}

pub(crate) struct PdfAnnotations {
    /// Canonical document revision every note draft is written with. It has no
    /// say in whether notes are offered: `0` is the ordinary revision of every
    /// imported or created book, so a zero value must not hide the toolbar.
    pub(crate) document_revision: u64,
    /// One bridge session per opened PDF revision. The continuous viewer keeps
    /// several pages mounted at once, so pages are declared separately instead
    /// of rebinding the whole session on every scroll.
    pub(crate) session: String,
    pub(crate) last_request: u64,
    /// Pages the viewer currently has mounted. A page request is only served
    /// while it is part of this window, which replaces the old single-page
    /// check without weakening it.
    pub(crate) rendered_pages: BTreeSet<u32>,
    pub(crate) busy: bool,
    pub(crate) dirty: bool,
    pub(crate) pending_ai: Option<PendingAiNote>,
}

impl PdfAnnotations {
    pub(crate) fn new(document_revision: u64) -> Self {
        Self {
            document_revision,
            session: String::new(),
            last_request: 0,
            rendered_pages: BTreeSet::new(),
            busy: false,
            dirty: false,
            pending_ai: None,
        }
    }
}

/// Notes of one page, already resolved to its canonical content unit.
#[derive(Clone)]
pub(crate) struct PageAnnotations {
    pub(crate) page: u32,
    pub(crate) unit_revision: u64,
    pub(crate) enabled: bool,
    pub(crate) notes: Vec<Annotation>,
}

pub(crate) fn page_annotations_json(entry: PageAnnotations) -> serde_json::Value {
    serde_json::json!({
        "page": entry.page,
        "revision": entry.unit_revision,
        "enabled": entry.enabled,
        "notes": notes_json(entry.notes),
    })
}

pub(crate) fn notes_json(notes: Vec<Annotation>) -> serde_json::Value {
    serde_json::Value::Array(
        notes
            .into_iter()
            .map(|note| {
                let content = note.comment.unwrap_or_default();
                let content_html = note
                    .kind
                    .is_comment()
                    .then(|| thought_display_html(&content));
                serde_json::json!({
                    "id": note.id, "kind": note.kind, "anchor": note.anchor,
                    "content": content, "content_html": content_html, "stale": note.stale,
                    "created_at": note.created_at,
                })
            })
            .collect(),
    )
}

/// Canonical identity of the page a note may be written to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PdfNotePosition {
    pub(crate) unit_id: String,
    pub(crate) unit_revision: u64,
    pub(crate) page_number: u32,
}

/// Resolves whether the rendered page can own persisted notes.
///
/// Only the page's own canonical unit identity decides this: without a stable
/// unit ID nothing could be associated without guessing, which is how
/// ephemeral Office PDFs are represented. The document revision must never
/// gate this because `Revision::INITIAL` (`0`) is the ordinary revision of
/// every imported or created book, and only editing advances it.
pub(crate) fn page_can_own_notes(page: &PdfReaderPage) -> bool {
    page.unit_id
        .as_deref()
        .is_some_and(|unit_id| !unit_id.trim().is_empty())
}

pub(crate) fn pdf_note_position(
    pages: &[PdfReaderPage],
    page_number: u32,
) -> Option<PdfNotePosition> {
    let page = pages.iter().find(|page| page.page_number == page_number)?;
    let unit_id = page
        .unit_id
        .as_deref()
        .filter(|unit_id| !unit_id.trim().is_empty())?;
    Some(PdfNotePosition {
        unit_id: unit_id.to_string(),
        unit_revision: page.unit_revision,
        page_number: page.page_number,
    })
}
