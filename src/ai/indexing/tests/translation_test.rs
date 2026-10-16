//! Integration tests of translation block extraction, reruns and the
//! read-only block inspector.

use super::*;

use std::sync::{Arc, Mutex, atomic::Ordering};

use crate::{ai::OpenAiCompatibleProvider, db, document::BlockDocument};

#[test]
pub(crate) fn translation_blocks_flatten_text_blocks_in_document_order() {
    use crate::document::{Block, ListItem, TableCell, TableRow};

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'h', 1);
             INSERT INTO books(id, title, author, language, format, revision,
                               source_object_key, added_at, updated_at)
                 VALUES ('book', 'Book', '', 'en', 'epub', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'epub', 'original', 'objects/source', 1);",
    )
    .unwrap();
    let document = BlockDocument {
        schema_version: 1,
        blocks: vec![
            Block::paragraph("p1", "First paragraph"),
            Block::heading("h1", 2, "Heading"),
            Block::BlockQuote {
                id: "q1".into(),
                blocks: vec![Block::paragraph("q1p", "Quote")],
            },
            Block::BulletList {
                id: "l1".into(),
                items: vec![ListItem::new(vec![Block::paragraph("li1", "Item")])],
            },
            Block::CodeBlock {
                id: "c1".into(),
                language: None,
                code: "let x = 1;".into(),
            },
            Block::Table {
                id: "t1".into(),
                header: Some(TableRow::new(vec![TableCell::text("H")])),
                rows: vec![TableRow::new(vec![TableCell::text("C")])],
            },
            Block::ThematicBreak { id: "hr".into() },
        ],
    };
    conn.execute(
        "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                 source_locator_json, title, block_json, revision, created_at, updated_at)
             VALUES ('unit', 'book', 'source', 0, 'chapter', '{}', '第一章', ?1, 1, 1, 1)",
        rusqlite::params![serde_json::to_string(&document).unwrap()],
    )
    .unwrap();

    let blocks = translation_blocks(&conn, "book", "source").unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "unit::h0", "unit::h1", "unit::h2", "unit::h3", "unit::h4", "unit::h5"
        ],
    );
    assert!(blocks.iter().all(|block| block.unit_title == "第一章"));
    assert!(blocks.iter().all(|block| block.unit_ordinal == 0));
    // 每个文本块都冻结了书名；它与章节标题一起进入模型请求的用户消息。
    assert!(blocks.iter().all(|block| block.book_title == "Book"));
    let input: serde_json::Value = serde_json::from_str(
        &blocks[0]
            .source
            .request_input(&blocks[0].translation_context()),
    )
    .unwrap();
    assert_eq!(input["book"], "Book");
    assert_eq!(input["chapter"], "第一章");
    assert_eq!(
        blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5],
    );
    assert_eq!(blocks[0].source.text, "First paragraph");
    assert_eq!(blocks[2].unit_id, "unit");

    // Every chapter of the source stays in the list. A chapter whose own
    // revision is older than the document is still translated once: its
    // revision only decides whether a stored译文 may be reused, which the
    // per-block cache check does, not whether the chapter is listed.
    assert!(blocks.iter().all(|block| block.unit_revision == 1));
}

#[test]
pub(crate) fn translation_blocks_parse_preserved_epub_html_when_the_ast_is_raw() {
    use crate::document::Block;
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'h', 1);
             INSERT INTO books(id, title, author, language, format, revision,
                               source_object_key, added_at, updated_at)
                 VALUES ('book', 'Book', '', 'en', 'epub', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'epub', 'original', 'objects/source', 1);",
    )
    .unwrap();
    // EPUB chapters are stored as one preserved RawHtml subtree; the visible
    // paragraph structure only exists in the canonical HTML source.
    let html = "<div id=\"sbo-rt-content\"><h1>Rust Brain Teasers</h1>
                    <p>Copyright 2022</p>
                    <p>Hello <b>world</b></p></div>";
    let document = BlockDocument {
        schema_version: 1,
        blocks: vec![Block::RawHtml {
            id: "raw".into(),
            source: html.into(),
            plain_text: crate::document::raw_html_plain_text(html).unwrap(),
        }],
    };
    conn.execute(
        "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                 source_locator_json, source_text, block_json, revision, created_at, updated_at)
             VALUES ('unit', 'book', 'source', 0, 'chapter', '{}', ?1, ?2, 1, 1, 1)",
        rusqlite::params![html, serde_json::to_string(&document).unwrap()],
    )
    .unwrap();

    let blocks = translation_blocks(&conn, "book", "source").unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.source.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Rust Brain Teasers", "Copyright 2022", "Hello world"],
    );
    assert!(blocks.iter().all(|block| block.unit_id == "unit"));

    // Deterministic IDs keep repeated runs idempotent for the same source.
    let again = translation_blocks(&conn, "book", "source").unwrap();
    assert_eq!(
        again
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
    );
}

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
