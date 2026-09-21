//! Tests for the library view: import, groups, search and requests.

use super::*;
use ngy_book_studio::document::{
    BlockDocument, ContentUnit, ContentUnitKind, DocumentLocator, Revision,
};

#[test]
fn search_modes_default_and_round_trip_to_selector_indices() {
    assert_eq!(SearchMode::default(), SearchMode::Hybrid);
    for mode in [
        SearchMode::Keyword,
        SearchMode::Semantic,
        SearchMode::Hybrid,
    ] {
        assert_eq!(search_mode_from_index(search_mode_index(mode)), mode);
    }
}

#[test]
fn stale_search_completion_cannot_replace_current_results() {
    assert!(is_current_search_completion(7, 7));
    assert!(!is_current_search_completion(6, 7));
    assert!(!is_current_search_completion(u64::MAX, 0));
}

#[test]
fn closing_library_never_restarts_a_nonempty_search() {
    assert!(should_start_library_search(false, "  关键词  "));
    assert!(!should_start_library_search(true, "关键词"));
    assert!(!should_start_library_search(false, "   "));
}

#[test]
fn pending_name_modal_cannot_be_dismissed_or_resync_ai_during_close() {
    assert!(name_input_modal_can_close(None));
    assert!(!name_input_modal_can_close(Some(7)));
    assert!(should_sync_library_scope(false));
    assert!(!should_sync_library_scope(true));
}

#[test]
fn older_library_projection_cannot_replace_a_newer_group_projection() {
    assert!(should_apply_library_projection(12, 12));
    assert!(should_apply_library_projection(13, 12));
    assert!(!should_apply_library_projection(11, 12));
}

#[test]
fn library_close_waits_for_every_pending_mutation() {
    let mut lifecycle = LibraryMutationLifecycle::default();
    assert!(lifecycle.begin());
    assert!(lifecycle.begin());
    assert_eq!(lifecycle.pending, 2);

    assert!(!lifecycle.request_close());
    assert!(lifecycle.close_requested());
    assert!(!lifecycle.begin());
    assert!(!lifecycle.finish());
    assert_eq!(lifecycle.pending, 1);
    assert!(lifecycle.finish());
    assert_eq!(lifecycle.pending, 0);

    assert!(!lifecycle.request_close());
}

#[test]
fn library_close_without_pending_mutations_is_ready_once() {
    let mut lifecycle = LibraryMutationLifecycle::default();
    assert!(lifecycle.request_close());
    assert!(!lifecycle.request_close());
    assert!(!lifecycle.begin());
}

#[test]
fn hybrid_passage_maps_to_existing_open_target_and_author() {
    let hit = search_hit_from_passage(
        PassageRecord {
            passage_id: "passage-1".to_string(),
            book_id: "book-1".to_string(),
            book_title: "测试图书".to_string(),
            unit_id: "unit-2".to_string(),
            unit_title: "第二章".to_string(),
            document_revision: ngy_book_studio::document::Revision::new(1),
            unit_revision: ngy_book_studio::document::Revision::new(1),
            text: "命中的正文".to_string(),
            locator: DocumentLocator::unit("book-1", "unit-2"),
            relevance: Some(0.75),
        },
        "作者".to_string(),
        1,
    );
    assert_eq!(hit.spine_index, Some(1));
    assert_eq!(hit.chapter_title.as_deref(), Some("第二章"));
    assert_eq!(hit.author, "作者");
    assert_eq!(hit.relevance, 0.75);
}

#[test]
fn original_export_extension_comes_from_canonical_source() {
    let document = BookDocument::new(
        "book-1",
        "测试图书",
        CanonicalBookSource::imported(
            BookFormat::Azw3,
            "asset-original",
            Some("original.azw3".to_string()),
        ),
    );
    assert_eq!(canonical_original_extension(&document).unwrap(), "azw3");

    let created = BookDocument::created("book-2", "新建图书");
    assert!(canonical_original_extension(&created).is_err());
}

#[test]
fn library_dispatch_binds_native_locator_to_target_document() {
    let mut document = BookDocument::new(
        "book-pdf",
        "PDF",
        CanonicalBookSource::imported(
            BookFormat::Pdf,
            "original-pdf",
            Some("book.pdf".to_string()),
        ),
    );
    document.revision = Revision::new(3);
    document.units.push(
        ContentUnit::new(
            "unit-page",
            ContentUnitKind::Page,
            "Page 1",
            "<p>page</p>",
            BlockDocument::default(),
        )
        .with_source_locator(SourceLocator::pdf_page(1)),
    );
    let unit_revision = document.units[0].revision;
    let exact = AiSourceLink {
        citation_id: "citation-pdf".to_string(),
        book_id: document.id.clone(),
        unit_id: document.units[0].id.clone(),
        unit_index: Some(0),
        document_revision: document.revision,
        unit_revision,
        locator: Some(
            DocumentLocator::unit(&document.id, &document.units[0].id)
                .with_source(SourceLocator::pdf_page(1)),
        ),
        label: "PDF page".to_string(),
        quote: None,
        selection_snapshot: false,
        stale: false,
        url: None,
    };
    assert_eq!(
        current_canonical_source_unit_index(&exact, &document),
        Ok(0)
    );

    let wrong_coordinate = AiSourceLink {
        locator: Some(
            DocumentLocator::unit(&document.id, &document.units[0].id)
                .with_source(SourceLocator::pdf_page(2)),
        ),
        ..exact.clone()
    };
    assert!(current_canonical_source_unit_index(&wrong_coordinate, &document).is_err());

    let mut corrupt_document = document.clone();
    corrupt_document.units[0].source_locator = Some(SourceLocator::slide(1));
    let wrong_type = AiSourceLink {
        locator: Some(
            DocumentLocator::unit(&document.id, &document.units[0].id)
                .with_source(SourceLocator::slide(1)),
        ),
        ..exact
    };
    assert_eq!(wrong_type.current_unit_index(&corrupt_document), Some(0));
    assert!(
        current_canonical_source_unit_index(&wrong_type, &corrupt_document).is_err(),
        "library cross-book dispatch must reject a canonical unit locator whose type conflicts with the target book"
    );
}

#[test]
fn annotation_navigation_uses_stable_unit_identity_and_rejects_stale_notes() {
    use ngy_book_studio::annotations::{AnnotationKind, TextAnchor};
    let mut document = BookDocument::created("book-notes", "笔记测试");
    for id in ["first", "second"] {
        document.units.push(ContentUnit::new(
            id,
            ContentUnitKind::Chapter,
            id,
            "<p>重复文字</p>",
            BlockDocument::default(),
        ));
    }
    let note = Annotation {
        id: "note".into(),
        book_id: document.id.clone(),
        content_unit_id: "second".into(),
        document_revision: document.revision.0,
        unit_revision: document.units[1].revision.0,
        anchor: TextAnchor {
            quote: "重复文字".into(),
            start: 0,
            end: 4,
        },
        kind: AnnotationKind::HumanComment,
        comment: Some("想法".into()),
        created_at: 1,
        updated_at: 1,
        stale: false,
    };
    assert_eq!(current_annotation_unit_index(&note, &document).unwrap(), 1);
    document.units.swap(0, 1);
    assert_eq!(current_annotation_unit_index(&note, &document).unwrap(), 0);
    let mut invalid = note.clone();
    invalid.book_id = "another-book".into();
    assert!(current_annotation_unit_index(&invalid, &document).is_err());
    invalid = note.clone();
    invalid.stale = true;
    assert!(current_annotation_unit_index(&invalid, &document).is_err());
    invalid = note.clone();
    invalid.document_revision += 1;
    assert!(current_annotation_unit_index(&invalid, &document).is_err());
    invalid = note.clone();
    invalid.unit_revision += 1;
    assert!(current_annotation_unit_index(&invalid, &document).is_err());
    document.units.remove(0);
    assert!(current_annotation_unit_index(&note, &document).is_err());
}

#[test]
fn cancelled_or_superseded_office_completion_is_stale() {
    let cancellation = OfficeCancellation::default();
    let active = OfficePreviewRun {
        request_id: 8,
        book_id: "book-8".to_string(),
        title: "测试".to_string(),
        kind: VisualPageSourceKind::OfficeEnhanced,
        initial_page: 1,
        cancellation: cancellation.clone(),
        enabling: true,
    };
    assert!(is_current_office_preview(Some(&active), 8, "book-8"));
    assert!(!is_current_office_preview(Some(&active), 7, "book-8"));
    assert!(!is_current_office_preview(Some(&active), 8, "book-9"));
    assert!(!is_current_office_preview(None, 8, "book-8"));
    cancellation.cancel();
    assert!(cancellation.is_cancelled());
}

fn test_group(id: &str, parent_id: Option<&str>) -> BookGroup {
    BookGroup {
        id: id.to_string(),
        name: id.to_string(),
        parent_id: parent_id.map(str::to_owned),
        created_at: 0,
    }
}

#[test]
fn group_picker_expands_only_the_ancestors_of_the_current_group() {
    let path = vec![
        test_group("root", None),
        test_group("child", Some("root")),
        test_group("leaf", Some("child")),
    ];
    let expanded = picker_expanded_groups(&path);
    assert_eq!(expanded.len(), 2);
    assert!(expanded.contains("root"));
    assert!(expanded.contains("child"));
    assert!(
        !expanded.contains("leaf"),
        "the current group itself stays collapsed until the user expands it"
    );
}

#[test]
fn group_picker_without_a_current_group_expands_nothing() {
    assert!(picker_expanded_groups(&[]).is_empty());
    assert!(picker_expanded_groups(&[test_group("root", None)]).is_empty());
}

#[test]
fn only_a_concrete_existing_group_files_an_import() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let group = library.create_group("导入目标", None).unwrap();

    assert_eq!(import_target_group(&library, &GroupFilter::All), None);
    assert_eq!(import_target_group(&library, &GroupFilter::Ungrouped), None);
    assert_eq!(
        import_target_group(&library, &GroupFilter::Group(group.id.clone())),
        Some(group.id.clone())
    );
    assert_eq!(
        import_target_group(&library, &GroupFilter::Group("deleted".to_string())),
        None,
        "a group missing from the library must not fail a durable import"
    );
}
