//! Tests for scope filtering and backend result shaping.

use super::super::*;
use super::*;

#[tokio::test]
async fn passage_results_never_expose_book_or_unit_identifiers() {
    // A model that sees `book-a` copies it into [[ngy-source:...]], which
    // the host must reject, failing the whole answer.
    let search = MockSearch {
        results: Arc::new(vec![clean_passage()]),
        ..Default::default()
    };
    let mut agent = ReadOnlyAgent::new(
        search,
        MockBooks::default(),
        scope(),
        AgentLimits::default(),
    )
    .unwrap();
    let result = agent
        .execute_tool(&tool_call(SEARCH_BOOKS_TOOL, json!({"query": "visible"})))
        .await
        .unwrap();
    assert!(result.content.contains("passage:p-1"));
    assert!(result.content.contains("visible text"));
    assert!(!result.content.contains("book-a"), "{}", result.content);
    assert!(!result.content.contains("unit-1"), "{}", result.content);
    assert_eq!(result.citations[0].book_id, "book-a");
}

#[tokio::test]
async fn outline_results_never_expose_book_or_unit_identifiers() {
    let books = MockBooks {
        outlines: Arc::new(vec![BookOutlineRecord {
            book_id: "book-a".to_string(),
            book_title: "Alpha".to_string(),
            nodes: vec![OutlineNodeRecord {
                title: "Chapter 1".to_string(),
                locator: DocumentLocator::unit("book-a", "unit-1"),
                children: Vec::new(),
            }],
        }]),
        ..Default::default()
    };
    let mut agent = ReadOnlyAgent::new(
        MockSearch::default(),
        books,
        scope(),
        AgentLimits::default(),
    )
    .unwrap();
    let result = agent
        .execute_tool(&tool_call(GET_OUTLINE_TOOL, json!({})))
        .await
        .unwrap();
    assert!(result.content.contains("Alpha"));
    assert!(result.content.contains("Chapter 1"));
    assert!(!result.content.contains("book-a"), "{}", result.content);
    assert!(!result.content.contains("unit-1"), "{}", result.content);
}

#[test]
fn authorized_book_listing_never_exposes_internal_book_ids() {
    let agent = ReadOnlyAgent::new(
        MockSearch::default(),
        MockBooks::default(),
        scope(),
        AgentLimits::default(),
    )
    .unwrap();
    let messages = agent
        .question_messages("question", &[("book-a".to_string(), "Alpha".to_string())])
        .unwrap();
    let serialized = serde_json::to_string(&messages[0]).unwrap();
    assert!(serialized.contains("Alpha"));
    assert!(
        !serialized.contains("book-a"),
        "system prompt must not leak internal book ids: {serialized}"
    );
}

#[tokio::test]
async fn model_book_ids_can_narrow_but_cannot_expand_host_scope() {
    let search = MockSearch::default();
    let requests = search.requests.clone();
    let mut agent = ReadOnlyAgent::new(
        search,
        MockBooks::default(),
        scope(),
        AgentLimits::default(),
    )
    .unwrap();

    let denied = tool_call(
        SEARCH_BOOKS_TOOL,
        json!({"query": "secret", "book_ids": ["book-secret"]}),
    );
    assert_eq!(
        agent.execute_tool(&denied).await.unwrap_err(),
        AgentError::ScopeViolation
    );
    assert!(requests.lock().unwrap().is_empty());

    let allowed = tool_call(
        SEARCH_BOOKS_TOOL,
        json!({"query": "chapter", "book_ids": ["book-b"]}),
    );
    agent.execute_tool(&allowed).await.unwrap();
    assert_eq!(requests.lock().unwrap()[0].book_ids, vec!["book-b"]);
}

#[tokio::test]
async fn backend_results_are_filtered_against_scope_and_requested_ids() {
    let search = MockSearch {
        results: Arc::new(vec![
            passage("book-a", "public", "visible"),
            passage("book-secret", "secret", "must not leak"),
            passage("book-b", "not-requested", "also hidden"),
        ]),
        ..Default::default()
    };
    let mut agent = ReadOnlyAgent::new(
        search,
        MockBooks::default(),
        scope(),
        AgentLimits::default(),
    )
    .unwrap();
    let result = agent
        .execute_tool(&tool_call(
            SEARCH_BOOKS_TOOL,
            json!({"query": "visible", "book_ids": ["book-a"]}),
        ))
        .await
        .unwrap();
    assert!(result.content.contains("visible"));
    assert!(!result.content.contains("must not leak"));
    assert!(!result.content.contains("also hidden"));
    assert_eq!(result.citations.len(), 1);
    assert_eq!(result.citations[0].book_id, "book-a");
}

#[tokio::test]
async fn preview_only_office_passages_are_rejected_for_search_and_read_tools() {
    let mut preview = passage("book-a", "preview-only", "must not reach the model");
    preview.locator = DocumentLocator::unit("book-a", "unit-1")
        .with_source(SourceLocator::office_rendered_page(1));
    let search = MockSearch {
        results: Arc::new(vec![preview.clone()]),
        ..Default::default()
    };
    let books = MockBooks {
        passages: Arc::new(vec![preview]),
        ..Default::default()
    };
    let mut agent = ReadOnlyAgent::new(search, books, scope(), AgentLimits::default()).unwrap();

    let searched = agent
        .execute_tool(&tool_call(
            SEARCH_BOOKS_TOOL,
            json!({"query": "preview", "book_ids": ["book-a"]}),
        ))
        .await
        .unwrap();
    assert!(searched.citations.is_empty());
    assert!(!searched.content.contains("must not reach the model"));

    let read = agent
        .execute_tool(&tool_call(
            READ_PASSAGES_TOOL,
            json!({
                "passage_ids": ["preview-only"],
                "book_ids": ["book-a"]
            }),
        ))
        .await
        .unwrap();
    assert!(read.citations.is_empty());
    assert!(!read.content.contains("must not reach the model"));
}
