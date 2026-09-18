//! Tests for reference validation, freezing and citation mapping.

use super::*;

#[test]
pub(crate) fn controller_rejects_preview_only_and_mismatched_locators() {
    let allowed = vec!["book-a".to_string()];
    let mut preview_only = reference("book-a", "unit-1", 0);
    preview_only.locator = Some(
        DocumentLocator::unit("book-a", "unit-1")
            .with_source(SourceLocator::office_rendered_page(1)),
    );
    assert!(
        validate_reference_request(&[preview_only], &allowed)
            .unwrap_err()
            .to_string()
            .contains("派生数据")
    );

    let mut source_locator = reference("book-a", "unit-1", 0);
    source_locator.locator =
        Some(DocumentLocator::unit("book-a", "unit-1").with_source(SourceLocator::pdf_page(1)));
    assert!(validate_reference_request(&[source_locator], &allowed).is_ok());

    let mut mismatched = reference("book-a", "unit-1", 0);
    mismatched.locator = Some(DocumentLocator::unit("book-a", "unit-2"));
    assert!(
        validate_reference_request(&[mismatched], &allowed)
            .unwrap_err()
            .to_string()
            .contains("不一致")
    );
}

#[test]
pub(crate) fn live_and_persisted_citations_keep_only_valid_exact_locators() {
    let locator = DocumentLocator::unit("book-a", "unit-1").with_source(SourceLocator::pdf_page(2));
    let live = source_link_from_agent_citation(AgentCitation {
        citation_id: "passage:p-1".to_string(),
        book_id: "book-a".to_string(),
        book_title: "Book".to_string(),
        unit_id: "unit-1".to_string(),
        unit_title: "Unit".to_string(),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        quote: "evidence".to_string(),
        locator: locator.clone(),
        source_kind: AgentCitationSourceKind::Book,
        url: None,
        source_title: None,
    })
    .expect("valid live citation");
    assert_eq!(live.locator.as_ref(), Some(&locator));
    assert!(!live.selection_snapshot);

    let live_selection = source_link_from_agent_citation(AgentCitation {
        citation_id: "selection:host-issued".to_string(),
        book_id: "book-a".to_string(),
        book_title: "Book".to_string(),
        unit_id: "unit-1".to_string(),
        unit_title: "Unit".to_string(),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        quote: "unsaved selection".to_string(),
        locator: locator.clone(),
        source_kind: AgentCitationSourceKind::Book,
        url: None,
        source_title: None,
    })
    .expect("valid live selection citation");
    assert!(live_selection.selection_snapshot);

    let restored = source_link_from_chat_citation(ChatCitation {
        id: "citation-1".to_string(),
        content_unit_id: Some("unit-1".to_string()),
        search_chunk_id: Some("p-1".to_string()),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        quote: "evidence".to_string(),
        locator: locator.clone(),
        source_kind: AgentCitationSourceKind::Book,
        url: None,
        source_title: None,
        created_at: 1,
    })
    .expect("valid persisted citation");
    assert_eq!(restored.locator.as_ref(), Some(&locator));
    assert!(!restored.selection_snapshot);

    assert!(
        source_link_from_agent_citation(AgentCitation {
            citation_id: "passage:forged".to_string(),
            book_id: "book-a".to_string(),
            book_title: "Book".to_string(),
            unit_id: "unit-2".to_string(),
            unit_title: "Other".to_string(),
            document_revision: Revision::new(1),
            unit_revision: Revision::new(1),
            quote: "forged".to_string(),
            locator,
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
        })
        .is_none()
    );
}

#[test]
pub(crate) fn web_citations_keep_a_url_instead_of_a_book_locator() {
    let link = source_link_from_chat_citation(ChatCitation {
        id: "web-citation".to_string(),
        content_unit_id: None,
        search_chunk_id: None,
        document_revision: Revision::new(0),
        unit_revision: Revision::new(0),
        quote: "verified snippet".to_string(),
        locator: DocumentLocator::unit("", ""),
        created_at: 1,
        source_kind: AgentCitationSourceKind::Web,
        url: Some("https://example.test/docs".to_string()),
        source_title: Some("Docs".to_string()),
    })
    .expect("web citation must survive restoration");
    assert_eq!(link.url.as_deref(), Some("https://example.test/docs"));
    assert_eq!(link.label, "Docs");
    assert!(link.locator.is_none());
    assert!(link.book_id.is_empty());
}

#[test]
pub(crate) fn restored_source_status_uses_rows_before_ui_source_filtering() {
    let thread_id = "thread-source-status".to_string();
    let cited = StoredChatMessage {
        id: "assistant-cited".to_string(),
        thread_id: thread_id.clone(),
        parent_id: None,
        ordinal: 0,
        role: ChatRole::Assistant,
        content: "grounded answer".to_string(),
        model: Some("mock".to_string()),
        created_at: 1,
        citations: vec![ChatCitation {
            id: "citation-invalid-for-ui".to_string(),
            content_unit_id: Some("unit-a".to_string()),
            search_chunk_id: Some("chunk-a".to_string()),
            document_revision: Revision::new(1),
            unit_revision: Revision::new(1),
            quote: "evidence".to_string(),
            locator: DocumentLocator::unit("book-a", "different-unit"),
            created_at: 1,
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
        }],
    };
    let uncited = StoredChatMessage {
        id: "assistant-uncited".to_string(),
        thread_id: thread_id.clone(),
        parent_id: Some(cited.id.clone()),
        ordinal: 1,
        role: ChatRole::Assistant,
        content: "general answer".to_string(),
        model: Some("mock".to_string()),
        created_at: 2,
        citations: Vec::new(),
    };
    let restored = restored_messages(ChatSession {
        thread: ChatThread {
            id: thread_id,
            primary_book_id: None,
            title: "Question".to_string(),
            scope: ChatScope::default(),
            window_kind: ChatWindowKind::Library,
            created_at: 1,
            updated_at: 2,
        },
        messages: vec![cited, uncited],
    });

    assert!(restored[0].sources.is_empty());
    assert_eq!(
        restored[0].source_status,
        Some(AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources),
        "a persisted citation row must keep the answer grounded even when its UI link is rejected"
    );
    assert_eq!(
        restored[1].source_status,
        Some(AgentAnswerSourceStatus::NoVerifiedSources)
    );
}

#[test]
pub(crate) fn persisted_reference_checks_current_revision_and_text_bounds() {
    let unit = ContentUnit::new(
        "unit-1",
        ContentUnitKind::Chapter,
        "One",
        "<p>body</p>",
        BlockDocument::new(vec![Block::paragraph("block-1", "body")]),
    );
    let mut document = BookDocument::created("book-a", "Book");
    document.units.push(unit);
    let unit = &document.units[0];

    let mut reference = reference("book-a", "unit-1", 0);
    reference.revision = Some(unit.revision.get());
    reference.locator = Some(DocumentLocator::text("book-a", "unit-1", "block-1", 0, 4));
    assert!(validate_persisted_reference(&document, unit, &reference).is_ok());

    reference.revision = Some(unit.revision.get() + 1);
    assert!(validate_persisted_reference(&document, unit, &reference).is_err());
    reference.revision = Some(unit.revision.get());
    reference.locator = Some(DocumentLocator::text("book-a", "unit-1", "block-1", 0, 99));
    assert!(validate_persisted_reference(&document, unit, &reference).is_err());

    reference.locator =
        Some(DocumentLocator::unit("book-a", "unit-1").with_source(SourceLocator::pdf_page(1)));
    assert!(
        validate_persisted_reference(&document, unit, &reference)
            .unwrap_err()
            .to_string()
            .contains("原始来源位置")
    );
}

#[test]
pub(crate) fn combined_snapshot_payload_has_a_hard_total_byte_limit() {
    let within = vec![
        SelectionSnapshot::capture("book-a", "unit-1", 1, "a".repeat(1024)).unwrap(),
        SelectionSnapshot::capture("book-a", "unit-2", 2, "b".repeat(1024)).unwrap(),
    ];
    assert!(validate_snapshot_payload(&within).is_ok());

    let oversized = vec![
        SelectionSnapshot::capture(
            "book-a",
            "unit-large",
            3,
            "x".repeat(MAX_REFERENCE_TOTAL_BYTES + 1),
        )
        .unwrap(),
    ];
    assert!(
        validate_snapshot_payload(&oversized)
            .unwrap_err()
            .to_string()
            .contains("总大小")
    );
}

#[test]
pub(crate) fn reference_freeze_stops_when_the_prepared_request_is_cancelled() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();
    let cancellation = AgentCancellation::default();
    cancellation.cancel();

    let error = runtime
        .block_on(freeze_references(services, Vec::new(), &[], cancellation))
        .expect_err("cancelled freeze must stop before reading the library");
    assert!(error.to_string().contains("cancelled"));
    assert!(error.is::<AgentRequestCancelled>());
    assert_eq!(error_kind(&error), "cancelled");
}

#[test]
pub(crate) fn restored_unsaved_selection_is_stale_when_current_ast_cannot_prove_it() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
    let record = library
        .create_book("Selection restore", "Author")
        .expect("create book");
    let document = library.document(&record.id).expect("load document");
    let unit = document.units.first().expect("default chapter");
    let locator = match unit.source_locator.clone() {
        Some(source) => DocumentLocator::unit(&document.id, &unit.id).with_source(source),
        None => DocumentLocator::unit(&document.id, &unit.id),
    };
    let session = ChatSession {
        thread: ChatThread {
            id: "thread-selection".to_string(),
            primary_book_id: None,
            title: "Question".to_string(),
            scope: ChatScope::new([document.id.clone()]).unwrap(),
            window_kind: ChatWindowKind::Library,
            created_at: 1,
            updated_at: 1,
        },
        messages: vec![StoredChatMessage {
            id: "assistant-selection".to_string(),
            thread_id: "thread-selection".to_string(),
            parent_id: None,
            ordinal: 0,
            role: ChatRole::Assistant,
            content: "Answer".to_string(),
            model: Some("mock".to_string()),
            created_at: 1,
            citations: vec![ChatCitation {
                id: "stored-selection".to_string(),
                content_unit_id: Some(unit.id.clone()),
                search_chunk_id: None,
                document_revision: document.revision,
                unit_revision: unit.revision,
                quote: "unsaved text absent from the current AST".to_string(),
                locator,
                created_at: 1,
                source_kind: AgentCitationSourceKind::Book,
                url: None,
                source_title: None,
            }],
        }],
    };
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
    let runtime = services.runtime();

    let restored = runtime
        .block_on(validated_restored_messages(services, session))
        .expect("restore messages");
    let source = &restored[0].sources[0];
    assert!(source.selection_snapshot);
    assert!(source.stale);
}
