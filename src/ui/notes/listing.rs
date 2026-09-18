//! Preparing the notes, filtering them and paging the result.

use super::*;

pub(crate) struct PreparedNotes {
    pub(crate) notes: Vec<AnnotationOverview>,
    pub(crate) comment_markdown: HashMap<String, Arc<ThoughtDisplay>>,
}

impl PreparedNotes {
    // Run after the library read on the background worker. Sanitizing a large
    // saved AI thought requires Markdown parsing, so it must not run in a
    // GPUI callback when loading, searching, or moving between pages.
    pub(super) fn new(notes: Vec<AnnotationOverview>) -> Self {
        let comment_markdown = notes
            .iter()
            .filter_map(|note| {
                note.annotation.comment.as_deref().map(|comment| {
                    (
                        note.annotation.id.clone(),
                        Arc::new(ThoughtDisplay::new(assistant_display_markdown(comment))),
                    )
                })
            })
            .collect();
        Self {
            notes,
            comment_markdown,
        }
    }
}

#[derive(Default)]
pub(crate) struct NoteListing {
    pub(crate) notes: Vec<AnnotationOverview>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) page: usize,
    pub(crate) cards: Vec<NoteCard>,
    pub(crate) filter: NoteFilter,
    pub(crate) query: String,
    pub(crate) book_count: usize,
    pub(crate) comment_markdown: HashMap<String, Arc<ThoughtDisplay>>,
}

impl NoteListing {
    #[cfg(test)]
    pub(super) fn replace(&mut self, notes: Vec<AnnotationOverview>) {
        self.replace_prepared(PreparedNotes::new(notes));
    }

    pub(super) fn replace_prepared(&mut self, prepared: PreparedNotes) {
        let PreparedNotes {
            mut notes,
            comment_markdown,
        } = prepared;
        notes.sort_by(|left, right| {
            right
                .annotation
                .updated_at
                .cmp(&left.annotation.updated_at)
                .then_with(|| right.annotation.created_at.cmp(&left.annotation.created_at))
                .then_with(|| left.annotation.id.cmp(&right.annotation.id))
        });
        self.book_count = notes
            .iter()
            .map(|note| &note.annotation.book_id)
            .collect::<HashSet<_>>()
            .len();
        self.notes = notes;
        self.comment_markdown = comment_markdown;
        self.apply_filter();
    }

    pub(super) fn apply_filter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.filtered = self
            .notes
            .iter()
            .enumerate()
            .filter(|(_, note)| {
                self.filter.includes(note.annotation.kind) && note_matches(note, &query)
            })
            .map(|(index, _)| index)
            .collect();
        self.page = 0;
        self.prepare_page();
    }

    pub(super) fn page_count(&self) -> usize {
        self.filtered.len().div_ceil(NOTES_PER_PAGE).max(1)
    }

    pub(super) fn set_page(&mut self, page: usize) {
        self.page = page.min(self.page_count() - 1);
        self.prepare_page();
    }

    // Escape only the visible quotes; thoughts use the projection prepared on
    // the background worker. Page/filter changes only clone those SharedStrings.
    pub(super) fn prepare_page(&mut self) {
        self.cards = self
            .filtered
            .iter()
            .skip(self.page * NOTES_PER_PAGE)
            .take(NOTES_PER_PAGE)
            .map(|&index| {
                let note = &self.notes[index].annotation;
                NoteCard {
                    index,
                    quote_html: plain_text_html(&note.anchor.quote).into(),
                    comment_markdown: self.comment_markdown.get(&note.id).cloned(),
                }
            })
            .collect();
    }
}
