//! Tests for job filtering, actions and the detail panes.

use super::*;
use ngy_book_studio::services::BackgroundJobProgress;

fn job(status: BackgroundJobStatus) -> BackgroundJobSnapshot {
    BackgroundJobSnapshot {
        id: "job-1".to_string(),
        book_id: "book-1".to_string(),
        source_id: None,
        kind: "embedding".to_string(),
        status,
        pause_requested: false,
        cancel_requested: false,
        attempts: 2,
        progress: BackgroundJobProgress {
            completed: 3,
            total: Some(10),
        },
        error: None,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_500,
        started_at: Some(1_700_000_100),
        finished_at: None,
        cursor_json: None,
    }
}

#[test]
fn actions_follow_the_durable_job_state_machine() {
    assert_eq!(
        available_actions(&job(BackgroundJobStatus::Queued)),
        vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
    );
    assert_eq!(
        available_actions(&job(BackgroundJobStatus::Running)),
        vec![BackgroundJobAction::Pause, BackgroundJobAction::Cancel]
    );
    assert_eq!(
        available_actions(&job(BackgroundJobStatus::Paused)),
        vec![BackgroundJobAction::Resume, BackgroundJobAction::Cancel]
    );
    assert_eq!(
        available_actions(&job(BackgroundJobStatus::Failed)),
        vec![BackgroundJobAction::Retry]
    );
    assert_eq!(
        available_actions(&job(BackgroundJobStatus::Cancelled)),
        vec![BackgroundJobAction::Retry]
    );
    assert!(available_actions(&job(BackgroundJobStatus::Succeeded)).is_empty());
}

#[test]
fn task_filters_combine_kind_status_and_owning_book() {
    let snapshot = job(BackgroundJobStatus::Failed);
    let books = vec![
        BackgroundJobBook {
            id: "book-1".into(),
            title: "Rust 开发".into(),
        },
        BackgroundJobBook {
            id: "book-2".into(),
            title: "其他图书".into(),
        },
    ];
    assert!(job_matches(
        &snapshot,
        &books,
        "embedding",
        Some(BackgroundJobStatus::Failed),
        "rust"
    ));
    assert!(job_matches(&snapshot, &books, "", None, "job-1"));
    assert!(!job_matches(&snapshot, &books, "translation", None, ""));
    assert!(!job_matches(
        &snapshot,
        &books,
        "",
        Some(BackgroundJobStatus::Running),
        ""
    ));
    assert!(!job_matches(&snapshot, &books, "", None, "其他图书"));
    assert!(kind_matches("future-job", "other"));
    assert!(!kind_matches("translation", "other"));
}

#[test]
fn copied_details_preserve_full_identifiers_and_failure_context() {
    let mut snapshot = job(BackgroundJobStatus::Failed);
    snapshot.id = "translation:0123456789abcdef0123456789abcdef:zh-Hans".into();
    snapshot.error = Some("HTTP 请求失败".into());
    snapshot.cursor_json = Some(r#"{"next_ordinal":17}"#.into());
    let copied = diagnostic_summary(&snapshot);
    assert!(copied.contains(&snapshot.id));
    assert!(copied.contains("HTTP 请求失败"));
    assert!(copied.contains("\"next_ordinal\": 17"));
    assert!(copied.contains("UTC"));
    let logs = diagnostic_log_copy(&snapshot, &BackgroundJobLogSnapshot::default());
    assert!(!logs.contains("HTTP 请求失败"));
    assert!(!logs.contains("next_ordinal"));
    assert!(logs.contains(&snapshot.id));
}

#[test]
fn pending_control_hides_duplicate_actions_and_updates_status() {
    let mut pending = job(BackgroundJobStatus::Running);
    pending.pause_requested = true;
    assert!(available_actions(&pending).is_empty());
    assert_eq!(status_presentation(&pending, Palette::LIGHT).0, "正在暂停");

    pending.pause_requested = false;
    pending.cancel_requested = true;
    assert!(available_actions(&pending).is_empty());
    assert_eq!(status_presentation(&pending, Palette::LIGHT).0, "正在取消");
}

#[test]
fn progress_is_bounded_by_the_latest_total() {
    let mut snapshot = job(BackgroundJobStatus::Running);
    assert_eq!(progress_label(&snapshot), "进度 3 / 10 分块");
    snapshot.progress.completed = 12;
    assert_eq!(progress_label(&snapshot), "进度 10 / 10 分块");
    snapshot.progress.total = None;
    assert_eq!(progress_label(&snapshot), "已处理 12 分块");
}

#[test]
fn status_database_label_matches_durable_state() {
    assert_eq!(status_database_label(BackgroundJobStatus::Queued), "queued");
    assert_eq!(
        status_database_label(BackgroundJobStatus::Running),
        "running"
    );
    assert_eq!(status_database_label(BackgroundJobStatus::Paused), "paused");
    assert_eq!(
        status_database_label(BackgroundJobStatus::Succeeded),
        "succeeded"
    );
    assert_eq!(status_database_label(BackgroundJobStatus::Failed), "failed");
    assert_eq!(
        status_database_label(BackgroundJobStatus::Cancelled),
        "cancelled"
    );
}

#[test]
fn timestamp_round_trips_through_civil_from_days() {
    // 1700000000 == 2023-11-14 22:13:20 UTC; the local-zone display only
    // affects HH:MM:SS so the date must be stable across machines.
    let formatted = format_timestamp(1_700_000_000);
    assert!(formatted.starts_with("2023-11-14 "));
    assert_eq!(formatted.len(), "YYYY-MM-DD HH:MM:SS".len());
}

#[test]
fn prettify_cursor_falls_back_for_invalid_json() {
    assert_eq!(prettify_cursor("not json"), "not json");
    let pretty = prettify_cursor(r#"{"next_ordinal":42,"model":"qwen"}"#);
    assert!(pretty.contains("\"next_ordinal\""));
    assert!(pretty.contains("\"model\""));
}

fn detail_fixture(model: &str) -> TranslationBlockDetail {
    TranslationBlockDetail {
        ordinal: 7,
        unit_id: "unit-3".into(),
        unit_ordinal: 2,
        unit_title: "第三章".into(),
        unit_revision: 4,
        block_id: "unit-3::h5".into(),
        source_text: "Hello world".into(),
        segments: vec!["Hello".into(), "world".into()],
        request: TranslationBlockRequest {
            model: model.into(),
            target_language: "zh-Hans".into(),
            source_language: Some("en".into()),
            max_tokens: 4_096,
            temperature: 0.0,
            body: r#"{"model":"m"}"#.into(),
            system_prompt: "系统".into(),
            user_content: r#"{"source":"Hello world","segments":[]}"#.into(),
        },
    }
}

fn detail_state(tab: BlockDetailTab) -> BlockDetailState {
    BlockDetailState {
        job_id: "job".into(),
        ordinal: 7,
        generation: 0,
        tab,
        failed: false,
        loading: false,
        detail: None,
        error: None,
        probe: BlockProbeState::default(),
        scroll: ScrollHandle::default(),
    }
}

fn probe_fixture(response_text: &str) -> TranslationBlockProbe {
    TranslationBlockProbe {
        ordinal: 7,
        model: "chat-test".into(),
        response_text: response_text.into(),
        response_bytes: response_text.len(),
        end: "done",
        finish_reason: Some("stop"),
        decoded_segments: None,
        decode_error_kind: Some(JobLogErrorKind::SegmentCountMismatch),
        decode_error_line: None,
        decode_error_column: None,
        elapsed_ms: 1500,
    }
}

/// The response tab is the only one that is not part of the pinned payload,
/// and the only one that must stay hidden for a block that translated fine.
#[test]
fn only_a_failed_block_offers_the_model_response_tab() {
    let healthy = block_detail_tabs(false);
    assert!(!healthy.contains(&BlockDetailTab::Response));
    assert_eq!(
        healthy,
        vec![
            BlockDetailTab::Overview,
            BlockDetailTab::Body,
            BlockDetailTab::Source,
            BlockDetailTab::Prompt,
            BlockDetailTab::Message,
        ]
    );

    let failed = block_detail_tabs(true);
    assert_eq!(failed.len(), healthy.len() + 1);
    assert_eq!(failed.last(), Some(&BlockDetailTab::Response));
    // Every tab keeps a distinct id and label, so a click can never land on
    // the wrong page.
    let ids: std::collections::BTreeSet<_> = failed.iter().map(|tab| tab.id()).collect();
    let labels: std::collections::BTreeSet<_> = failed.iter().map(|tab| tab.label()).collect();
    assert_eq!(ids.len(), failed.len());
    assert_eq!(labels.len(), failed.len());
}

/// A replayed answer is shown the way it arrived: a valid JSON object is
/// indented, and anything the decoder rejects is shown byte for byte, because
/// that broken shape is exactly what the user opened the tab to see.
#[test]
fn the_response_tab_shows_the_replayed_answer_without_cleaning_it() {
    // Nothing is shown before the user asks for a replay.
    assert!(block_detail_text(&detail_state(BlockDetailTab::Response), None).is_none());

    let mut state = detail_state(BlockDetailTab::Response);
    state.probe.result = Some(probe_fixture(r#"{"translations":[{"id":0,"text":"甲"}]}"#));
    let pretty = block_detail_text(&state, None).unwrap();
    assert!(
        pretty.contains('\n'),
        "a valid object is indented: {pretty}"
    );
    assert!(serde_json::from_str::<serde_json::Value>(&pretty).is_ok());

    // A fenced answer is the classic weak-model failure; it must be visible
    // as it was written, fence and all.
    let fenced = "```json\n{\"translations\":[{\"id\":0,\"text\":\"甲\"}]}\n```";
    state.probe.result = Some(probe_fixture(fenced));
    assert_eq!(block_detail_text(&state, None).unwrap(), fenced);

    // So is a truncated object with no closing braces.
    let truncated = r#"{"translations":[{"id":0,"text":"甲""#;
    state.probe.result = Some(probe_fixture(truncated));
    assert_eq!(block_detail_text(&state, None).unwrap(), truncated);

    // The replayed text is copied verbatim, so the clipboard never carries a
    // request payload from another tab.
    assert_eq!(
        block_detail_text(&detail_state(BlockDetailTab::Source), None),
        None
    );
}

#[test]
fn the_replay_panel_names_how_the_stream_ended_and_what_the_decoder_said() {
    let mut probe = probe_fixture("{}");
    assert_eq!(probe_end_label(&probe), "模型正常收尾");

    probe.end = "unexpected_finish_reason";
    probe.finish_reason = Some("length");
    assert_eq!(
        probe_end_label(&probe),
        "模型提前结束响应（finish_reason=length）"
    );

    probe.end = "stream_eof";
    assert_eq!(probe_end_label(&probe), "响应流在没有收尾标记的情况下关闭");

    probe.end = "size_limit";
    assert_eq!(
        probe_end_label(&probe),
        "响应超过大小上限，只显示已收到的部分"
    );

    probe.end = "tool_call";
    assert_eq!(probe_end_label(&probe), "模型意外请求了工具");

    // The verdict comes from the worker's own decoder, labelled from the
    // closed category table rather than re-derived in the window.
    probe.decode_error_kind = Some(JobLogErrorKind::SegmentCountMismatch);
    probe.decode_error_line = Some(3);
    probe.decode_error_column = Some(12);
    assert_eq!(
        probe_decode_label(&probe),
        "严格解码失败：片段数量与原文不一致（第 3 行第 12 列）"
    );

    probe.decode_error_kind = Some(JobLogErrorKind::IncompleteJson);
    probe.decode_error_line = None;
    probe.decode_error_column = None;
    assert_eq!(probe_decode_label(&probe), "严格解码失败：响应 JSON 不完整");

    probe.decode_error_kind = None;
    probe.decoded_segments = Some(2);
    assert_eq!(probe_decode_label(&probe), "严格解码通过：2 个片段");

    // A replay that produced no answer at all still says what the model did.
    let mut empty = probe_fixture("");
    empty.end = "unexpected_finish_reason";
    empty.finish_reason = None;
    assert_eq!(
        probe_end_label(&empty),
        "模型提前结束响应（finish_reason=other）"
    );
}

#[test]
fn block_detail_tabs_show_the_payload_and_hide_nothing_behind_the_preview() {
    let detail = detail_fixture("chat-test");
    let source = block_detail_text(&detail_state(BlockDetailTab::Source), Some(&detail)).unwrap();
    // The whole block, not the 96-character preview the list holds.
    assert_eq!(source, "Hello world");
    assert_eq!(
        block_detail_text(&detail_state(BlockDetailTab::Prompt), Some(&detail)).unwrap(),
        "系统"
    );

    let body = block_detail_text(&detail_state(BlockDetailTab::Body), Some(&detail)).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());

    // A body that is not JSON must still be shown verbatim rather than
    // rendering an empty panel.
    let mut broken = detail.clone();
    broken.request.body = "<html>error page</html>".into();
    let body = block_detail_text(&detail_state(BlockDetailTab::Body), Some(&broken)).unwrap();
    assert_eq!(body, "<html>error page</html>");

    let overview =
        block_detail_text(&detail_state(BlockDetailTab::Overview), Some(&detail)).unwrap();
    assert!(overview.contains("#7"));
    assert!(overview.contains("unit-3::h5"));
    assert!(overview.contains("chat-test"));
    assert!(overview.contains("中文（简体）"));
    assert!(overview.contains("5, 5"), "per-segment character counts");
    assert!(overview.contains("原文语言标记: en"));

    // A task that never ran records no model; the overview must say so
    // instead of printing an empty field.
    let empty_model = block_detail_text(
        &detail_state(BlockDetailTab::Overview),
        Some(&detail_fixture("")),
    )
    .unwrap();
    assert!(empty_model.contains("调用模型: \n"));

    // Nothing is rendered while the payload is missing.
    for tab in block_detail_tabs(true) {
        assert!(block_detail_text(&detail_state(tab), None).is_none());
    }
}
