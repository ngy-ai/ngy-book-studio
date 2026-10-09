//! Shared fixtures for the notes tests.

use super::*;
use gpui_kit::{
    Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext, point,
};
use ngy_book_studio::annotations::{Annotation, TextAnchor};

fn note(id: usize, book: &str, kind: AnnotationKind) -> AnnotationOverview {
    AnnotationOverview {
        annotation: Annotation {
            id: format!("note-{id:03}"),
            book_id: book.into(),
            content_unit_id: format!("unit-{id}"),
            document_revision: 1,
            unit_revision: 1,
            anchor: TextAnchor {
                quote: format!("引文{id}"),
                start: 0,
                end: 1,
            },
            kind,
            comment: kind.is_comment().then(|| format!("想法{id}")),
            created_at: id as u64,
            updated_at: id as u64,
            stale: false,
        },
        book_title: book.into(),
        chapter_title: Some(format!("章节{id}")),
        chapter_index: Some(id),
    }
}

#[test]
fn filters_combine_type_with_book_chapter_quote_and_thought_search() {
    let mut listing = NoteListing::default();
    listing.replace(vec![
        note(1, "Rust 学习", AnnotationKind::Highlight),
        note(2, "Rust 学习", AnnotationKind::HumanComment),
        note(3, "Agent 实践", AnnotationKind::AiComment),
        note(4, "Agent 实践", AnnotationKind::Wavy),
        note(5, "Agent 实践", AnnotationKind::Underline),
    ]);
    assert_eq!(listing.book_count, 2);
    for query in ["rust", "  RUST  ", "章节2", "引文2", "想法2"] {
        listing.query = query.into();
        listing.filter = NoteFilter::Human;
        listing.apply_filter();
        assert_eq!(listing.filtered.len(), 1, "{query}");
        assert_eq!(listing.notes[listing.filtered[0]].annotation.id, "note-002");
    }
    listing.query.clear();
    listing.filter = NoteFilter::Marks;
    listing.apply_filter();
    assert_eq!(listing.filtered.len(), 3);
    listing.filter = NoteFilter::Ai;
    listing.apply_filter();
    assert_eq!(listing.filtered.len(), 1);
    listing.query = "Rust".into();
    listing.apply_filter();
    assert!(listing.cards.is_empty());
}

#[test]
fn pages_cover_every_result_and_reset_after_filter_or_refresh() {
    let mut listing = NoteListing::default();
    listing.replace(
        (0..65)
            .map(|id| note(id, "书", AnnotationKind::Highlight))
            .collect(),
    );
    assert_eq!(listing.page_count(), 3);
    assert_eq!(listing.cards.len(), 30);
    assert_eq!(
        listing.notes[listing.cards[0].index].annotation.updated_at,
        64
    );
    let mut visited = HashSet::new();
    for page in 0..3 {
        listing.set_page(page);
        for card in &listing.cards {
            assert!(visited.insert(listing.notes[card.index].annotation.id.clone()));
        }
    }
    assert_eq!(visited.len(), 65);
    assert_eq!(listing.cards.len(), 5);
    listing.set_page(usize::MAX);
    assert_eq!(listing.page, 2);
    listing.query = "引文64".into();
    listing.apply_filter();
    assert_eq!(listing.page, 0);
    assert_eq!(listing.cards.len(), 1);
    listing.replace(Vec::new());
    assert_eq!(listing.page, 0);
    assert_eq!(listing.page_count(), 1);
    assert_eq!(listing.book_count, 0);
    assert!(listing.cards.is_empty());
}

mod display_test;
