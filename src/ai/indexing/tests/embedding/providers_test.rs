use super::*;

#[test]
pub(crate) fn embedding_and_vision_jobs_use_their_assigned_providers() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let embedding_provider = Arc::new(MockProvider::default());
    embedding_provider
        .embedding_marker
        .store(7, Ordering::SeqCst);
    let vision_provider = Arc::new(MockProvider::default());
    let coordinator =
        fixture.coordinator_with_providers(embedding_provider.clone(), vision_provider.clone());
    for kind in ["vision", "embedding"] {
        fixture
            .runtime
            .block_on(coordinator.wait_for_state(
                &format!("{kind}:{}", fixture.source_id),
                IndexingJobStatus::Succeeded,
                TEST_PROGRESS_WAIT,
            ))
            .unwrap();
    }
    fixture.wait_for_post_vision_embedding(
        &coordinator,
        "embedding-endpoint-a:embed-test",
        "vision-endpoint-a:vision-test",
    );
    assert!(embedding_provider.embedding_calls.load(Ordering::SeqCst) > 0);
    assert_eq!(
        embedding_provider.vision_png_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(vision_provider.embedding_calls.load(Ordering::SeqCst), 0);
    assert_eq!(vision_provider.vision_png_calls.load(Ordering::SeqCst), 1);

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let chunks = db::search_chunks::list_for_source(&conn, &fixture.source_id).unwrap();
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk.id.starts_with("vision-chunk-"))
    );
    for chunk in chunks {
        let (_, vector) = db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
            .unwrap()
            .unwrap();
        assert_eq!(vector[0], 7.0);
    }
}

/// 保存了新的 Endpoint/密钥（Embedding 模型、执行身份与维度都不变）之后，正在跑的 embedding
/// 任务必须换用新 provider，而不是把领取时的快照用到任务结束。
///
/// 现场 2026-09-24（翻译同类问题）：只换 API key 时任务行不会被重置，运行中的执行器却一直
/// 握着旧 provider —— embedding 与 vision 的运行原本也在领取时固定这份快照。
#[test]
pub(crate) fn a_running_embedding_uses_the_saved_provider_instead_of_its_claim_snapshot() {
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let before = Arc::new(MockProvider::default());
    before.embedding_marker.store(2, Ordering::SeqCst);
    let after = Arc::new(MockProvider::default());
    after.embedding_marker.store(9, Ordering::SeqCst);

    // 领取时的快照是旧 provider；运行期间用户保存了新 Endpoint/密钥之后，`inner.models`
    // 已经是新的那一份。
    let claimed = ModelServices {
        embedding_provider: before.clone(),
        ..inner.models_snapshot()
    };
    inner.models.write().unwrap().embedding_provider = after.clone();

    let job = running_embedding_job(&fixture, "embed-test", "embedding-endpoint-a:embed-test");
    let outcome = fixture
        .runtime
        .block_on(run_embedding(&inner, &job, &claimed))
        .unwrap();
    assert!(
        matches!(outcome, RunOutcome::Succeeded(_)),
        "换 provider 之后运行照旧走完"
    );
    assert_eq!(
        after.embedding_calls.load(Ordering::SeqCst),
        1,
        "请求必须发给保存后的 provider"
    );
    assert_eq!(
        before.embedding_calls.load(Ordering::SeqCst),
        0,
        "旧 provider 不该再收到请求"
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let chunk = db::search_chunks::list_for_source(&conn, &fixture.source_id)
        .unwrap()
        .remove(0);
    let (_, vector) = db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
        .unwrap()
        .unwrap();
    assert_eq!(vector[0], 9.0, "落库的向量来自保存后的 provider");
}

/// 保存的配置属于另一套引擎（换了 Embedding 端点或模型）时，本次运行不换 provider：那次保存
/// 会把任务行整个重置，本次运行随后会被判成已被替换，重新领取时才按新引擎继续。
#[test]
pub(crate) fn a_running_embedding_ignores_a_saved_provider_from_another_engine() {
    let fixture = Fixture::new();
    let inner = fixture.indexing_inner();
    let before = Arc::new(MockProvider::default());
    before.embedding_marker.store(2, Ordering::SeqCst);
    let after = Arc::new(MockProvider::default());
    after.embedding_marker.store(9, Ordering::SeqCst);

    let claimed = ModelServices {
        embedding_provider: before.clone(),
        ..inner.models_snapshot()
    };
    {
        let mut models = inner.models.write().unwrap();
        models.embedding_provider = after.clone();
        models.config.embedding_execution_identity = "embedding-endpoint-b:embed-test".to_string();
    }

    let job = running_embedding_job(&fixture, "embed-test", "embedding-endpoint-a:embed-test");
    fixture
        .runtime
        .block_on(run_embedding(&inner, &job, &claimed))
        .unwrap();
    assert_eq!(
        before.embedding_calls.load(Ordering::SeqCst),
        1,
        "本次运行仍然只用领取时那一套引擎"
    );
    assert_eq!(
        after.embedding_calls.load(Ordering::SeqCst),
        0,
        "本次运行不许换用另一套引擎的 provider"
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let chunk = db::search_chunks::list_for_source(&conn, &fixture.source_id)
        .unwrap()
        .remove(0);
    let (_, vector) = db::embeddings::get_f32_for_chunk_model(&conn, &chunk.id, "embed-test")
        .unwrap()
        .unwrap();
    assert_eq!(vector[0], 2.0, "落库的向量来自领取时的 provider");
}
