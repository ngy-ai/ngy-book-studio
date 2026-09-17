//! Tests for annotations and the document metadata they depend on.

use super::*;

fn initial_note_draft(
    library: &LibraryStore,
    book_id: &str,
    kind: AnnotationKind,
) -> AnnotationDraft {
    let document = library.document(book_id).unwrap();
    AnnotationDraft {
        content_unit_id: document.units[0].id.clone(),
        document_revision: document.revision.get(),
        unit_revision: document.units[0].revision.get(),
        anchor: TextAnchor {
            quote: "第一章".into(),
            start: 0,
            end: 3,
        },
        kind,
        comment: kind.is_comment().then(|| "对这一段的想法".to_string()),
    }
}

#[test]
fn annotations_share_storage_reopen_and_preserve_human_ai_authorship() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let book = library.create_book("阅读笔记", "作者").unwrap();
    let other = library.create_book("另一书", "作者").unwrap();
    let mut ids = Vec::new();
    for kind in [
        AnnotationKind::Highlight,
        AnnotationKind::HumanComment,
        AnnotationKind::AiComment,
    ] {
        let draft = initial_note_draft(&library, &book.id, kind);
        ids.push(library.create_annotation(&book.id, &draft).unwrap().id);
    }
    assert!(
        library
            .list_annotations(&other.id, None)
            .unwrap()
            .is_empty()
    );
    assert!(library.delete_annotation(&other.id, &ids[0]).is_err());
    assert!(
        library
            .update_human_comment(&book.id, &ids[2], "伪成人工")
            .is_err()
    );
    assert!(
        library
            .update_human_comment(&other.id, &ids[1], "跨书修改")
            .is_err()
    );
    let edited = library
        .update_human_comment(&book.id, &ids[1], "人工修改后的想法")
        .unwrap();
    assert_eq!(edited.kind, AnnotationKind::HumanComment);
    assert_eq!(edited.comment.as_deref(), Some("人工修改后的想法"));
    drop(library);

    let mut reopened = LibraryStore::load_from(temp.path().into()).unwrap();
    let notes = reopened.list_annotations(&book.id, None).unwrap();
    assert_eq!(notes.len(), 3);
    assert!(notes.iter().all(|note| !note.stale));
    assert_eq!(
        notes.iter().find(|note| note.id == ids[2]).unwrap().kind,
        AnnotationKind::AiComment
    );
    reopened.delete_annotation(&book.id, &ids[0]).unwrap();
    assert_eq!(reopened.list_annotations(&book.id, None).unwrap().len(), 2);
    reopened.remove_book(&book.id).unwrap();
    let conn = db::open_conn(reopened.database_path()).unwrap();
    assert!(
        db::annotations::list(&conn, &book.id, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn annotations_reject_wrong_book_quote_revision_and_survive_document_changes_as_stale() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let book = library.create_book("阅读笔记", "作者").unwrap();
    let other = library.create_book("另一书", "作者").unwrap();
    let draft = initial_note_draft(&library, &book.id, AnnotationKind::HumanComment);
    assert!(library.create_annotation(&other.id, &draft).is_err());
    let mut wrong = draft.clone();
    wrong.anchor.quote = "不存在".into();
    assert!(library.create_annotation(&book.id, &wrong).is_err());
    wrong = draft.clone();
    wrong.unit_revision += 1;
    assert!(library.create_annotation(&book.id, &wrong).is_err());
    wrong = draft.clone();
    wrong.document_revision += 1;
    assert!(library.create_annotation(&book.id, &wrong).is_err());
    let note = library.create_annotation(&book.id, &draft).unwrap();
    library
        .update_content_unit_source(
            &book.id,
            &draft.content_unit_id,
            "<h1>新章</h1><p>正文已经改变</p>",
        )
        .unwrap();
    assert!(library.create_annotation(&book.id, &draft).is_err());
    drop(library);
    let mut reopened = LibraryStore::load_from(temp.path().into()).unwrap();
    let notes = reopened.list_annotations(&book.id, None).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].stale);
    assert_eq!(notes[0].anchor.quote, "第一章");
    let edited = reopened
        .update_human_comment(&book.id, &note.id, "仍可整理旧版想法")
        .unwrap();
    assert!(edited.stale);
}

#[test]
fn annotations_replace_exclusive_marks_and_delete_only_the_exact_range() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let (book, first_id, second_id) = create_two_unit_book(&mut library);
    let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
    editor
        .update_unit_source(&first_id, "<h1>第一章</h1><p>第一章</p>")
        .unwrap();
    editor
        .update_unit_identity(&second_id, "第一章", ContentUnitKind::Chapter)
        .unwrap();
    editor
        .update_unit_source(&second_id, "<h1>第一章</h1>")
        .unwrap();
    library.apply_document(editor.into_document()).unwrap();
    let other = library.create_book("其他图书", "作者").unwrap();
    for kind in [AnnotationKind::HumanComment, AnnotationKind::AiComment] {
        let draft = initial_note_draft(&library, &book.id, kind);
        library.create_annotation(&book.id, &draft).unwrap();
    }

    let mut draft = initial_note_draft(&library, &book.id, AnnotationKind::Highlight);
    let first_mark = library.create_annotation(&book.id, &draft).unwrap();
    for kind in [
        AnnotationKind::Wavy,
        AnnotationKind::Underline,
        AnnotationKind::Highlight,
    ] {
        draft.kind = kind;
        let mark = library.create_annotation(&book.id, &draft).unwrap();
        assert_eq!(mark.id, first_mark.id);
        assert_eq!(mark.kind, kind);
        assert_eq!(library.create_annotation(&book.id, &draft).unwrap(), mark);
        let notes = library.list_annotations(&book.id, None).unwrap();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes.iter().filter(|note| note.kind.is_mark()).count(), 1);
        assert_eq!(
            notes
                .iter()
                .filter(|note| note.kind == AnnotationKind::HumanComment)
                .count(),
            1
        );
        assert_eq!(
            notes
                .iter()
                .filter(|note| note.kind == AnnotationKind::AiComment)
                .count(),
            1
        );
    }
    // The SQLite constraint independently prevents two mark styles at the
    // same range, while the human/AI thoughts above share that range.
    let conn = db::open_conn(library.database_path()).unwrap();
    let mut duplicate = first_mark.clone();
    duplicate.id.push_str("-conflict");
    duplicate.kind = AnnotationKind::Wavy;
    let error = db::annotations::insert(&conn, &duplicate).unwrap_err();
    assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
    assert!(
        conn.execute(
            "UPDATE annotations SET kind = 'strikethrough' WHERE id = ?1",
            [&first_mark.id]
        )
        .is_err()
    );
    drop(conn);

    let mut duplicate_quote = draft.clone();
    duplicate_quote.anchor.start = 3;
    duplicate_quote.anchor.end = 6;
    let duplicate_mark = library
        .create_annotation(&book.id, &duplicate_quote)
        .unwrap();
    let mut other_chapter = draft.clone();
    other_chapter.content_unit_id = second_id;
    let other_chapter_mark = library.create_annotation(&book.id, &other_chapter).unwrap();
    let other_draft = initial_note_draft(&library, &other.id, AnnotationKind::Underline);
    let other_book_mark = library.create_annotation(&other.id, &other_draft).unwrap();

    assert!(
        library
            .delete_annotation_marks(
                &other.id,
                &draft.content_unit_id,
                other_draft.document_revision,
                other_draft.unit_revision,
                &draft.anchor
            )
            .is_err()
    );
    let mut wrong_anchor = draft.anchor.clone();
    wrong_anchor.quote = "不存在".into();
    assert!(
        library
            .delete_annotation_marks(
                &book.id,
                &draft.content_unit_id,
                draft.document_revision,
                draft.unit_revision,
                &wrong_anchor
            )
            .is_err()
    );
    for _ in 0..2 {
        library
            .delete_annotation_marks(
                &book.id,
                &draft.content_unit_id,
                draft.document_revision,
                draft.unit_revision,
                &draft.anchor,
            )
            .unwrap();
    }
    let notes = library.list_annotations(&book.id, None).unwrap();
    let range_notes = notes
        .iter()
        .filter(|note| {
            note.content_unit_id == draft.content_unit_id
                && note.anchor.start == draft.anchor.start
                && note.anchor.end == draft.anchor.end
        })
        .collect::<Vec<_>>();
    assert_eq!(range_notes.len(), 2);
    assert!(range_notes.iter().all(|note| note.kind.is_comment()));
    assert!(notes.iter().any(|note| note.id == duplicate_mark.id));
    assert!(notes.iter().any(|note| note.id == other_chapter_mark.id));
    assert_eq!(
        library.list_annotations(&other.id, None).unwrap(),
        vec![other_book_mark]
    );

    // Late requests cannot erase marks anchored in another revision.
    let document = library.document(&book.id).unwrap();
    library.apply_document(document).unwrap();
    assert!(
        library
            .delete_annotation_marks(
                &book.id,
                &duplicate_quote.content_unit_id,
                duplicate_quote.document_revision,
                duplicate_quote.unit_revision,
                &duplicate_quote.anchor
            )
            .is_err()
    );
    let mut conn = db::open_conn(library.database_path()).unwrap();
    assert!(
        db::transactions::delete_annotation_marks(
            &mut conn,
            &book.id,
            &duplicate_quote.content_unit_id,
            duplicate_quote.document_revision,
            duplicate_quote.unit_revision,
            &duplicate_quote.anchor
        )
        .is_err()
    );
    drop(conn);
    drop(library);
    let reopened = LibraryStore::load_from(temp.path().into()).unwrap();
    let notes = reopened.list_annotations(&book.id, None).unwrap();
    assert_eq!(notes.len(), 4);
    assert!(notes.iter().all(|note| note.stale));
    assert!(notes.iter().any(|note| note.id == duplicate_mark.id));
}

/// PDF page notes reuse the EPUB rules for storage, ownership, revisions
/// and exclusive marks. Only the canonical-text comparison differs: a page
/// anchor addresses the pinned PDF.js text layer, which no host extractor
/// reproduces, so the range is checked against its own quote instead.
#[test]
fn pdf_page_notes_share_storage_and_scope_rules_without_reading_page_text() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let book = library.create_book("PDF 笔记", "作者").unwrap();
    let chapter_book = library.create_book("重排图书", "作者").unwrap();
    let document = library.document(&book.id).unwrap();
    let page_id = document.units[0].id.clone();
    let mut editor = DocumentEditor::new(document).unwrap();
    editor
        .update_unit_identity(&page_id, "第 1 页", ContentUnitKind::Page)
        .unwrap();
    library.apply_document(editor.into_document()).unwrap();
    let document = library.document(&book.id).unwrap();
    let mut draft = AnnotationDraft {
        content_unit_id: page_id.clone(),
        document_revision: document.revision.get(),
        unit_revision: document.units[0].revision.get(),
        anchor: TextAnchor {
            quote: "页面 选区".into(),
            start: 12,
            end: 16,
        },
        kind: AnnotationKind::Highlight,
        comment: None,
    };

    // The very same draft is not a valid reflowable chapter anchor.
    assert!(library.create_annotation(&book.id, &draft).is_err());
    let mark = library.create_pdf_annotation(&book.id, &draft).unwrap();
    for kind in [AnnotationKind::Wavy, AnnotationKind::Underline] {
        draft.kind = kind;
        let replaced = library.create_pdf_annotation(&book.id, &draft).unwrap();
        assert_eq!(replaced.id, mark.id);
        assert_eq!(replaced.kind, kind);
    }
    let mut thoughts = Vec::new();
    for kind in [AnnotationKind::HumanComment, AnnotationKind::AiComment] {
        let mut thought = draft.clone();
        thought.kind = kind;
        thought.comment = Some("对这一页的想法".to_string());
        thoughts.push(
            library
                .create_pdf_annotation(&book.id, &thought)
                .unwrap()
                .id,
        );
    }
    assert_eq!(library.list_annotations(&book.id, None).unwrap().len(), 3);

    for invalid in [
        {
            // A range that cannot describe its own quote.
            let mut invalid = draft.clone();
            invalid.anchor.end += 1;
            invalid
        },
        {
            let mut invalid = draft.clone();
            invalid.anchor.quote = "   ".into();
            invalid
        },
        {
            let mut invalid = draft.clone();
            invalid.document_revision += 1;
            invalid
        },
        {
            let mut invalid = draft.clone();
            invalid.unit_revision += 1;
            invalid
        },
        {
            // A reflowable chapter is never a PDF page position.
            let other = library.document(&chapter_book.id).unwrap();
            let mut invalid = draft.clone();
            invalid.content_unit_id = other.units[0].id.clone();
            invalid.document_revision = other.revision.get();
            invalid.unit_revision = other.units[0].revision.get();
            invalid
        },
    ] {
        assert!(
            library.create_pdf_annotation(&book.id, &invalid).is_err(),
            "{invalid:?}"
        );
    }
    assert!(
        library
            .create_pdf_annotation(&chapter_book.id, &draft)
            .is_err()
    );

    // Removing the mark keeps both thoughts on the identical range.
    library
        .delete_pdf_annotation_marks(
            &book.id,
            &page_id,
            draft.document_revision,
            draft.unit_revision,
            &draft.anchor,
        )
        .unwrap();
    drop(library);
    let reopened = LibraryStore::load_from(temp.path().into()).unwrap();
    let notes = reopened.list_annotations(&book.id, Some(&page_id)).unwrap();
    assert_eq!(notes.len(), 2);
    assert!(
        notes
            .iter()
            .all(|note| note.kind.is_comment() && !note.stale)
    );
    assert!(thoughts.iter().all(|id| notes.iter().any(|n| &n.id == id)));
}

#[test]
fn annotations_verify_duplicate_offsets_unicode_and_exported_heading() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let book = library.create_book("定位", "作者").unwrap();
    let unit_id = library.document(&book.id).unwrap().units[0].id.clone();
    library
        .update_content_unit_source(&book.id, &unit_id, "<p>甲😀重复</p><p>乙重复</p>")
        .unwrap();
    let document = library.document(&book.id).unwrap();
    let mut draft = AnnotationDraft {
        content_unit_id: unit_id,
        document_revision: document.revision.get(),
        unit_revision: document.units[0].revision.get(),
        anchor: TextAnchor {
            quote: "重 复".into(),
            start: 9,
            end: 11,
        },
        kind: AnnotationKind::Highlight,
        comment: None,
    };
    let note = library.create_annotation(&book.id, &draft).unwrap();
    assert_eq!(note.anchor.start, 9);
    draft.anchor.start = 8;
    draft.anchor.end = 10;
    assert!(library.create_annotation(&book.id, &draft).is_err());
    draft.anchor = TextAnchor {
        quote: "😀".into(),
        start: 4,
        end: 6,
    };
    library.create_annotation(&book.id, &draft).unwrap();
    draft.anchor.end = 5;
    assert!(library.create_annotation(&book.id, &draft).is_err());
}

#[test]
fn annotations_retain_deleted_chapter_and_transaction_rejects_late_old_revision() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let (book, first_unit_id, _) = create_two_unit_book(&mut library);
    let draft = initial_note_draft(&library, &book.id, AnnotationKind::HumanComment);
    let note = library.create_annotation(&book.id, &draft).unwrap();
    let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
    editor.remove_unit(&first_unit_id).unwrap();
    library.apply_document(editor.into_document()).unwrap();
    let mut conn = db::open_conn(library.database_path()).unwrap();
    let mut late_note = note.clone();
    late_note.id.push_str("-late");
    assert!(db::transactions::insert_annotation(&mut conn, &late_note).is_err());
    drop(conn);
    drop(library);
    let reopened = LibraryStore::load_from(temp.path().into()).unwrap();
    let notes = reopened.list_annotations(&book.id, None).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].stale);
    assert_eq!(notes[0].content_unit_id, first_unit_id);
    assert_eq!(notes[0].comment, note.comment);
}

#[test]
fn annotation_overview_reads_all_books_and_chapters_with_current_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    assert!(library.annotation_overview(None).unwrap().is_empty());
    let (book, first_unit, second_unit) = create_two_unit_book(&mut library);
    let empty = library.create_book("没有笔记", "作者").unwrap();
    let old_projection = library.clone();
    let other = library.create_book("另一书", "作者").unwrap();
    assert!(
        old_projection
            .books()
            .iter()
            .all(|item| item.id != other.id)
    );

    for kind in [AnnotationKind::Highlight, AnnotationKind::HumanComment] {
        let draft = initial_note_draft(&library, &book.id, kind);
        library.create_annotation(&book.id, &draft).unwrap();
    }
    let document = library.document(&book.id).unwrap();
    let mut ai_draft = initial_note_draft(&library, &book.id, AnnotationKind::AiComment);
    ai_draft.content_unit_id = second_unit.clone();
    ai_draft.unit_revision = document.units[1].revision.get();
    ai_draft.anchor.quote = "第二章".into();
    let ai_note = library.create_annotation(&book.id, &ai_draft).unwrap();
    let mut other_draft = initial_note_draft(&library, &other.id, AnnotationKind::Wavy);
    library.create_annotation(&other.id, &other_draft).unwrap();
    other_draft.kind = AnnotationKind::Underline;
    other_draft.anchor.quote = "第一".into();
    other_draft.anchor.end = 2;
    library.create_annotation(&other.id, &other_draft).unwrap();

    // Deterministic timestamps exercise both newest-first order and ties.
    let conn = db::open_conn(library.database_path()).unwrap();
    conn.execute(
        "UPDATE annotations SET updated_at = (SELECT MAX(created_at) + 1 FROM annotations)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE annotations SET updated_at = updated_at + 1 WHERE id = ?1",
        [&ai_note.id],
    )
    .unwrap();
    drop(conn);
    let notes = old_projection.annotation_overview(None).unwrap();
    assert_eq!(notes.len(), 5);
    assert_eq!(notes[0].annotation.id, ai_note.id);
    assert!(
        notes[1..]
            .windows(2)
            .all(|pair| { pair[0].annotation.id < pair[1].annotation.id })
    );
    assert!(notes.iter().all(|note| !note.annotation.stale));
    for kind in [
        AnnotationKind::Highlight,
        AnnotationKind::Wavy,
        AnnotationKind::Underline,
        AnnotationKind::HumanComment,
        AnnotationKind::AiComment,
    ] {
        assert!(notes.iter().any(|note| note.annotation.kind == kind));
    }
    let scoped = old_projection.annotation_overview(Some(&book.id)).unwrap();
    assert_eq!(scoped.len(), 3);
    assert!(
        scoped
            .iter()
            .all(|note| { note.annotation.book_id == book.id && note.book_title == book.title })
    );
    assert_eq!(scoped[0].chapter_title.as_deref(), Some("第二章"));
    assert_eq!(scoped[0].chapter_index, Some(1));
    assert!(scoped[1..].iter().all(|note| {
        note.chapter_title.as_deref() == Some("第一章") && note.chapter_index == Some(0)
    }));
    assert!(
        library
            .annotation_overview(Some(&empty.id))
            .unwrap()
            .is_empty()
    );
    assert!(library.annotation_overview(Some("missing-book")).is_err());

    let mut editor = DocumentEditor::new(document).unwrap();
    editor
        .set_metadata("更新后的书名", vec!["作者".into()], None, None)
        .unwrap();
    editor
        .update_unit_identity(&first_unit, "改名的第一章", ContentUnitKind::Chapter)
        .unwrap();
    editor.remove_unit(&second_unit).unwrap();
    library.apply_document(editor.into_document()).unwrap();
    let scoped = old_projection.annotation_overview(Some(&book.id)).unwrap();
    assert_eq!(scoped.len(), 3);
    assert!(
        scoped
            .iter()
            .all(|note| note.annotation.stale && note.book_title == "更新后的书名")
    );
    let deleted_chapter = &scoped[0];
    assert_eq!(deleted_chapter.annotation.id, ai_note.id);
    assert_eq!(deleted_chapter.annotation.anchor, ai_note.anchor);
    assert_eq!(deleted_chapter.annotation.comment, ai_note.comment);
    assert_eq!(deleted_chapter.chapter_title, None);
    assert_eq!(deleted_chapter.chapter_index, None);
    assert!(scoped[1..].iter().all(|note| {
        note.chapter_title.as_deref() == Some("改名的第一章") && note.chapter_index == Some(0)
    }));
    library.remove_book(&other.id).unwrap();
    assert_eq!(old_projection.annotation_overview(None).unwrap().len(), 3);
    assert!(old_projection.annotation_overview(Some(&other.id)).is_err());
}

#[test]
fn annotation_overview_never_uses_foreign_book_or_source_chapter_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let book = library.create_book("阅读笔记", "作者").unwrap();
    let other = library.create_book("另一书", "作者").unwrap();
    let draft = initial_note_draft(&library, &book.id, AnnotationKind::HumanComment);
    let mut note = library.create_annotation(&book.id, &draft).unwrap();
    note.id.push_str("-foreign-unit");
    note.content_unit_id = library.document(&other.id).unwrap().units[0].id.clone();
    // Fault-injected persisted row bypasses the host's ownership checks.
    let conn = db::open_conn(library.database_path()).unwrap();
    db::annotations::insert(&conn, &note).unwrap();
    let overview = library.annotation_overview(Some(&book.id)).unwrap();
    let foreign = overview
        .iter()
        .find(|entry| entry.annotation.id == note.id)
        .unwrap();
    assert!(foreign.annotation.stale);
    assert_eq!(foreign.chapter_title, None);
    assert_eq!(foreign.chapter_index, None);
    assert_eq!(foreign.book_title, book.title);
    assert!(
        library
            .annotation_overview(Some(&other.id))
            .unwrap()
            .is_empty()
    );
    let other_source = db::book_sources::get_revision(&conn, &other.id, other.revision)
        .unwrap()
        .unwrap();
    conn.execute(
            "UPDATE content_units SET source_id = ?1,
                ordinal = (SELECT COALESCE(MAX(ordinal), -1) + 1 FROM content_units WHERE source_id = ?1)
             WHERE id = ?2",
            rusqlite::params![other_source.id, draft.content_unit_id],
        )
        .unwrap();
    assert!(
        library
            .annotation_overview(Some(&book.id))
            .unwrap()
            .iter()
            .all(|entry| {
                entry.annotation.stale
                    && entry.chapter_title.is_none()
                    && entry.chapter_index.is_none()
            })
    );
}
