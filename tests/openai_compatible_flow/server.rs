//! Mock and scripted HTTP servers plus the request helpers.

use super::*;

pub(crate) fn search_batch_sse(calls: &[(&str, &str)]) -> String {
    let tool_calls = calls
        .iter()
        .enumerate()
        .map(|(index, (id, query))| {
            serde_json::json!({
                "index": index,
                "id": id,
                "type": "function",
                "function": {
                    "name": "search_books",
                    "arguments": serde_json::json!({"query": query, "mode": "keyword"}).to_string()
                }
            })
        })
        .collect::<Vec<_>>();
    let event = serde_json::json!({
        "choices": [{"delta": {"tool_calls": tool_calls}, "finish_reason": "tool_calls"}]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

pub(crate) fn context_test_runtime(
    base_url: String,
    search: &(impl SearchBackend + Clone + 'static),
) -> AgentRuntime {
    AgentRuntime::new(
        Arc::new(
            OpenAiHttpProvider::new(ProviderConfig {
                base_url,
                api_key: None,
                remote_content_confirmed: false,
                allow_insecure_remote_http: false,
                request_timeout_secs: 10,
            })
            .unwrap(),
        ),
        Arc::new(search.clone()),
        Arc::new(EmptyBooks),
        "chat-model",
        AgentLimits::default(),
    )
    .unwrap()
}

pub(crate) fn context_test_question() -> AgentQuestion {
    AgentQuestion {
        question: "请回答现在的问题，并保留本轮来源。".to_string(),
        allowed_book_ids: vec!["book-1".to_string()],
        book_titles: vec![("book-1".to_string(), "允许的图书".to_string())],
        history: vec![
            ChatMessage::text(ChatRole::User, "最早的问题"),
            ChatMessage::text(ChatRole::Assistant, "最早的回答"),
            ChatMessage::text(ChatRole::User, "最近的问题"),
            ChatMessage::text(ChatRole::Assistant, "最近的回答"),
        ],
        snapshots: Vec::new(),
    }
}

pub(crate) fn nested_context_error() -> String {
    let inner = serde_json::json!({
        "error": {
            "code": 400,
            "message": "request (4155 tokens) exceeds the available context size (4096 tokens), try increasing it",
            "type": "exceed_context_size_error",
            "n_prompt_tokens": 4155,
            "n_ctx": 4096
        }
    });
    serde_json::json!({
        "error": {
            "message": inner.to_string(),
            "type": "invalid_request_error",
            "param": null,
            "code": null
        }
    })
    .to_string()
}

pub(crate) fn answer_sse(answer: &str) -> String {
    let event = serde_json::json!({
        "choices": [{"delta": {"content": answer}, "finish_reason": "stop"}]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

pub(crate) fn chat_request_body(request: &str) -> serde_json::Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

pub(crate) fn custom_chat_generation() -> ChatGenerationSettings {
    ChatGenerationSettings {
        temperature: Some(0.7),
        top_p: Some(0.8),
        max_output_tokens: 131_072,
        presence_penalty: Some(0.3),
        frequency_penalty: Some(-0.4),
    }
}

pub(crate) fn assert_chat_sampling_parameters(
    request: &serde_json::Value,
    expected: &ChatGenerationSettings,
) {
    for (field, value) in [
        ("temperature", expected.temperature),
        ("top_p", expected.top_p),
        ("presence_penalty", expected.presence_penalty),
        ("frequency_penalty", expected.frequency_penalty),
    ] {
        match value {
            Some(value) => {
                let actual = request[field].as_f64().unwrap();
                assert!(
                    (actual - f64::from(value)).abs() < 1e-6,
                    "{field}: expected {value}, received {actual}"
                );
            }
            None => assert!(
                request.get(field).is_none(),
                "unset {field} must be omitted"
            ),
        }
    }
}

pub(crate) fn assert_history_trim_preserves_current_turn(
    original: &serde_json::Value,
    retried: &serde_json::Value,
    current_turn_messages: usize,
) {
    let original = original["messages"].as_array().unwrap();
    let retried = retried["messages"].as_array().unwrap();
    assert!(retried.len() < original.len());
    assert!(retried.len() > current_turn_messages);
    let removed = original.len() - retried.len();
    assert_eq!(
        removed % 2,
        0,
        "history trimming must remove complete turns"
    );
    assert_eq!(original[0]["role"], "system");
    assert_eq!(retried[0], original[0]);
    assert_eq!(&retried[1..], &original[1 + removed..]);
    assert_eq!(
        &retried[retried.len() - current_turn_messages..],
        &original[original.len() - current_turn_messages..]
    );
}

pub(crate) fn mock_server(
    requests_to_serve: usize,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = thread::spawn(move || {
        for _ in 0..requests_to_serve {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut stream);
            let first_line = request.lines().next().unwrap_or_default().to_string();
            captured.lock().unwrap().push(request);
            let (content_type, body) = if first_line.starts_with("GET /v1/models ") {
                (
                    "application/json",
                    r#"{"data":[{"id":"embed-model"},{"id":"chat-model"},{"id":"chat-model"}]}"#
                        .to_string(),
                )
            } else if first_line.starts_with("POST /v1/embeddings ") {
                (
                    "application/json",
                    r#"{"model":"embed-model","data":[{"index":1,"embedding":[0.0,1.0]},{"index":0,"embedding":[1.0,0.0]}],"usage":{"prompt_tokens":2,"completion_tokens":0,"total_tokens":2}}"#.to_string(),
                )
            } else if first_line.starts_with("POST /v1/chat/completions ") {
                (
                    "text/event-stream",
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"mock \"},\"finish_reason\":null}]}\n\n",
                        "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                    .to_string(),
                )
            } else {
                ("text/plain", "unexpected route".to_string())
            };
            let status = if body == "unexpected route" {
                "404 Not Found"
            } else {
                "200 OK"
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("http://{address}/v1/"), requests, server)
}

pub(crate) fn scripted_chat_server(
    responses: Vec<String>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    scripted_chat_http_server(
        responses
            .into_iter()
            .map(|body| ("200 OK", "text/event-stream", body))
            .collect(),
    )
}

pub(crate) fn scripted_chat_http_server(
    responses: Vec<(&'static str, &'static str, String)>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    scripted_chat_http_server_at_path(responses, "/v1/")
}

pub(crate) fn scripted_chat_http_server_at_path(
    responses: Vec<(&'static str, &'static str, String)>,
    base_path: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = thread::spawn(move || {
        for (status, content_type, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut stream);
            assert!(
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .starts_with(&format!("POST {base_path}chat/completions "))
            );
            captured.lock().unwrap().push(request);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("http://{address}{base_path}"), requests, server)
}

/// 先回一发 429（带 `Retry-After`），再回一发正常的 chat SSE；记录每一发到达服务器的时刻。
///
/// 到达时刻是「这一发真的发出去了」的证据：本地限速只能把请求推迟，不能让它凭空消失。
pub(crate) fn throttled_chat_server(
    retry_after_secs: u64,
) -> (
    String,
    Arc<Mutex<Vec<std::time::Instant>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&arrivals);
    let server = thread::spawn(move || {
        for round in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let request = read_request(&mut stream);
            assert!(
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .starts_with("POST /v1/chat/completions ")
            );
            captured.lock().unwrap().push(std::time::Instant::now());
            let (status, content_type, body) = if round == 0 {
                (
                    "429 Too Many Requests",
                    "application/json",
                    r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#.to_string(),
                )
            } else {
                (
                    "200 OK",
                    "text/event-stream",
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                    .to_string(),
                )
            };
            let retry_after = if round == 0 {
                format!("Retry-After: {retry_after_secs}\r\n")
            } else {
                String::new()
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{retry_after}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("http://{address}/v1/"), arrivals, server)
}

pub(crate) fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut expected = None;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if expected.is_none()
            && let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                })
                .unwrap_or(0);
            expected = Some(header_end + 4 + content_length);
        }
        if expected.is_some_and(|expected| bytes.len() >= expected) {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}

pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
