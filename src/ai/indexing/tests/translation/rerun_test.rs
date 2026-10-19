use super::*;

#[test]
pub(crate) fn retranslating_a_translation_job_resets_it_and_clears_its_rows() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let cursor_json = db::index_jobs::get(&conn, &job_id)
        .unwrap()
        .unwrap()
        .cursor_json;
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &cursor_json,
        None,
        10,
        Some(10),
        Some(10),
    )
    .unwrap();
    db::translations::upsert(
        &conn,
        &db::translations::NewTranslation {
            book_id: fixture.book_id.clone(),
            content_unit_id: fixture.unit_id.clone(),
            block_id: "stale".into(),
            ordinal: 0,
            document_revision: fixture.revision,
            unit_revision: fixture.revision,
            target_language: "zh-Hans".into(),
            source_language: Some("en".into()),
            model: "chat-test".into(),
            source_text: "Alpha".into(),
            translated_text: "甲".into(),
            created_at: 10,
            updated_at: 10,
        },
    )
    .unwrap();
    drop(conn);

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate(&job_id))
            .unwrap()
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(job.status, db::index_jobs::IndexJobStatus::Paused);
    assert_eq!(job.error, None);
    let cursor = serde_json::from_str::<JobCursor>(&job.cursor_json).unwrap();
    assert_eq!(cursor.next_ordinal, 0);
    assert!(
        db::translations::list_for_unit(&conn, &fixture.unit_id, "zh-Hans")
            .unwrap()
            .is_empty(),
        "re-translating discards the previous rows"
    );

    // Other kinds are never touched by a translation-only action.
    let other = format!("embedding:{}", fixture.source_id);
    assert!(
        !fixture
            .runtime
            .block_on(coordinator.retranslate(&other))
            .unwrap()
    );
}

#[test]
pub(crate) fn retranslating_a_translation_job_keeps_a_manual_edit_and_clears_the_rest() {
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let manual_text = serde_json::to_string(&crate::ai::translation::StoredTranslation {
        execution_identity: "translation-v1:chat-test".to_string(),
        segments: vec![crate::ai::translation::TranslationSegment {
            source: "Alpha".to_string(),
            translated: "人工甲".to_string(),
        }],
    })
    .unwrap();
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        for (block_id, ordinal) in [("edited", 0), ("plain", 1)] {
            db::translations::upsert(
                &conn,
                &db::translations::NewTranslation {
                    book_id: fixture.book_id.clone(),
                    content_unit_id: fixture.unit_id.clone(),
                    block_id: block_id.to_string(),
                    ordinal,
                    document_revision: fixture.revision,
                    unit_revision: fixture.revision,
                    target_language: "zh-Hans".to_string(),
                    source_language: Some("en".to_string()),
                    model: "chat-test".to_string(),
                    source_text: "Alpha".to_string(),
                    translated_text: "甲".to_string(),
                    created_at: 10,
                    updated_at: 10,
                },
            )
            .unwrap();
        }
        db::translations::set_manual_text(
            &conn,
            &fixture.book_id,
            &fixture.unit_id,
            "edited",
            "zh-Hans",
            fixture.revision,
            fixture.revision,
            Some(&manual_text),
            20,
        )
        .unwrap();
    }

    assert!(
        fixture
            .runtime
            .block_on(coordinator.retranslate(&job_id))
            .unwrap()
    );
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let rows = db::translations::list_for_unit(&conn, &fixture.unit_id, "zh-Hans").unwrap();
    assert_eq!(
        rows.len(),
        1,
        "re-translating clears the machine rows and keeps the reader's own text"
    );
    assert_eq!(rows[0].block_id, "edited");
    assert_eq!(rows[0].manual_text.as_deref(), Some(manual_text.as_str()));
}

#[test]
pub(crate) fn changing_the_translation_model_resumes_at_the_same_block() {
    // 现场 2026-09-22：暂停在「已处理三千多」的任务，改翻译模型后变成三百多 —— 重配置
    // 刻意保留了位置，但它重建的游标漏了 `blocks_version`，执行器于是把这次重建读成
    // 「块列表变过」而把任务退回书首重扫。用户只想要后者：换模型从原来的位置继续。
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-one".to_string(),
            "translation-v1:chat-one".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    // 把任务推到一个「已经翻过若干块」的位置：这道题只关心位置本身。
    finish_job_at(&fixture, &job_id, 7);

    // 换模型（模型设置里保存新的「翻译模型」走的就是这一条）。
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider,
            "chat-two".to_string(),
            "translation-v1:chat-two".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(job.status, db::index_jobs::IndexJobStatus::Paused);
    let stored = serde_json::from_str::<JobCursor>(&job.cursor_json).unwrap();
    assert_eq!(stored.model.as_deref(), Some("chat-two"));
    assert_eq!(stored.next_ordinal, 7);
    assert_eq!(stored.blocks_version, TRANSLATION_BLOCKS_VERSION);
    // 执行器领取前会把这一行重写成规范编码，那个读数才是任务窗口的「已处理」。
    let bound = JobCursor::from_job(&job, "chat-two", Some("translation-v1:chat-two")).unwrap();
    assert_eq!(bound.next_ordinal, 7, "换模型必须从原来的位置继续");
}
