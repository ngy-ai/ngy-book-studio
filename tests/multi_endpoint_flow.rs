use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ngy_book_studio::{
    agent::{AgentAnswerSourceStatus, SearchMode, SearchRequest},
    agent_chat::{AgentConversation, ConversationQuestion},
    chat::ChatWindowKind,
    credentials::MemoryCredentialStore,
    services::{
        ApiKeyUpdate, AppServices, BackgroundJobStatus, EndpointSettings, ModelRole,
        ProviderSettings,
    },
};
use serde_json::{Value, json};

struct MockEndpoint {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockEndpoint {
    fn start(label: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock accept: {error}"),
                };
                // Windows accepted sockets inherit the listener's nonblocking
                // mode; the HTTP reader requires blocking reads with a timeout.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream);
                let path = request.lines().next().unwrap().to_string();
                let body = request.split_once("\r\n\r\n").unwrap().1;
                let input: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                captured.lock().unwrap().push(request.clone());
                let (content_type, output) = if path.starts_with("GET /v1/models ") {
                    (
                        "application/json",
                        json!({"data":[{"id":format!("{label}-model")}]}).to_string(),
                    )
                } else if path.starts_with("POST /v1/embeddings ") {
                    let vectors: Vec<_> = input["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                        .map(|(index, _)| json!({"index":index,"embedding":[1.0,0.0,0.0]}))
                        .collect();
                    (
                        "application/json",
                        json!({"model":input["model"],"data":vectors}).to_string(),
                    )
                } else {
                    assert!(path.starts_with("POST /v1/chat/completions "));
                    let content = if body.contains("data:image/png;base64,") {
                        json!({"regions":[{"ocr":"fixture page", "description":"endpoint routing fixture", "region":null}]}).to_string()
                    } else {
                        format!("来自 {label} 的回答。")
                    };
                    let delta =
                        json!({"choices":[{"delta":{"content":content},"finish_reason":"stop"}]})
                            .to_string();
                    (
                        "text/event-stream",
                        format!("data: {delta}\n\ndata: [DONE]\n\n"),
                    )
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{output}", output.len()).unwrap();
                stream.flush().unwrap();
            }
        });
        Self {
            url,
            requests,
            stopped,
            worker: Some(worker),
        }
    }

    fn endpoint(&self, id: &str) -> EndpointSettings {
        EndpointSettings {
            id: id.into(),
            name: id.into(),
            base_url: self.url.clone(),
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: String::new(),
            request_timeout_secs: 10,
        }
    }
}

impl Drop for MockEndpoint {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Preserve a worker failure without aborting on a second panic while
        // the test is already unwinding from its original failure.
        if let Err(payload) = self.worker.take().unwrap().join()
            && !thread::panicking()
        {
            std::panic::resume_unwind(payload);
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "mock request ended early");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}

#[test]
fn independent_endpoints_route_chat_search_and_background_jobs_after_restart() {
    let chat = MockEndpoint::start("chat");
    let embedding = MockEndpoint::start("embedding");
    let vision = MockEndpoint::start("vision");
    let next_chat = MockEndpoint::start("next-chat");
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let mut settings = ProviderSettings {
        base_url: chat.url.clone(),
        chat_model: "chat-model".into(),
        embedding_model: "embedding-model".into(),
        vision_model: "vision-model".into(),
        auto_run_background_jobs: true,
        // The mock endpoints only answer the indexing pipeline; whole-book
        // translation is off so no extra job enters the completion wait below.
        default_language: None,
        ..ProviderSettings::default()
    };
    settings.endpoint_routing.additional_endpoints =
        vec![embedding.endpoint("embedding"), vision.endpoint("vision")];
    settings.endpoint_routing.embedding_endpoint_id = "embedding".into();
    settings.endpoint_routing.vision_endpoint_id = "vision".into();
    {
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        services
            .runtime()
            .block_on(services.configure_providers(
                settings.clone(),
                BTreeMap::from([
                    (
                        "default".into(),
                        ApiKeyUpdate::Set("fixture-chat-key".into()),
                    ),
                    (
                        "embedding".into(),
                        ApiKeyUpdate::Set("fixture-embedding-key".into()),
                    ),
                    (
                        "vision".into(),
                        ApiKeyUpdate::Set("fixture-vision-key".into()),
                    ),
                ]),
            ))
            .unwrap();
    }
    let services = Arc::new(AppServices::open_with_credentials(temp.path(), credentials).unwrap());
    assert_eq!(services.provider_settings().unwrap(), settings);
    services.runtime().block_on(async {
        let mut invalid_other_drafts = settings.clone();
        invalid_other_drafts.chat_generation.max_output_tokens = 0;
        invalid_other_drafts.web_search_enabled = true;
        invalid_other_drafts.web_search_url_template = "invalid web draft".into();
        assert_eq!(
            services
                .probe_provider_models(invalid_other_drafts, ApiKeyUpdate::Keep)
                .await
                .unwrap()[0]
                .id,
            "chat-model"
        );
        assert_eq!(
            services
                .probe_endpoint_models(
                    next_chat.endpoint("unsaved"),
                    ApiKeyUpdate::Set("fixture-next-key".into())
                )
                .await
                .unwrap()[0]
                .id,
            "next-chat-model"
        );
        assert_eq!(services.provider_settings().unwrap(), settings);
        for role in [ModelRole::Chat, ModelRole::Embedding, ModelRole::Vision] {
            let endpoint = settings.endpoint_for(role).unwrap();
            let models = services
                .probe_endpoint_models(endpoint, ApiKeyUpdate::Keep)
                .await
                .unwrap();
            assert_eq!(models.len(), 1);
        }
        let book = services
            .spawn_library(|library| library.create_book("多端点测试", "fixture"))
            .await
            .unwrap()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let jobs = services
                .background_jobs_for_books(vec![book.id.clone()])
                .await
                .unwrap();
            // Vision may run before rasterization, then automatically retry
            // when pages are published. Require the complete pipeline to finish.
            if jobs.len() >= 3
                && jobs
                    .iter()
                    .all(|job| job.status == BackgroundJobStatus::Succeeded)
            {
                break;
            }
            assert!(Instant::now() < deadline, "jobs did not finish: {jobs:?}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let before = embedding.requests.lock().unwrap().len();
        let results = services
            .search()
            .unwrap()
            .search(SearchRequest {
                query: "fixture page".into(),
                book_ids: vec![book.id],
                limit: 5,
                mode: SearchMode::Semantic,
            })
            .await
            .unwrap();
        assert!(!results.is_empty());
        assert_eq!(embedding.requests.lock().unwrap().len(), before + 1);

        let conversation =
            AgentConversation::new(services.clone(), ChatWindowKind::Library, None).unwrap();
        let prepared = conversation.prepare_request(1).unwrap();
        let mut next = settings.clone();
        next.base_url = next_chat.url.clone();
        next.chat_model = "next-chat-model".into();
        services
            .configure_providers(
                next,
                BTreeMap::from([(
                    "default".into(),
                    ApiKeyUpdate::Set("fixture-next-key".into()),
                )]),
            )
            .await
            .unwrap();
        let question = |request_id| ConversationQuestion {
            request_id,
            question: "端点测试".into(),
            allowed_book_ids: vec![],
            book_titles: vec![],
            snapshots: vec![],
        };
        let old_answer = conversation
            .ask_prepared(question(1), None, prepared)
            .await
            .unwrap();
        assert!(old_answer.answer.markdown.contains("来自 chat 的回答"));
        assert_eq!(
            old_answer.answer.source_status,
            AgentAnswerSourceStatus::NoVerifiedSources
        );
        let new_answer = conversation.ask(question(2), None).await.unwrap();
        assert!(new_answer.answer.markdown.contains("来自 next-chat 的回答"));
    });
    for (server, key, model, allowed_path) in [
        (
            &chat,
            "fixture-chat-key",
            "chat-model",
            "POST /v1/chat/completions ",
        ),
        (
            &embedding,
            "fixture-embedding-key",
            "embedding-model",
            "POST /v1/embeddings ",
        ),
        (
            &vision,
            "fixture-vision-key",
            "vision-model",
            "POST /v1/chat/completions ",
        ),
        (
            &next_chat,
            "fixture-next-key",
            "next-chat-model",
            "POST /v1/chat/completions ",
        ),
    ] {
        let requests = server.requests.lock().unwrap();
        assert!(!requests.is_empty());
        for request in requests.iter() {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains(&format!("authorization: bearer {key}"))
            );
            if request.starts_with("GET /v1/models ") {
                continue;
            }
            assert!(request.starts_with(allowed_path), "wrong endpoint route");
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["model"], model);
            if model == "vision-model" {
                assert!(request.contains("data:image/png;base64,"));
            }
        }
    }
}
