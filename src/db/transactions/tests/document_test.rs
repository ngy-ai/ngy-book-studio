//! Tests for document CRUD, revisions and derivative job reconciliation.

pub(super) use super::super::*;
pub(super) use super::*;

#[test]
fn document_and_supporting_crud_round_trip() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    let derivative_jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
    assert_eq!(derivative_jobs.len(), 3);
    assert!(derivative_jobs.iter().all(|job| {
        matches!(job.kind.as_str(), "embedding" | "vision" | "visual_render")
            && job.status == IndexJobStatus::Queued
            && job.source_id.as_deref() == Some("source-1")
    }));
    assert!(derivative_jobs.iter().all(|job| {
        serde_json::from_str::<serde_json::Value>(&job.cursor_json)
            .is_ok_and(|cursor| cursor["revision"] == 1 || cursor["spec"]["document_revision"] == 1)
    }));

    assert_eq!(
        books::get(&conn, "book-1").unwrap(),
        Some(fixture.book.clone())
    );
    assert_eq!(
        book_sources::get(&conn, "source-1").unwrap(),
        Some(fixture.source.clone())
    );
    assert_eq!(
        blobs::get(&conn, "objects/source-1").unwrap(),
        Some(fixture.source_blob.clone())
    );
    assert_eq!(
        progress::get(&conn, "book-1").unwrap(),
        Some(fixture.progress.clone())
    );
    assert_eq!(
        content_units::list_for_source(&conn, "source-1").unwrap(),
        fixture.units
    );
    assert_eq!(
        toc_entries::list_for_source(&conn, "source-1").unwrap(),
        fixture.toc
    );
    assert_eq!(
        assets::list_for_source(&conn, "source-1").unwrap(),
        fixture.assets
    );
    assert_eq!(
        asset_refs::list_for_unit(&conn, "unit-1").unwrap(),
        fixture.refs
    );
    assert_eq!(
        search_chunks::list_for_source(&conn, "source-1").unwrap(),
        fixture.chunks
    );
    assert_eq!(
        visual_pages::list_for_source(&conn, "source-1").unwrap(),
        fixture.pages
    );
    assert!(blobs::list_unreferenced(&conn).unwrap().is_empty());

    let hits = book_search::search(&conn, None, &["document".to_string()], 10).unwrap();
    assert!(hits.iter().any(|hit| hit.spine_index == Some(2)));

    let embedding = Embedding {
        id: "embedding-1".to_string(),
        search_chunk_id: "chunk-1".to_string(),
        model: "test-model".to_string(),
        dimensions: 2,
        vector: vec![0, 1, 2, 3, 4, 5, 6, 7],
        created_at: 2,
    };
    embeddings::upsert(&conn, &embedding).unwrap();
    assert_eq!(
        embeddings::get(&conn, "embedding-1").unwrap(),
        Some(embedding)
    );

    let job = IndexJob {
        id: "job-1".to_string(),
        book_id: "book-1".to_string(),
        source_id: Some("source-1".to_string()),
        kind: "embedding".to_string(),
        status: IndexJobStatus::Queued,
        pause_requested: false,
        cancel_requested: false,
        attempts: 0,
        cursor_json: "{}".to_string(),
        error: None,
        created_at: 2,
        updated_at: 2,
        started_at: None,
        finished_at: None,
    };
    index_jobs::insert(&conn, &job).unwrap();
    // 排队中的任务没有执行者：暂停必须当场结清。只留下 flag 会让这行谁也领不走
    // （claim 要求 pause_requested = 0），界面于是永远显示「正在暂停」。
    index_jobs::request_pause(&conn, "job-1", 3).unwrap();
    let paused = index_jobs::get(&conn, "job-1").unwrap().unwrap();
    assert_eq!(paused.status, IndexJobStatus::Paused);
    assert!(!paused.pause_requested);
    index_jobs::request_cancel(&conn, "job-1", 4).unwrap();
    let cancelled = index_jobs::get(&conn, "job-1").unwrap().unwrap();
    // 取消一条已暂停的任务直接落终态：留着 cancel_requested 只会重演「谁都不消费」。
    assert_eq!(cancelled.status, IndexJobStatus::Cancelled);
    assert!(!cancelled.pause_requested);
    assert!(!cancelled.cancel_requested);

    let thread = ChatThread {
        id: "thread-1".to_string(),
        book_id: Some("book-1".to_string()),
        title: "章节问答".to_string(),
        scope_json: r#"{"book_ids":["book-1"]}"#.to_string(),
        window_kind: "reader".to_string(),
        created_at: 2,
        updated_at: 2,
    };
    chat_threads::insert(&conn, &thread).unwrap();
    let message = ChatMessage {
        id: "message-1".to_string(),
        thread_id: thread.id.clone(),
        parent_id: None,
        ordinal: 0,
        role: "assistant".to_string(),
        content: "回答".to_string(),
        model: Some("test-model".to_string()),
        created_at: 3,
    };
    let citation = ChatCitation {
        id: "citation-1".to_string(),
        message_id: message.id.clone(),
        content_unit_id: Some("unit-1".to_string()),
        search_chunk_id: Some("chunk-1".to_string()),
        ordinal: 0,
        quote: "document body".to_string(),
        document_revision: 1,
        unit_revision: 1,
        locator_json: Some(fixture.chunks[0].locator_json.clone()),
        source_kind: "book".to_string(),
        url: None,
        source_title: None,
        created_at: 3,
    };
    insert_chat_message(&mut conn, &message, std::slice::from_ref(&citation), 3).unwrap();
    assert_eq!(
        chat_messages::list_for_thread(&conn, "thread-1").unwrap(),
        vec![message.clone()]
    );
    assert_eq!(
        chat_citations::list_for_message(&conn, "message-1").unwrap(),
        vec![citation.clone()]
    );
    assert_eq!(
        chat_threads::get(&conn, "thread-1")
            .unwrap()
            .unwrap()
            .updated_at,
        3
    );

    chat_threads::update_context(
        &conn,
        "thread-1",
        "章节问答",
        r#"{"book_ids":[]}"#,
        "reader",
        4,
    )
    .unwrap();
    let rejected_message = ChatMessage {
        id: "message-after-scope-change".to_string(),
        thread_id: thread.id.clone(),
        parent_id: Some(message.id.clone()),
        ordinal: 1,
        role: "assistant".to_string(),
        content: "不应写入".to_string(),
        model: Some("test-model".to_string()),
        created_at: 4,
    };
    let rejected_citation = ChatCitation {
        id: "citation-after-scope-change".to_string(),
        message_id: rejected_message.id.clone(),
        created_at: 4,
        ..citation.clone()
    };
    let error = insert_chat_message(
        &mut conn,
        &rejected_message,
        std::slice::from_ref(&rejected_citation),
        4,
    )
    .unwrap_err();
    assert!(error.to_string().contains("授权范围"));
    assert!(
        chat_messages::get(&conn, &rejected_message.id)
            .unwrap()
            .is_none(),
        "scope validation and message insertion must share one transaction"
    );
    assert!(
        chat_citations::get(&conn, &rejected_citation.id)
            .unwrap()
            .is_none()
    );

    let setting = Setting {
        key: "reader.theme".to_string(),
        value_json: r#"{"name":"paper"}"#.to_string(),
        updated_at: 2,
    };
    settings::upsert(&conn, &setting).unwrap();
    assert_eq!(settings::get(&conn, "reader.theme").unwrap(), Some(setting));
    assert!(crate::db::schema::is_current(&conn).unwrap());
}

#[test]
fn deleting_a_mapped_unit_cascades_its_derived_visual_pages() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    conn.execute("DELETE FROM content_units WHERE id = ?1", ["unit-1"])
        .unwrap();

    assert!(
        visual_pages::list_for_source(&conn, "source-1")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn newer_revision_drops_old_derivative_jobs_and_queues_current_work() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let next = fixture.revision_two();
    let _ = install_document_revision(&mut conn, &next.graph(), true).unwrap();

    let jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
    assert_eq!(jobs.len(), 3);
    assert!(
        jobs.iter()
            .all(|job| job.source_id.as_deref() != Some("source-1"))
    );
    assert!(
        jobs.iter()
            .filter(|job| {
                job.source_id.as_deref() == Some("source-2") && job.status == IndexJobStatus::Queued
            })
            .count()
            == 3
    );
}

#[test]
fn disabled_auto_run_pauses_import_and_replacement_derivative_jobs() {
    fn assert_paused_jobs(jobs: &[IndexJob], source_id: &str) {
        assert_eq!(jobs.len(), 3);
        assert!(jobs.iter().all(|job| {
            matches!(job.kind.as_str(), "embedding" | "vision" | "visual_render")
                && job.status == IndexJobStatus::Paused
                && job.source_id.as_deref() == Some(source_id)
                && !job.pause_requested
                && !job.cancel_requested
                && job.attempts == 0
                && job.started_at.is_none()
                && job.finished_at.is_none()
        }));
    }

    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), false).unwrap();
    let imported_jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
    assert_paused_jobs(&imported_jobs, "source-1");
    let imported_job_ids = imported_jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<Vec<_>>();

    let next = fixture.revision_two();
    let _ = install_document_revision(&mut conn, &next.graph(), false).unwrap();
    let replacement_jobs = index_jobs::list_for_book(&conn, "book-1").unwrap();
    assert_paused_jobs(&replacement_jobs, "source-2");
    assert!(
        imported_job_ids
            .iter()
            .all(|job_id| index_jobs::get(&conn, job_id).unwrap().is_none()),
        "the superseded source must not retain paused derivative jobs"
    );
}

/// 重启恢复要把暂停请求一并结清（现场 2026-10-17）：进程退出时正在暂停的任务若带着
/// flag 回到队列，这行既领不走（claim 要求 pause_requested = 0）也没有执行者消费它，
/// 界面于是永远显示「正在暂停」，重启多少次都一样。旧构建写下的「排队中 + flag」行
/// 按同一条规则修复；没有暂停请求的运行中任务照旧回到队列。
#[test]
fn restart_recovery_settles_a_pause_request_no_executor_can_observe() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let embedding_id = "embedding:source-1";

    // 进程退出时正在暂停：行还在 running，flag 已经写下。
    let running = index_jobs::get(&conn, embedding_id).unwrap().unwrap();
    index_jobs::update_state(
        &conn,
        embedding_id,
        IndexJobStatus::Running,
        &running.cursor_json,
        None,
        10,
        Some(10),
        None,
    )
    .unwrap();
    index_jobs::request_pause(&conn, embedding_id, 11).unwrap();
    assert!(
        index_jobs::get(&conn, embedding_id)
            .unwrap()
            .unwrap()
            .pause_requested
    );

    index_jobs::recover_interrupted(&conn, 12).unwrap();
    let paused = index_jobs::get(&conn, embedding_id).unwrap().unwrap();
    assert_eq!(paused.status, IndexJobStatus::Paused);
    assert!(!paused.pause_requested);
    assert!(paused.error.is_none());

    // 旧构建留下的卡死形态：排队中的行带着暂停请求。
    index_jobs::insert(
        &conn,
        &IndexJob {
            id: "embedding:legacy-stuck".to_string(),
            book_id: "book-1".to_string(),
            source_id: Some("source-1".to_string()),
            kind: "embedding".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: true,
            cancel_requested: false,
            attempts: 1,
            cursor_json: "{}".to_string(),
            error: None,
            created_at: 1,
            updated_at: 1,
            started_at: None,
            finished_at: None,
        },
    )
    .unwrap();
    index_jobs::recover_interrupted(&conn, 13).unwrap();
    let repaired = index_jobs::get(&conn, "embedding:legacy-stuck")
        .unwrap()
        .unwrap();
    assert_eq!(repaired.status, IndexJobStatus::Paused);
    assert!(!repaired.pause_requested);

    // 没有暂停请求的运行中任务照旧回到持久队列。
    index_jobs::update_state(
        &conn,
        embedding_id,
        IndexJobStatus::Running,
        &paused.cursor_json,
        None,
        14,
        Some(14),
        None,
    )
    .unwrap();
    index_jobs::recover_interrupted(&conn, 15).unwrap();
    let queued = index_jobs::get(&conn, embedding_id).unwrap().unwrap();
    assert_eq!(queued.status, IndexJobStatus::Queued);
    assert!(!queued.pause_requested);
    assert!(queued.error.is_some());
}
