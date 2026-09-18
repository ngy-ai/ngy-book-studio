//! Tests for hybrid recall, scope rechecks and the vector index.

use super::super::*;
use super::*;

#[tokio::test]
pub(crate) async fn endpoint_switch_during_query_embedding_falls_back_to_scoped_fts() {
    let fixture = Fixture::new();
    fixture.set_embedding_execution("endpoint-a:mock-model");
    fixture.put_semantic_only_hit();
    let mut query = request(SearchMode::Semantic, vec!["book-a"]);
    query.query = "alpha".to_string();
    let current = SearchService::new_with_execution_identity(
        &fixture.path,
        Arc::new(MockProvider::new(vec![1.0, 0.0])),
        "mock-model",
        2,
        "endpoint-a:mock-model",
    )
    .unwrap();
    let hits = current.search(query.clone()).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].passage_id, "book-a-chunk-1");

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut provider = MockProvider::new(vec![1.0, 0.0]);
    provider.response_gate = Some((entered.clone(), release.clone()));
    let snapshot = SearchService::new_with_execution_identity(
        &fixture.path,
        Arc::new(provider),
        "mock-model",
        2,
        "endpoint-a:mock-model",
    )
    .unwrap();
    let pending_query = query.clone();
    let pending = tokio::spawn(async move { snapshot.search(pending_query).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();

    // Same model name and dimensions, but the endpoint's vector space has
    // changed while the old snapshot's provider response was pending.
    fixture.set_embedding_execution("endpoint-b:mock-model");
    fixture.put_semantic_only_hit();
    release.notify_one();
    let hits = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].passage_id, "book-a-chunk-0");
    assert_eq!(hits[0].book_id, "book-a");

    let replacement = SearchService::new_with_execution_identity(
        &fixture.path,
        Arc::new(MockProvider::new(vec![1.0, 0.0])),
        "mock-model",
        2,
        "endpoint-b:mock-model",
    )
    .unwrap();
    let hits = replacement.search(query).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].passage_id, "book-a-chunk-1");
}

#[test]
pub(crate) fn sqlite_execution_identity_filter_rejects_unbound_and_malformed_jobs() {
    let fixture = Fixture::new();
    fixture.put_semantic_only_hit();
    let lookup = || {
        SqliteVectorIndex
            .exact_search_for_execution(
                &fixture.path,
                "mock-model",
                "endpoint-a:mock-model",
                &[1.0, 0.0],
                &["book-a".to_string()],
                10,
            )
            .unwrap()
    };
    assert!(lookup().is_empty());
    fixture.set_embedding_execution("endpoint-a:mock-model");
    fixture.put_semantic_only_hit();
    assert_eq!(lookup().len(), 1);
    let conn = connection::open_conn(&fixture.path).unwrap();
    for cursor in ["null", "{"] {
        conn.execute(
            "UPDATE index_jobs SET cursor_json = ?1
                 WHERE id = 'embedding:book-a-source'",
            [cursor],
        )
        .unwrap();
        assert!(lookup().is_empty());
    }
}

#[tokio::test]
pub(crate) async fn empty_scope_short_circuits_without_database_or_embedding_access() {
    let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
    let vector_index = Arc::new(FailingVectorIndex::default());
    let service = SearchService::with_vector_index(
        PathBuf::from("database-must-not-be-opened.db"),
        provider.clone(),
        "mock-model",
        2,
        vector_index.clone(),
    )
    .unwrap();

    let hits = service
        .search(request(SearchMode::Hybrid, Vec::new()))
        .await
        .unwrap();

    assert!(hits.is_empty());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(vector_index.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
pub(crate) async fn hybrid_falls_back_to_scoped_fts_when_vector_search_fails() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
    let vector_index = Arc::new(FailingVectorIndex::default());
    let service = SearchService::with_vector_index(
        &fixture.path,
        provider.clone(),
        "mock-model",
        2,
        vector_index.clone(),
    )
    .unwrap();

    let hits = service
        .search(request(SearchMode::Hybrid, vec!["book-a"]))
        .await
        .unwrap();

    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit.book_id == "book-a"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(vector_index.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        vector_index.scopes.lock().unwrap().as_slice(),
        &[vec!["book-a".to_string()]]
    );
}

#[tokio::test]
pub(crate) async fn service_rechecks_scope_for_untrusted_vector_hits() {
    let fixture = Fixture::new();
    let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
    let vector_index = Arc::new(StaticVectorIndex {
        hits: vec![
            VectorHit {
                search_chunk_id: "book-b-chunk-0".to_string(),
                distance: 0.0,
            },
            VectorHit {
                search_chunk_id: "book-a-chunk-0".to_string(),
                distance: 0.1,
            },
        ],
    });
    let service =
        SearchService::with_vector_index(&fixture.path, provider, "mock-model", 2, vector_index)
            .unwrap();

    let hits = service
        .search(request(SearchMode::Semantic, vec!["book-a"]))
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].passage_id, "book-a-chunk-0");
}

#[tokio::test]
pub(crate) async fn preview_only_office_chunks_are_excluded_from_keyword_and_semantic_search() {
    let fixture = Fixture::new();
    let conn = connection::open_conn(&fixture.path).unwrap();
    let locator = serde_json::to_string(
        &DocumentLocator::unit("book-a", "book-a-unit")
            .with_source(SourceLocator::office_rendered_page(1)),
    )
    .unwrap();
    conn.execute(
        "UPDATE search_chunks SET locator_json = ?2 WHERE id = ?1",
        params!["book-a-chunk-0", locator],
    )
    .unwrap();
    drop(conn);

    let provider = Arc::new(MockProvider::new(vec![1.0, 0.0]));
    let vector_index = Arc::new(StaticVectorIndex {
        hits: vec![
            VectorHit {
                search_chunk_id: "book-a-chunk-0".to_string(),
                distance: 0.0,
            },
            VectorHit {
                search_chunk_id: "book-a-chunk-1".to_string(),
                distance: 0.1,
            },
        ],
    });
    let service =
        SearchService::with_vector_index(&fixture.path, provider, "mock-model", 2, vector_index)
            .unwrap();

    for mode in [SearchMode::Keyword, SearchMode::Semantic] {
        let hits = service.search(request(mode, vec!["book-a"])).await.unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| hit.passage_id.as_str())
                .collect::<Vec<_>>(),
            vec!["book-a-chunk-1"]
        );
    }
}

#[test]
pub(crate) fn sqlite_vector_index_upserts_reads_and_exactly_ranks_with_hard_scope() {
    let fixture = Fixture::new();
    let index = SqliteVectorIndex;
    for (id, chunk_id, vector) in [
        ("embedding-a-near", "book-a-chunk-0", vec![0.9, 0.1]),
        ("embedding-a-far", "book-a-chunk-1", vec![0.0, 1.0]),
        ("embedding-b-exact", "book-b-chunk-0", vec![1.0, 0.0]),
    ] {
        index
            .upsert(
                &fixture.path,
                &ChunkEmbedding {
                    id: id.to_string(),
                    search_chunk_id: chunk_id.to_string(),
                    model: "mock-model".to_string(),
                    vector,
                    created_at: 7,
                },
            )
            .unwrap();
    }

    let stored = index
        .get(&fixture.path, "book-a-chunk-0", "mock-model")
        .unwrap()
        .unwrap();
    assert_eq!(stored.vector, vec![0.9, 0.1]);

    let hits = index
        .exact_search(
            &fixture.path,
            "mock-model",
            &[1.0, 0.0],
            &["book-a".to_string()],
            10,
        )
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.search_chunk_id.as_str())
            .collect::<Vec<_>>(),
        vec!["book-a-chunk-0", "book-a-chunk-1"]
    );
    assert!(hits[0].distance < hits[1].distance);
    assert!(
        hits.iter()
            .all(|hit| !hit.search_chunk_id.starts_with("book-b"))
    );
    assert!(
        index
            .exact_search(&fixture.path, "mock-model", &[1.0, 0.0], &[], 10,)
            .unwrap()
            .is_empty()
    );

    let conn = connection::open_conn(&fixture.path).unwrap();
    let locator = serde_json::to_string(
        &DocumentLocator::unit("book-a", "book-a-unit")
            .with_source(SourceLocator::office_rendered_page(1)),
    )
    .unwrap();
    conn.execute(
        "UPDATE search_chunks SET locator_json = ?2 WHERE id = ?1",
        params!["book-a-chunk-0", locator],
    )
    .unwrap();
    drop(conn);
    let hits = index
        .exact_search(
            &fixture.path,
            "mock-model",
            &[1.0, 0.0],
            &["book-a".to_string()],
            10,
        )
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.search_chunk_id.as_str())
            .collect::<Vec<_>>(),
        vec!["book-a-chunk-1"]
    );
}

#[test]
pub(crate) fn reciprocal_rank_fusion_rewards_passages_found_by_both_rankers() {
    fn ranked(id: &str) -> RankedPassage {
        RankedPassage {
            record: PassageRecord {
                passage_id: id.to_string(),
                book_id: "book-a".to_string(),
                book_title: "Book".to_string(),
                unit_id: "unit".to_string(),
                unit_title: "Unit".to_string(),
                document_revision: Revision::new(1),
                unit_revision: Revision::new(1),
                text: id.to_string(),
                locator: DocumentLocator::unit("book-a", "unit"),
                relevance: None,
            },
        }
    }

    let fused = reciprocal_rank_fusion(
        vec![ranked("a"), ranked("b")],
        vec![ranked("b"), ranked("c")],
        3,
    );

    assert_eq!(fused[0].passage_id, "b");
    assert!(fused[0].relevance.unwrap() > fused[1].relevance.unwrap());
}
