//! Tests for the web search fallback.

pub(crate) use super::super::*;
pub(crate) use super::*;

#[tokio::test]
pub(crate) async fn web_fallback_grounds_an_answer_when_the_books_cannot() {
    let provider = ungrounded_then_web_answer("Answer from the web [[ngy-source:web:0]].");
    let requests = Arc::clone(&provider.requests);
    let runtime = runtime(provider)
        .with_chat_generation(custom_generation())
        .unwrap()
        .with_web_search(Some(web_backend(vec![web_result(
            "Docs",
            "https://example.test/docs",
            "verified body",
        )])));

    let answer = ask(&runtime).await.unwrap();
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::VerifiedWebSources
    );
    assert_eq!(answer.citations.len(), 1);
    assert!(answer.citations[0].is_web());
    assert_eq!(
        answer.citations[0].url.as_deref(),
        Some("https://example.test/docs")
    );
    assert!(!answer.markdown.contains("[[ngy-source:"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_custom_generation(request);
    }
}

#[tokio::test]
pub(crate) async fn web_fallback_is_skipped_when_disabled() {
    let runtime = runtime(answer_only("Only general knowledge here."));
    let answer = ask(&runtime).await.unwrap();
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    assert!(answer.citations.is_empty());
}

#[tokio::test]
pub(crate) async fn web_fallback_keeps_ungrounded_answer_when_search_finds_nothing() {
    let runtime = runtime(answer_only("Only general knowledge here."))
        .with_web_search(Some(web_backend(Vec::new())));
    let answer = ask(&runtime).await.unwrap();
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    assert!(answer.citations.is_empty());
}

#[tokio::test]
pub(crate) async fn web_results_alone_do_not_ground_an_answer_the_model_does_not_cite() {
    // The host offers web sources, but the model declines to cite them.
    // The answer must still be reported as ungrounded.
    let runtime = runtime(ungrounded_then_web_answer("General knowledge only.")).with_web_search(
        Some(web_backend(vec![web_result(
            "Docs",
            "https://example.test/docs",
            "verified body",
        )])),
    );
    let answer = ask(&runtime).await.unwrap();
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::NoVerifiedSources
    );
    assert!(answer.citations.is_empty());
}

#[tokio::test]
pub(crate) async fn web_fallback_rejects_a_citation_the_host_never_served() {
    let runtime = runtime(ungrounded_then_web_answer(
        "Fabricated [[ngy-source:web:9]].",
    ))
    .with_web_search(Some(web_backend(vec![web_result(
        "Docs",
        "https://example.test/docs",
        "verified body",
    )])));
    let error = ask(&runtime).await.unwrap_err();
    assert!(
        error.to_string().contains("unknown or unserved citation"),
        "{error:#}"
    );
}

#[tokio::test]
pub(crate) async fn grounded_book_answers_never_trigger_the_web_fallback() {
    // A book-grounded answer must not issue an internet request at all.
    let runtime = runtime(search_then_answer(
        "Answer from the book [[ngy-source:passage:passage-1]].",
    ))
    .with_web_search(Some(web_backend(vec![web_result(
        "Docs",
        "https://example.test/docs",
        "verified body",
    )])));
    let answer = ask(&runtime).await.unwrap();
    assert_eq!(
        answer.source_status,
        AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
    );
    assert!(answer.citations.iter().all(|citation| !citation.is_web()));
}

#[test]
pub(crate) fn sqlite_read_passages_skips_preview_only_office_chunks() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    conn.execute_batch(
            "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document', 1, 'source-hash', 1);
             INSERT INTO books(id, title, author, format, revision, source_object_key,
                               added_at, updated_at)
                 VALUES ('book-1', 'Book', '', 'docx', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source-1', 'book-1', 1, 'docx', 'original', 'objects/source', 1);
             INSERT INTO content_units(
                 id, book_id, source_id, ordinal, kind,
                 source_locator_json, block_json, revision, created_at, updated_at)
                 VALUES ('unit-1', 'book-1', 'source-1', 0, 'section',
                         '{\"type\":\"office_section\",\"index\":1}',
                         '{\"schema_version\":1,\"blocks\":[]}', 1, 1, 1);",
        )
        .unwrap();
    let locator = serde_json::to_string(
        &DocumentLocator::unit("book-1", "unit-1")
            .with_source(SourceLocator::office_rendered_page(1)),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO search_chunks(
                 id, book_id, source_id, content_unit_id, ordinal, heading, body,
                 token_count, content_hash, locator_json, created_at)
             VALUES ('preview-chunk', 'book-1', 'source-1', 'unit-1', 0,
                     'Preview', 'must not reach the model', 5, 'preview-hash', ?1, 1)",
        [locator],
    )
    .unwrap();
    drop(conn);

    let passages = SqliteBookBackend::read_passages_sync(
        &db_path,
        ReadPassagesRequest {
            passage_ids: vec!["preview-chunk".to_string()],
            book_ids: vec!["book-1".to_string()],
        },
    )
    .unwrap();
    assert!(passages.is_empty());
}
