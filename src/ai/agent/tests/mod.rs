//! Shared fixtures for the read-only agent tests.

use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use futures_util::FutureExt as _;

use super::*;

#[derive(Clone, Default)]
struct MockSearch {
    requests: Arc<Mutex<Vec<SearchRequest>>>,
    results: Arc<Vec<PassageRecord>>,
}

impl SearchBackend for MockSearch {
    fn search(&self, request: SearchRequest) -> BoxFuture<'_, AnyResult<Vec<PassageRecord>>> {
        self.requests.lock().unwrap().push(request);
        let results = self.results.clone();
        async move { Ok(results.as_ref().clone()) }.boxed()
    }
}

#[derive(Clone, Default)]
struct MockBooks {
    passage_requests: Arc<Mutex<Vec<ReadPassagesRequest>>>,
    outline_requests: Arc<Mutex<Vec<OutlineRequest>>>,
    passages: Arc<Vec<PassageRecord>>,
    outlines: Arc<Vec<BookOutlineRecord>>,
}

impl BookBackend for MockBooks {
    fn read_passages(
        &self,
        request: ReadPassagesRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<PassageRecord>>> {
        self.passage_requests.lock().unwrap().push(request);
        let passages = self.passages.clone();
        async move { Ok(passages.as_ref().clone()) }.boxed()
    }

    fn get_outline(
        &self,
        request: OutlineRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<BookOutlineRecord>>> {
        self.outline_requests.lock().unwrap().push(request);
        let outlines = self.outlines.clone();
        async move { Ok(outlines.as_ref().clone()) }.boxed()
    }
}

fn scope() -> AllowedBookScope {
    AllowedBookScope::new(["book-a", "book-b"]).unwrap()
}

fn passage(book_id: &str, passage_id: &str, text: &str) -> PassageRecord {
    PassageRecord {
        passage_id: passage_id.to_string(),
        book_id: book_id.to_string(),
        book_title: format!("Title {book_id}"),
        unit_id: "unit-1".to_string(),
        unit_title: "Chapter 1".to_string(),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        text: text.to_string(),
        locator: DocumentLocator::unit(book_id, "unit-1"),
        relevance: Some(0.5),
    }
}

fn tool_call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "call-1".to_string(),
        kind: "function".to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

#[test]
fn tool_schemas_are_closed_and_only_expose_read_only_tools() {
    let definitions = agent_tool_definitions();
    assert_eq!(definitions.len(), 3);
    assert_eq!(definitions[0].function.name, SEARCH_BOOKS_TOOL);
    assert_eq!(definitions[1].function.name, READ_PASSAGES_TOOL);
    assert_eq!(definitions[2].function.name, GET_OUTLINE_TOOL);
    assert!(definitions.iter().all(|definition| {
        definition.function.parameters["additionalProperties"] == Value::Bool(false)
    }));
}

fn clean_passage() -> PassageRecord {
    PassageRecord {
        passage_id: "p-1".to_string(),
        book_id: "book-a".to_string(),
        book_title: "Alpha".to_string(),
        unit_id: "unit-1".to_string(),
        unit_title: "Chapter 1".to_string(),
        document_revision: Revision::new(1),
        unit_revision: Revision::new(1),
        text: "visible text".to_string(),
        locator: DocumentLocator::unit("book-a", "unit-1"),
        relevance: Some(0.5),
    }
}

mod limits_test;

mod scope_test;

mod stream_test;
