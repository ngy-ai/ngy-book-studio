//! Diagnostic correlation and streamed error flows.

use super::*;

#[tokio::test]
pub(crate) async fn ai_diagnostics_correlate_http_retry_without_logging_private_payloads() {
    let mut rejected: serde_json::Value =
        serde_json::from_str(&incomplete_tool_arguments_error("read_passages")).unwrap();
    rejected["error"]["code"] = "PRIVATE_PROVIDER_CODE".into();
    rejected["error"]["type"] = "PRIVATE_PROVIDER_TYPE".into();
    rejected["error"]["param"] = "PRIVATE_ERROR_PARAM".into();
    rejected["response_extension"] =
        "PRIVATE_ERROR_BODY http://localhost/PRIVATE_RESPONSE_PATH?key=PRIVATE_RESPONSE_QUERY"
            .into();
    let success = serde_json::json!({
        "choices": [{
            "delta": {"content": "PRIVATE_RESPONSE_TEXT [[ngy-no-source]]"},
            "finish_reason": "PRIVATE_FINISH_REASON"
        }],
        "response_extension": "PRIVATE_RESPONSE_EXTENSION"
    });
    let (base_url, requests, server) = scripted_chat_http_server_at_path(
        vec![
            (
                "500 Internal Server Error",
                "application/json",
                rejected.to_string(),
            ),
            (
                "200 OK",
                "text/event-stream",
                format!("data: {success}\n\ndata: [DONE]\n\n"),
            ),
        ],
        "/PRIVATE_ENDPOINT_PATH/v1/",
    );
    let search = RecordingSearch::default();
    let cancellation = AgentCancellation::default();
    let trace_id = cancellation.trace_id();
    let logs = AiLogCapture::default();
    let answer = async {
        // Query strings are rejected before transport. Cover that diagnostic
        // boundary too; the actual valid endpoint exercises path redaction.
        let invalid = OpenAiHttpProvider::new(ProviderConfig {
            base_url: format!("{base_url}?key=PRIVATE_ENDPOINT_QUERY"),
            ..ProviderConfig::default()
        });
        assert!(invalid.is_err());
        diagnostic_test_runtime(base_url, &search)
            .answer(diagnostic_test_question(), None, cancellation)
            .await
    }
    .with_subscriber(logs.subscriber())
    .await
    .unwrap();
    server.join().unwrap();
    assert_eq!(answer.markdown, "PRIVATE_RESPONSE_TEXT ");
    assert!(search.requests.lock().unwrap().is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        for private in [
            "PRIVATE_ENDPOINT_PATH",
            "PRIVATE_API_KEY",
            "PRIVATE_QUESTION",
            "PRIVATE_BOOK_TEXT",
            "PRIVATE_BOOK_TITLE",
        ] {
            assert!(request.contains(private), "fixture did not send {private}");
        }
    }
    let rejected_request = chat_request_body(&requests[0]);
    let retried_request = chat_request_body(&requests[1]);
    assert_eq!(rejected_request["tools"], retried_request["tools"]);
    assert_eq!(retried_request["temperature"], 0.0);

    let logs = logs.text();
    assert!(
        !logs.contains("PRIVATE_"),
        "private data appeared in logs: {logs}"
    );
    let sent = logs
        .lines()
        .filter(|line| line.contains("stage=\"http_send\""))
        .collect::<Vec<_>>();
    assert_eq!(sent.len(), 2, "{logs}");
    let mut http_ids = Vec::new();
    for line in sent {
        assert_eq!(diagnostic_u64_field(line, "trace_id"), Some(trace_id));
        http_ids.push(diagnostic_u64_field(line, "http_id").unwrap());
        assert!(diagnostic_u64_field(line, "request_body_bytes").unwrap() > 0);
    }
    assert_ne!(http_ids[0], http_ids[1]);
    for expected in [
        "http_status=500",
        "http_status=200",
        "error_kind=\"incomplete_tool_arguments\"",
        "provider_code=Some(\"other\")",
        "provider_type=Some(\"other\")",
        // 信封里的 `param` 是服务商自己的字符串（`PRIVATE_ERROR_PARAM`），不进日志。
        "provider_param=None",
        "error_json_valid=true",
        "attempt=1",
        "attempt=2",
        "stage=\"sse_finished\"",
        "finish_reason=Some(\"other\")",
        "AI run completed",
    ] {
        assert!(logs.contains(expected), "missing {expected}: {logs}");
    }
    assert_eq!(
        logs.matches("retrying AI stream start once after incomplete tool arguments")
            .count(),
        1
    );
}

/// 422 这类「参数值不对」的拒绝要在诊断里点名**我们自己的**哪个字段被拒：服务商只认
/// 顶层 `reasoning_effort` 的 `low`/`medium`、而我们发 `none` 时，不点名就只能靠猜。
/// 名字只从固定列表里取，服务端自己的文本（这里故意塞了 `PRIVATE_`）一个字节都不出来。
#[tokio::test]
pub(crate) async fn http_rejection_names_our_own_rejected_parameter_only() {
    let rejected = serde_json::json!({
        "detail": [{
            "type": "literal_error",
            "loc": ["body", "reasoning_effort"],
            "msg": "Input should be 'low', 'medium' or 'xhigh'",
            "input": "PRIVATE_REJECTED_VALUE"
        }]
    });
    let (base_url, requests, server) = scripted_chat_http_server(vec![(
        "422 Unprocessable Entity",
        "application/json",
        rejected.to_string(),
    )]);
    let search = RecordingSearch::default();
    let logs = AiLogCapture::default();
    let error = async {
        diagnostic_test_runtime(base_url, &search)
            .answer(
                diagnostic_test_question(),
                None,
                AgentCancellation::default(),
            )
            .await
    }
    .with_subscriber(logs.subscriber())
    .await
    .unwrap_err();
    server.join().unwrap();
    assert!(format!("{error:#}").contains("422"), "{error:#}");
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(search.requests.lock().unwrap().is_empty());

    let logs = logs.text();
    assert!(
        !logs.contains("PRIVATE_"),
        "private data appeared in logs: {logs}"
    );
    assert!(logs.contains("http_status=422"), "{logs}");
    assert!(
        logs.contains("provider_param=Some(\"reasoning_effort\")"),
        "missing provider_param: {logs}"
    );
}

#[tokio::test]
pub(crate) async fn ai_diagnostics_classify_streamed_invalid_arguments_without_retry_or_payloads() {
    let provisional = serde_json::json!({
        "choices": [{"delta": {"content": "PRIVATE_PROVISIONAL_TEXT"}}],
        "response_extension": "PRIVATE_SSE_EXTENSION"
    });
    let stream = format!(
        "data: {provisional}\n\n{}",
        tool_sse(
            "PRIVATE_TOOL_CALL_ID",
            "read_passages",
            r#"{"passage_ids":["PRIVATE_TOOL_ARGUMENT""#,
        )
    );
    let (base_url, requests, server) = scripted_chat_http_server_at_path(
        vec![("200 OK", "text/event-stream", stream)],
        "/PRIVATE_ENDPOINT_PATH/v1/",
    );
    let search = RecordingSearch::default();
    let cancellation = AgentCancellation::default();
    let trace_id = cancellation.trace_id();
    let logs = AiLogCapture::default();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let error = async {
        diagnostic_test_runtime(base_url, &search)
            .answer(diagnostic_test_question(), Some(events_tx), cancellation)
            .await
    }
    .with_subscriber(logs.subscriber())
    .await
    .unwrap_err();
    server.join().unwrap();
    assert!(format!("{error:#}").contains("tool arguments are not valid JSON"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(search.requests.lock().unwrap().is_empty());
    let mut events = Vec::new();
    while let Some(event) = events_rx.recv().await {
        events.push(event);
    }
    assert!(events.contains(&AgentRunEvent::AnswerReset));
    assert!(!events.contains(&AgentRunEvent::AnswerCommitted));

    let logs = logs.text();
    assert!(
        !logs.contains("PRIVATE_"),
        "private data appeared in logs: {logs}"
    );
    assert_eq!(logs.matches("stage=\"http_send\"").count(), 1, "{logs}");
    assert!(!logs.contains("retrying"), "{logs}");
    let parse_failure = logs
        .lines()
        .find(|line| line.contains("stage=\"tool_arguments_json\""))
        .unwrap_or_else(|| panic!("missing tool JSON diagnostic: {logs}"));
    assert_eq!(
        diagnostic_u64_field(parse_failure, "trace_id"),
        Some(trace_id)
    );
    assert!(parse_failure.contains("tool=\"read_passages\""));
    assert!(parse_failure.contains("json_category=Eof"));
    assert_eq!(diagnostic_u64_field(parse_failure, "json_line"), Some(1));
    assert!(diagnostic_u64_field(parse_failure, "json_column").unwrap() > 0);
    assert!(logs.contains("error_kind=\"stream_protocol\""), "{logs}");
    let stream_summary = logs
        .lines()
        .find(|line| line.contains("stage=\"sse_finished\""))
        .unwrap_or_else(|| panic!("missing SSE summary: {logs}"));
    assert_eq!(
        diagnostic_u64_field(stream_summary, "trace_id"),
        Some(trace_id)
    );
    assert!(diagnostic_u64_field(stream_summary, "http_id").is_some());
    assert!(diagnostic_u64_field(stream_summary, "wire_bytes").unwrap() > 0);
    assert!(diagnostic_u64_field(stream_summary, "event_count").unwrap() > 0);
}

#[tokio::test]
pub(crate) async fn ai_diagnostics_classify_invalid_sse_json_without_retry_or_payloads() {
    let provisional = serde_json::json!({
        "choices": [{"delta": {"content": "PRIVATE_PROVISIONAL_TEXT"}}]
    });
    let stream = format!(
        "data: {provisional}\n\ndata: {{\"choices\":[{{\"delta\":{{\"content\":\"PRIVATE_BROKEN_BODY\"\n\n"
    );
    let (base_url, requests, server) = scripted_chat_http_server_at_path(
        vec![("200 OK", "text/event-stream", stream)],
        "/PRIVATE_ENDPOINT_PATH/v1/",
    );
    let search = RecordingSearch::default();
    let cancellation = AgentCancellation::default();
    let trace_id = cancellation.trace_id();
    let logs = AiLogCapture::default();
    let error = async {
        diagnostic_test_runtime(base_url, &search)
            .answer(diagnostic_test_question(), None, cancellation)
            .await
    }
    .with_subscriber(logs.subscriber())
    .await
    .unwrap_err();
    server.join().unwrap();
    assert!(format!("{error:#}").contains("invalid chat completion event"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(search.requests.lock().unwrap().is_empty());

    let logs = logs.text();
    assert!(
        !logs.contains("PRIVATE_"),
        "private data appeared in logs: {logs}"
    );
    assert_eq!(logs.matches("stage=\"http_send\"").count(), 1, "{logs}");
    assert!(!logs.contains("retrying"), "{logs}");
    let failure = logs
        .lines()
        .find(|line| line.contains("stage=\"sse_failed\""))
        .unwrap_or_else(|| panic!("missing SSE JSON diagnostic: {logs}"));
    assert_eq!(diagnostic_u64_field(failure, "trace_id"), Some(trace_id));
    let http_id = diagnostic_u64_field(failure, "http_id").unwrap();
    let sent = logs
        .lines()
        .find(|line| line.contains("stage=\"http_send\""))
        .unwrap();
    assert_eq!(diagnostic_u64_field(sent, "http_id"), Some(http_id));
    assert!(failure.contains("json_error_category=Some(Eof)"), "{logs}");
    assert_eq!(diagnostic_u64_field(failure, "json_error_line"), Some(1));
    assert!(diagnostic_u64_field(failure, "json_error_column").unwrap() > 0);
    assert!(failure.contains("error_kind=\"json_eof\""), "{logs}");
    assert!(logs.contains("outcome=\"failed\""), "{logs}");
}
