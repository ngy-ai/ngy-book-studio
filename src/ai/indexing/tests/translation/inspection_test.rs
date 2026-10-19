use super::*;

/// 配置的翻译生成参数必须真的进请求体。检查器展示的就是工作线程会发出的那一份（同一条
/// `translation_request`），所以在这里断言端到端一致：设置里改温度/采样/上限，请求体
/// 跟着变；关闭推理仍是固定项。
#[test]
pub(crate) fn translation_block_inspection_shows_the_configured_generation_parameters() {
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
            TranslationGenerationSettings {
                temperature: Some(0.6),
                top_p: Some(0.9),
                max_output_tokens: 8192,
                presence_penalty: None,
                frequency_penalty: None,
            },
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let detail = fixture
        .runtime
        .block_on(coordinator.translation_block_detail(&job_id, 0))
        .unwrap();
    assert_eq!(detail.request.temperature, Some(0.6));
    assert_eq!(detail.request.max_tokens, 8192);
    let body: serde_json::Value = serde_json::from_str(&detail.request.body).unwrap();
    assert_eq!(body["temperature"], 0.6);
    assert_eq!(body["top_p"], 0.9);
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["reasoning_effort"], "none");
}

#[test]
pub(crate) fn translation_block_inspection_is_revision_pinned_and_read_only() {
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

    let before = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    let list = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job_id))
        .unwrap();
    assert_eq!(list.target_language, "zh-Hans");
    assert!(
        list.total >= 2,
        "created book has a heading and a paragraph"
    );
    assert_eq!(list.total, list.blocks.len());
    assert_eq!(list.next_ordinal, 0);
    assert_eq!(list.blocks[0].ordinal, 0);
    assert_eq!(list.blocks[0].unit_ordinal, 0);
    assert_eq!(list.blocks[0].unit_title, "第一章");
    assert!(list.blocks[0].source_preview.contains("第一章"));
    assert!(
        list.blocks
            .iter()
            .all(|block| !block.source_preview.is_empty())
    );
    assert_eq!(
        list.blocks
            .iter()
            .map(|block| block.ordinal)
            .collect::<Vec<_>>(),
        (0..list.total).collect::<Vec<_>>(),
    );

    // Inspection must never claim, advance, or publish the inspected job.
    let after = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(before.status, after.status);
    assert_eq!(before.cursor_json, after.cursor_json);
    assert_eq!(before.updated_at, after.updated_at);

    // Only translation tasks expose blocks, and an unknown task is rejected.
    assert!(
        fixture
            .runtime
            .block_on(coordinator.translation_blocks(&format!("embedding:{}", fixture.source_id)))
            .is_err()
    );
    assert!(
        fixture
            .runtime
            .block_on(coordinator.translation_blocks("translation:missing:zh-Hans"))
            .is_err()
    );
}

#[test]
pub(crate) fn translation_block_detail_rebuilds_the_worker_request_and_stays_read_only() {
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

    let before = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    let list = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job_id))
        .unwrap();
    let detail = fixture
        .runtime
        .block_on(coordinator.translation_block_detail(&job_id, 0))
        .unwrap();

    // The detail describes the same block the list previews.
    assert_eq!(detail.ordinal, 0);
    assert_eq!(detail.unit_title, list.blocks[0].unit_title);
    assert_eq!(detail.unit_ordinal, list.blocks[0].unit_ordinal);
    assert!(detail.unit_revision <= 1);
    assert!(!detail.unit_id.is_empty());
    assert!(detail.block_id.starts_with(&detail.unit_id));
    assert!(!detail.segments.is_empty());
    assert!(!detail.source_text.is_empty());
    // The preview the list shows is cut from the very text the body sends.
    assert!(
        detail
            .source_text
            .contains(list.blocks[0].source_preview.trim_end_matches('…')),
        "the full block must contain its own preview"
    );

    // The rebuilt body is the real wire payload, not a summary of it.
    let parsed = serde_json::from_str::<serde_json::Value>(&detail.request.body)
        .expect("the body must be serializable JSON");
    assert_eq!(parsed["model"], "chat-test");
    assert_eq!(detail.request.model, "chat-test");
    assert_eq!(detail.request.target_language, "zh-Hans");
    assert_eq!(
        parsed["messages"][0]["content"],
        detail.request.system_prompt
    );
    assert_eq!(
        parsed["messages"][1]["content"],
        detail.request.user_content
    );
    assert_eq!(
        parsed["max_tokens"],
        serde_json::json!(DEFAULT_TRANSLATION_OUTPUT_TOKENS)
    );
    // The worker's prompt carries the format contract; a body without it
    // would not be the request the task actually sends.
    assert!(detail.request.system_prompt.contains("translations"));

    // 书名与章节标题随正文一起进入用户消息：上下文只帮助模型消歧，不出现在
    // 片段列表里，因此窗口展示的正文就是工作线程会发出的那一份。
    let user: serde_json::Value = serde_json::from_str(&detail.request.user_content).unwrap();
    assert_eq!(user["book"], "Index test");
    assert_eq!(user["chapter"], detail.unit_title);

    // Reading one block never claims, advances, or publishes the task.
    let after = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(before.status, after.status);
    assert_eq!(before.cursor_json, after.cursor_json);
    assert_eq!(before.updated_at, after.updated_at);

    // An ordinal outside the task, another task kind, and an unknown task
    // are all rejected rather than answered with a wrong block.
    assert!(
        fixture
            .runtime
            .block_on(coordinator.translation_block_detail(&job_id, list.total))
            .is_err()
    );
    assert!(
        fixture
            .runtime
            .block_on(
                coordinator
                    .translation_block_detail(&format!("embedding:{}", fixture.source_id), 0)
            )
            .is_err()
    );
    assert!(
        fixture
            .runtime
            .block_on(coordinator.translation_block_detail("translation:missing:zh-Hans", 0))
            .is_err()
    );
}

/// 失败块的现场重放：把模型**原样**的回答端出来，并且除这一次外部请求之外不产生
/// 任何副作用 —— 不改游标、不写译文、不记诊断。
#[test]
pub(crate) fn replaying_a_failed_block_returns_the_raw_answer_and_changes_nothing() {
    let provider = Arc::new(MockMergeProvider::default());
    // 先让模型回一个「答了、但形状不对」的答案：空 translations，正是文本块被跳过
    // 的那种形状。
    provider.reject_requests.store(1, Ordering::SeqCst);
    let fixture = Fixture::new();
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            "translation-v1:chat-test".to_string(),
            TranslationGenerationSettings::default(),
            NO_BATCHING,
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();

    let before = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    let probe = fixture
        .runtime
        .block_on(coordinator.probe_translation_block_response(&job_id, 0))
        .unwrap();

    // 原样：没有去围栏、没有 trim、没有美化，字节数也按原样报。
    assert_eq!(probe.ordinal, 0);
    assert_eq!(probe.model, "chat-test");
    assert_eq!(probe.response_text, r#"{"translations":[]}"#);
    assert_eq!(probe.response_bytes, probe.response_text.len());
    assert_eq!(probe.end, "done");
    // mock 只给 content 和 done，没有 finish_reason。
    assert_eq!(probe.finish_reason, None);
    // 判定来自工作进程自己的 `parse_response`，不是查看入口另写一套规则：
    // 空片段数不等于原文片段数，就是它被跳过时记下的那个分类。
    assert_eq!(probe.decoded_segments, None);
    assert_eq!(
        probe.decode_error_kind,
        Some(crate::ai::job_diagnostics::JobLogErrorKind::SegmentCountMismatch)
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);

    // 只读：状态、游标、更新时间一律不动，任务日志里也没有任何新事件。
    let after = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::get(&conn, &job_id).unwrap().unwrap()
    };
    assert_eq!(before.status, after.status);
    assert_eq!(before.cursor_json, after.cursor_json);
    assert_eq!(before.updated_at, after.updated_at);
    assert!(recorded_events(&fixture, &job_id).is_empty());

    // 截断的答案也要带原文回来：那恰恰是用户要看的东西（残缺的 JSON 本身）。
    provider.truncated.store(true, Ordering::SeqCst);
    let probe = fixture
        .runtime
        .block_on(coordinator.probe_translation_block_response(&job_id, 0))
        .unwrap();
    assert_eq!(probe.end, "unexpected_finish_reason");
    assert_eq!(probe.finish_reason, Some("length"));
    assert_eq!(
        probe.response_text,
        "{\"translations\":[{\"id\":0,\"text\":\"译文"
    );
    assert_eq!(
        probe.decode_error_kind,
        Some(crate::ai::job_diagnostics::JobLogErrorKind::IncompleteJson)
    );
    // 这个形状来自容器扫描（括号没闭合就走到流尾），拿不到行列坐标；窗口的判定行
    // 按「坐标缺省就不写」处理。带坐标的那条路径由 UI 侧用例固定。
    assert_eq!(probe.decode_error_line, None);
    assert_eq!(probe.decode_error_column, None);
    assert_eq!(provider.requests.load(Ordering::SeqCst), 2);

    // 越界的块号、别的任务种类、不存在的任务都在发请求之前就被拒掉。
    let total = fixture
        .runtime
        .block_on(coordinator.translation_blocks(&job_id))
        .unwrap()
        .total;
    assert!(
        fixture
            .runtime
            .block_on(coordinator.probe_translation_block_response(&job_id, total))
            .is_err()
    );
    assert!(
        fixture
            .runtime
            .block_on(
                coordinator.probe_translation_block_response(
                    &format!("embedding:{}", fixture.source_id),
                    0
                )
            )
            .is_err()
    );
    assert!(
        fixture
            .runtime
            .block_on(
                coordinator.probe_translation_block_response("translation:missing:zh-Hans", 0)
            )
            .is_err()
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 2);

    // provider 直接失败时没有答案可显示，按错误上抛，而不是回一个空正文让窗口
    // 以为模型什么都没说。
    provider.fail_provider.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .runtime
            .block_on(coordinator.probe_translation_block_response(&job_id, 0))
            .is_err()
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 3);
}
