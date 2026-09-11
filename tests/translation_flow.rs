use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use moye_epub_editor::{
    credentials::MemoryCredentialStore,
    library::ImportOutcome,
    services::{
        AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
        ProviderSettings, TranslatedBlock,
    },
};
use serde_json::{Value, json};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const FORMATTED_BODY: &str = r#"<h1>Formatting <em>matters</em></h1>
<p>Start <strong>bold</strong> and <em>italic</em>.<br/>Next <span style="color: #b24a00">colored</span> and <code>do_not_translate()</code> end.</p>
<ul><li>First <strong>item</strong></li><li>Second item</li></ul>
<table><thead><tr><th>Key</th><th>Value</th></tr></thead><tbody><tr><td>Alpha</td><td><em>Beta</em></td></tr></tbody></table>
<p>Read <a href="https://example.invalid">this</a> now</p>
<p><span>adjacent</span><span>parts</span></p>
<pre><code>print("leave this code unchanged")</code></pre>"#;

const INVALID_RESPONSE_SENTINEL: &str = "PRIVATE_PROVIDER_RESPONSE_MUST_NOT_BECOME_JOB_ERROR";

/// A source block carrying this marker always receives an invalid response, so
/// the per-block skip path can be exercised without a real model.
const POISON_SENTINEL: &str = "POISON_BLOCK";

#[derive(Clone, Copy, Debug)]
enum ReplyMode {
    ReverseIds,
    MissingId,
    DuplicateId,
    Fenced,
    Prefaced,
    MalformedThenValid,
    PlainThenValid,
    AlwaysInvalid,
    GatedCorrectionValid,
    GatedCorrectionInvalid,
    /// A JSON answer whose structural punctuation is full-width, as a
    /// CJK-oriented model writes it (`"text"："..."，"`).
    FullWidthStructure,
    /// Valid answers everywhere except blocks carrying [`POISON_SENTINEL`].
    PoisonedBlock,
}

/// An actual loopback HTTP/SSE provider. Every fixture opts out of automatic
/// jobs, then resumes only translation so no PDF rendering or other AI role is
/// involved in this contract test.
struct TranslationEndpoint {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    full_requests: Arc<Mutex<Vec<Value>>>,
    correction_released: Arc<AtomicBool>,
    responses_finished: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TranslationEndpoint {
    fn start(label: &'static str, mode: ReplyMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let full_requests = Arc::new(Mutex::new(Vec::new()));
        let captured_full = Arc::clone(&full_requests);
        let correction_released = Arc::new(AtomicBool::new(false));
        let released = Arc::clone(&correction_released);
        let responses_finished = Arc::new(AtomicUsize::new(0));
        let finished = Arc::clone(&responses_finished);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("translation mock accept: {error}"),
                };
                // Windows accepted sockets inherit the listener's nonblocking
                // flag, while this bounded reader requires blocking reads.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream);
                assert!(request.starts_with("POST /v1/chat/completions "));
                let body: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_eq!(body["model"], "same-chat-model");
                let user = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                let input: Value = serde_json::from_str(user["content"].as_str().unwrap()).unwrap();
                assert!(input["source"].is_string());
                let poisoned = input["source"]
                    .as_str()
                    .is_some_and(|source| source.contains(POISON_SENTINEL));
                let mut translations = input["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|segment| {
                        json!({"id":segment["id"], "text":format!("{label}{}", segment["text"].as_str().unwrap().trim())})
                    })
                    .collect::<Vec<_>>();
                assert!(!translations.is_empty());
                match mode {
                    ReplyMode::MissingId => {
                        translations.pop();
                    }
                    ReplyMode::DuplicateId => {
                        translations.last_mut().unwrap()["id"] = json!(0);
                    }
                    _ => translations.reverse(),
                }
                let request_number = {
                    let mut captured = captured.lock().unwrap();
                    captured.push(input);
                    captured.len()
                };
                captured_full.lock().unwrap().push(body);
                let valid = json!({"translations":translations}).to_string();
                let invalid = || {
                    format!(
                        "{}\n{INVALID_RESPONSE_SENTINEL}\n{{\"translations\":[",
                        "仅供隔离测试的无效模型前缀。".repeat(256)
                    )
                };
                // Same answer, but every structural separator is the full-width
                // character a Chinese model reaches for. Only the app's
                // structural-punctuation repair can accept this response, and the
                // translated text itself stays byte-identical.
                let full_width_structure = || {
                    let entries = translations
                        .iter()
                        .map(|segment| {
                            format!(
                                "{{\"id\"：{},\"text\"：{}}}",
                                segment["id"],
                                serde_json::to_string(&segment["text"]).unwrap()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("，");
                    format!("{{\"translations\"：[{entries}]}}")
                };
                let content = match mode {
                    ReplyMode::Fenced => format!("```json\n{valid}\n```"),
                    ReplyMode::Prefaced => format!("翻译结果如下：\n{valid}\n"),
                    ReplyMode::MalformedThenValid | ReplyMode::GatedCorrectionValid
                        if request_number == 1 =>
                    {
                        invalid()
                    }
                    ReplyMode::PlainThenValid if request_number == 1 => {
                        "这是没有分段信息的纯文本译文。".to_string()
                    }
                    ReplyMode::AlwaysInvalid | ReplyMode::GatedCorrectionInvalid => invalid(),
                    ReplyMode::FullWidthStructure => full_width_structure(),
                    ReplyMode::PoisonedBlock if poisoned => invalid(),
                    _ => valid,
                };
                let gated = matches!(
                    mode,
                    ReplyMode::GatedCorrectionValid | ReplyMode::GatedCorrectionInvalid
                );
                if gated && request_number == 2 {
                    while !released.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                }
                // Split valid structured output across SSE events, exercising
                // accumulation before shape and ID validation.
                let split = content
                    .char_indices()
                    .nth(content.chars().count() / 2)
                    .unwrap()
                    .0;
                let first = json!({"choices":[{"delta":{"content":&content[..split]},"finish_reason":null}]}).to_string();
                let last = json!({"choices":[{"delta":{"content":&content[split..]},"finish_reason":"stop"}]}).to_string();
                let output = format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n");
                let result = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{output}", output.len())
                    .and_then(|_| stream.flush());
                // A cancelled or superseded correction may close the socket
                // before the fixture releases its deliberately late response.
                if !gated {
                    result.unwrap();
                }
                finished.fetch_add(1, Ordering::Release);
            }
        });
        Self {
            url,
            requests,
            full_requests,
            correction_released,
            responses_finished,
            stopped,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn full_requests(&self) -> Vec<Value> {
        self.full_requests.lock().unwrap().clone()
    }

    fn release_correction(&self) {
        self.correction_released.store(true, Ordering::Release);
    }

    async fn wait_for_requests(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        // Full capture happens after the extracted input capture, so reaching
        // this count also guarantees assertions can inspect both snapshots.
        while self.full_requests.lock().unwrap().len() < count {
            assert!(
                Instant::now() < deadline,
                "mock did not receive request {count}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_responses(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.responses_finished.load(Ordering::Acquire) < count {
            assert!(
                Instant::now() < deadline,
                "mock did not finish response {count}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for TranslationEndpoint {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
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
        assert!(count > 0, "translation mock request ended early");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 1024 * 1024, "oversized fixture request");
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap();
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn settings(endpoint: &TranslationEndpoint) -> ProviderSettings {
    ProviderSettings {
        base_url: endpoint.url.clone(),
        chat_model: "same-chat-model".into(),
        default_language: Some("zh-Hans".into()),
        auto_run_background_jobs: false,
        background_job_interval_ms: 0,
        request_timeout_secs: 10,
        ..ProviderSettings::default()
    }
}

async fn import(services: &AppServices, path: &Path) -> (String, String) {
    let path = path.to_path_buf();
    let book = services
        .spawn_library(move |library| library.import(&path))
        .await
        .unwrap()
        .unwrap();
    let ImportOutcome::Added(book) = book else {
        panic!("fixture must be a fresh import")
    };
    let book_id = book.id.clone();
    let unit_id = services
        .spawn_library_read(move |library| Ok(library.document(&book_id)?.units[0].id.clone()))
        .await
        .unwrap()
        .unwrap();
    (book.id, unit_id)
}

async fn wait_translation(
    services: &AppServices,
    book_id: &str,
    expected: BackgroundJobStatus,
) -> BackgroundJobSnapshot {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let jobs = services
            .background_jobs_for_books(vec![book_id.to_owned()])
            .await
            .unwrap();
        if let Some(job) = jobs.iter().find(|job| job.kind == "translation") {
            if job.status == expected {
                return job.clone();
            }
            assert!(
                job.status != BackgroundJobStatus::Failed,
                "translation failed: {job:?}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "translation did not reach {expected:?}: {jobs:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn translate(services: &AppServices, book_id: &str) -> BackgroundJobSnapshot {
    let job = wait_translation(services, book_id, BackgroundJobStatus::Paused).await;
    assert!(
        services
            .control_background_job(job.id, BackgroundJobAction::Resume)
            .await
            .unwrap()
    );
    wait_translation(services, book_id, BackgroundJobStatus::Succeeded).await
}

fn assert_translated_segments(blocks: &[TranslatedBlock], requests: &[Value], label: &str) {
    assert_eq!(blocks.len(), requests.len());
    for (block, request) in blocks.iter().zip(requests) {
        assert_eq!(block.source, request["source"].as_str().unwrap());
        let segments = request["segments"].as_array().unwrap();
        assert_eq!(block.segments.len(), segments.len());
        for (index, (actual, sent)) in block.segments.iter().zip(segments).enumerate() {
            assert_eq!(sent["id"].as_u64(), Some(index as u64));
            assert_eq!(actual.source, sent["text"].as_str().unwrap());
            let core = actual.source.trim();
            assert_eq!(
                actual.translated,
                actual.source.replacen(core, &format!("{label}{core}"), 1),
                "source spacing must survive a model that trims boundary spaces"
            );
        }
    }
}

fn row_count(services: &AppServices) -> usize {
    rusqlite::Connection::open(services.database_path())
        .unwrap()
        .query_row("SELECT count(*) FROM translations", [], |row| row.get(0))
        .unwrap()
}

fn assert_one_correction(endpoint: &TranslationEndpoint) {
    let requests = endpoint.requests();
    assert_eq!(
        requests.len(),
        2,
        "one correction is the complete retry budget"
    );
    assert_eq!(
        requests[0], requests[1],
        "correction must preserve the source and all segment IDs"
    );
    let full = endpoint.full_requests();
    assert_eq!(full.len(), 2);
    for key in ["model", "temperature", "max_tokens"] {
        assert_eq!(full[0][key], full[1][key], "correction must preserve {key}");
    }
    assert_ne!(
        full[0]["messages"], full[1]["messages"],
        "correction must add a format reminder"
    );
    assert!(full[1]["messages"].to_string().contains("JSON"));
    assert!(
        !full[1].to_string().contains(INVALID_RESPONSE_SENTINEL),
        "raw rejected model output must not enter the repair prompt"
    );
}

#[test]
fn structured_translation_survives_restart_and_replaces_same_model_endpoint_cache() {
    let first = TranslationEndpoint::start("甲：", ReplyMode::ReverseIds);
    let second = TranslationEndpoint::start("乙：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("formatting.epub");
    write_epub(&path, FORMATTED_BODY);
    let data_dir = temp.path().join("library");
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(&data_dir, credentials.clone()).unwrap();
    let (book_id, unit_id, initial) = services.runtime().block_on(async {
        services
            .configure_providers(settings(&first), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        let requests = first.requests();
        assert_eq!(
            requests.len(),
            10,
            "heading, paragraphs, list items, and table cells"
        );
        assert_translated_segments(&blocks, &requests, "甲：");
        let paragraph = requests
            .iter()
            .find(|request| request["source"].as_str().unwrap().starts_with("Start "))
            .unwrap();
        let texts = paragraph["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment["text"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            [
                "Start ", "bold", " and ", "italic", ".", "Next ", "colored", " and ", " end."
            ]
        );
        for (source, expected) in [
            ("Read this now", vec!["Read ", "this", " now"]),
            ("adjacentparts", vec!["adjacent", "parts"]),
        ] {
            let request = requests
                .iter()
                .find(|request| request["source"] == source)
                .unwrap();
            assert_eq!(
                request["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|segment| segment["text"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected,
                "the actual EPUB source text leaves must survive AST normalization"
            );
        }
        assert!(
            paragraph["source"]
                .as_str()
                .unwrap()
                .contains("do_not_translate()"),
            "code remains in context"
        );
        assert!(
            requests
                .iter()
                .flat_map(|request| request["segments"].as_array().unwrap())
                .all(|segment| !segment["text"]
                    .as_str()
                    .unwrap()
                    .contains("do_not_translate")
                    && !segment["text"]
                        .as_str()
                        .unwrap()
                        .contains("leave this code unchanged"))
        );
        assert_eq!(row_count(&services), 10);
        (book_id, unit_id, blocks)
    });
    drop(services);

    let services = AppServices::open_with_credentials(&data_dir, credentials).unwrap();
    services.runtime().block_on(async {
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap(),
            initial
        );
        wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert_eq!(
            first.requests().len(),
            10,
            "current persisted output must not be requested again"
        );

        services
            .configure_providers(settings(&second), BTreeMap::new())
            .await
            .unwrap();
        wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            row_count(&services),
            0,
            "old endpoint output must not remain cached under the same model name"
        );
        let job = translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_translated_segments(&blocks, &second.requests(), "乙：");
        assert_eq!(first.requests().len(), 10);

        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Retranslate)
                .await
                .unwrap()
        );
        assert_eq!(row_count(&services), 0);
        translate(&services, &book_id).await;
        assert_eq!(second.requests().len(), 20);
        assert_eq!(
            row_count(&services),
            10,
            "retranslation replaces rows without duplication"
        );
    });
}

#[test]
fn incomplete_or_duplicate_response_ids_fail_before_persisting_a_block() {
    for mode in [ReplyMode::MissingId, ReplyMode::DuplicateId] {
        let endpoint = TranslationEndpoint::start("错误：", mode);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("invalid-response.epub");
        write_epub(&path, "<p>First <strong>second</strong> third.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
            assert!(
                services
                    .control_background_job(job.id, BackgroundJobAction::Resume)
                    .await
                    .unwrap()
            );
            let failed = wait_translation(&services, &book_id, BackgroundJobStatus::Failed).await;
            assert!(failed.error.is_some());
            assert_eq!(failed.progress.completed, 0);
            assert_one_correction(&endpoint);
            assert_eq!(
                row_count(&services),
                0,
                "{mode:?} must not persist a partial paragraph"
            );
            assert!(
                services
                    .translation_blocks_for_unit(book_id, unit_id)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    }
}

#[test]
fn fenced_and_prefaced_structured_responses_succeed_without_correction() {
    for mode in [ReplyMode::Fenced, ReplyMode::Prefaced] {
        let endpoint = TranslationEndpoint::start("译：", mode);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wrapped-response.epub");
        write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            translate(&services, &book_id).await;
            let blocks = services
                .translation_blocks_for_unit(book_id, unit_id)
                .await
                .unwrap();
            assert_eq!(
                endpoint.requests().len(),
                1,
                "{mode:?} is a valid wrapped structured response"
            );
            assert_eq!(row_count(&services), 1);
            assert_translated_segments(&blocks, &endpoint.requests(), "译：");
        });
    }
}

#[test]
fn malformed_or_plain_first_response_gets_one_immutable_structured_correction() {
    for mode in [ReplyMode::MalformedThenValid, ReplyMode::PlainThenValid] {
        let endpoint = TranslationEndpoint::start("修正：", mode);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("correctable-response.epub");
        write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            translate(&services, &book_id).await;
            let blocks = services
                .translation_blocks_for_unit(book_id, unit_id)
                .await
                .unwrap();
            assert_one_correction(&endpoint);
            assert_eq!(row_count(&services), 1);
            assert_translated_segments(&blocks, &endpoint.requests()[1..], "修正：");
        });
    }
}

#[test]
fn exhausted_correction_fails_after_two_calls_without_storing_response_text() {
    let endpoint = TranslationEndpoint::start("无效：", ReplyMode::AlwaysInvalid);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("exhausted-response.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let failed = wait_translation(&services, &book_id, BackgroundJobStatus::Failed).await;
        assert_one_correction(&endpoint);
        let error = failed.error.unwrap();
        assert!(!error.contains(INVALID_RESPONSE_SENTINEL));
        assert!(!error.contains("仅供隔离测试的无效模型前缀"));
        assert!(
            error.len() < 1024,
            "job error must remain bounded and independent of provider text"
        );
        assert_eq!(failed.progress.completed, 0);
        assert_eq!(row_count(&services), 0);
        assert!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id)
                .await
                .unwrap()
                .is_empty()
        );
        let persisted_error: String = rusqlite::Connection::open(services.database_path())
            .unwrap()
            .query_row(
                "SELECT error FROM index_jobs WHERE book_id = ?1 AND kind = 'translation'",
                [&book_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_error, error);
    });
}

#[test]
fn full_width_structure_punctuation_is_repaired_without_a_correction() {
    // 现场成因：模型把 JSON 的结构冒号、逗号写成全角标点。归一化只改结构位置，
    // 因此不需要纠正请求，译文内容逐字保持模型给出的文本。
    let endpoint = TranslationEndpoint::start("全角：", ReplyMode::FullWidthStructure);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("full-width-structure.epub");
    write_epub(
        &path,
        "<p>Read <strong>carefully</strong>.</p><p>Second <em>block</em>.</p>",
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(
            endpoint.requests().len(),
            2,
            "结构标点归一后不需要额外纠正请求"
        );
        assert_eq!(row_count(&services), 2);
        assert_translated_segments(&blocks, &endpoint.requests(), "全角：");
    });
}

#[test]
fn one_untranslatable_block_is_skipped_and_the_run_continues() {
    let endpoint = TranslationEndpoint::start("跳过：", ReplyMode::PoisonedBlock);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("skipped-block.epub");
    write_epub(
        &path,
        &format!(
            "<p>First block</p><p>{POISON_SENTINEL} never validates</p>\
             <p>Third block</p><p>Fourth block</p>"
        ),
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = translate(&services, &book_id).await;

        // 四个文本块：一个始终非法（两次请求），其余各一次请求。
        assert_eq!(endpoint.requests().len(), 5);
        let logs = services
            .background_job_logs(job.id.clone(), vec![book_id.clone()])
            .await
            .unwrap();
        let skipped: Vec<_> = logs
            .entries
            .iter()
            .filter(|entry| entry.message.contains("跳过当前文本块"))
            .collect();
        assert_eq!(skipped.len(), 1, "跳过的文本块必须留下可读记录");
        assert_eq!(skipped[0].metrics.ordinal, Some(1));
        assert_eq!(
            skipped[0].level,
            moye_epub_editor::job_diagnostics::JobLogLevel::Warning
        );
        for entry in &logs.entries {
            assert!(
                !entry.format_line().contains(POISON_SENTINEL),
                "任务日志不得包含正文"
            );
        }

        // 跳过不写入译文，但游标继续前进，读者看到的是原文。
        assert_eq!(row_count(&services), 3);
        assert_eq!(job.progress.completed, 4);
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(
            blocks
                .iter()
                .all(|block| !block.source.contains(POISON_SENTINEL))
        );
    });
}

#[test]
fn consecutive_untranslatable_blocks_fail_the_run_without_losing_progress() {
    let endpoint = TranslationEndpoint::start("失败：", ReplyMode::PoisonedBlock);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("consecutive-skips.epub");
    write_epub(
        &path,
        &format!(
            "<p>First block</p><p>{POISON_SENTINEL} one</p><p>{POISON_SENTINEL} two</p>\
             <p>{POISON_SENTINEL} three</p><p>Last block</p>"
        ),
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, _unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let failed = wait_translation(&services, &book_id, BackgroundJobStatus::Failed).await;

        // 一个成功块加三个连续失败块：第三个失败块上停止，最后一个块不再请求。
        assert_eq!(endpoint.requests().len(), 7);
        let error = failed.error.unwrap();
        assert!(error.contains("连续 3 个文本块"));
        assert!(!error.contains(POISON_SENTINEL));
        assert!(error.len() < 1024);
        // 失败沿用最后一个已提交游标，重试仍会从第一个未翻译块开始。
        assert_eq!(failed.progress.completed, 1);
        assert_eq!(row_count(&services), 1);
    });
}

#[test]
fn pause_and_cancel_interrupt_the_correction_before_it_can_publish() {
    for (action, expected) in [
        (BackgroundJobAction::Pause, BackgroundJobStatus::Paused),
        (BackgroundJobAction::Cancel, BackgroundJobStatus::Cancelled),
    ] {
        let endpoint = TranslationEndpoint::start("修正：", ReplyMode::GatedCorrectionValid);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("controlled-correction.epub");
        write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
            assert!(
                services
                    .control_background_job(job.id.clone(), BackgroundJobAction::Resume)
                    .await
                    .unwrap()
            );
            endpoint.wait_for_requests(2).await;
            assert_one_correction(&endpoint);
            assert_eq!(row_count(&services), 0);
            assert!(
                services
                    .control_background_job(job.id, action)
                    .await
                    .unwrap()
            );
            let controlled = wait_translation(&services, &book_id, expected).await;
            assert_eq!(controlled.progress.completed, 0);
            endpoint.release_correction();
            endpoint.wait_for_responses(2).await;
            assert!(
                services
                    .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(row_count(&services), 0);
            assert_eq!(endpoint.requests().len(), 2);
            if expected == BackgroundJobStatus::Paused {
                translate(&services, &book_id).await;
                assert_eq!(
                    endpoint.requests().len(),
                    3,
                    "resume restarts the uncommitted block"
                );
                let blocks = services
                    .translation_blocks_for_unit(book_id.clone(), unit_id)
                    .await
                    .unwrap();
                assert_translated_segments(&blocks, &endpoint.requests()[2..], "修正：");
            } else {
                wait_translation(&services, &book_id, BackgroundJobStatus::Cancelled).await;
            }
        });
    }
}

#[test]
fn late_invalid_correction_cannot_override_a_reconfigured_endpoint_success() {
    let old = TranslationEndpoint::start("旧：", ReplyMode::GatedCorrectionInvalid);
    let next = TranslationEndpoint::start("新：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("reconfigured-correction.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&old), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        old.wait_for_requests(2).await;
        services
            .configure_providers(settings(&next), BTreeMap::new())
            .await
            .unwrap();
        translate(&services, &book_id).await;
        assert_eq!(next.requests().len(), 1);
        old.release_correction();
        old.wait_for_responses(2).await;
        let current = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert!(current.error.is_none());
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_translated_segments(&blocks, &next.requests(), "新：");
        assert_eq!(row_count(&services), 1);
        assert_one_correction(&old);
    });
}

#[test]
fn failed_cursor_publication_settles_the_owned_execution_and_retry_reuses_saved_output() {
    let endpoint = TranslationEndpoint::start("保留：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cursor-publication-failure.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        {
            // Fail only cursor advancement in this isolated database. Saving
            // the translated block and settling its old cursor remain legal.
            let conn = rusqlite::Connection::open(services.database_path()).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER fixture_reject_translation_progress
                 BEFORE UPDATE OF cursor_json ON index_jobs
                 WHEN NEW.kind = 'translation'
                   AND json_extract(NEW.cursor_json, '$.next_ordinal')
                     > json_extract(OLD.cursor_json, '$.next_ordinal')
                 BEGIN
                   SELECT RAISE(ABORT, 'fixture rejected translation progress');
                 END;",
            )
            .unwrap();
        }
        assert!(
            services
                .control_background_job(job.id.clone(), BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let failed = wait_translation(&services, &book_id, BackgroundJobStatus::Failed).await;
        assert_eq!(
            failed.progress.completed, 0,
            "a failed write must retain the last committed cursor"
        );
        assert!(failed.error.is_some());
        assert_eq!(endpoint.requests().len(), 1);
        assert_eq!(
            row_count(&services),
            1,
            "the block was committed before progress publication failed"
        );
        {
            let conn = rusqlite::Connection::open(services.database_path()).unwrap();
            let (status, cursor): (String, String) = conn
                .query_row(
                    "SELECT status, cursor_json FROM index_jobs WHERE id = ?1",
                    [&job.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                status, "failed",
                "the failed execution must not remain workerless and running"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&cursor).unwrap()["next_ordinal"],
                0
            );
            conn.execute_batch("DROP TRIGGER fixture_reject_translation_progress;")
                .unwrap();
        }
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Retry)
                .await
                .unwrap()
        );
        let succeeded = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert_eq!(succeeded.progress.completed, 1);
        assert!(succeeded.error.is_none());
        assert_eq!(
            endpoint.requests().len(),
            1,
            "retry must reuse the already committed structured block"
        );
        assert_eq!(row_count(&services), 1);
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_translated_segments(&blocks, &endpoint.requests(), "保留：");
    });
}

#[test]
fn opening_a_legacy_plain_text_cache_requires_fresh_structured_translation() {
    let endpoint = TranslationEndpoint::start("新：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.epub");
    write_epub(&path, "<p>Legacy <strong>format</strong>.</p>");
    let data_dir = temp.path().join("library");
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(&data_dir, credentials.clone()).unwrap();
    let (book_id, unit_id, job_id) = services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = translate(&services, &book_id).await;
        (book_id, unit_id, job.id)
    });
    let db_path = services.database_path().to_path_buf();
    drop(services);
    {
        // Only this isolated fixture is changed; its completed old-version
        // cursor and plaintext row reproduce a pre-format-preservation cache.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let cursor: String = conn
            .query_row(
                "SELECT cursor_json FROM index_jobs WHERE id = ?1",
                [&job_id],
                |row| row.get(0),
            )
            .unwrap();
        let mut cursor: Value = serde_json::from_str(&cursor).unwrap();
        cursor["execution_identity"] = json!("translation-v1:legacy-fixture");
        conn.execute(
            "UPDATE index_jobs SET cursor_json = ?1 WHERE id = ?2",
            rusqlite::params![cursor.to_string(), job_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE translations SET translated_text = 'old flat translation'",
            [],
        )
        .unwrap();
    }
    let services = AppServices::open_with_credentials(&data_dir, credentials).unwrap();
    services.runtime().block_on(async {
        wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert_eq!(row_count(&services), 0);
        assert!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap()
                .is_empty()
        );
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(
            endpoint.requests().len(),
            2,
            "legacy success cursor cannot skip new structured output"
        );
        assert_translated_segments(&blocks, &endpoint.requests()[1..], "新：");
    });
}

fn write_epub(path: &Path, body: &str) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let chapter = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Formatting</title></head><body>{body}</body></html>"#
    );
    for (name, content) in [
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        ),
        (
            "EPUB/package.opf",
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="book-id">urn:uuid:translation-format-fixture</dc:identifier><dc:title>Translation Formatting Fixture</dc:title><dc:creator>Fixture</dc:creator><dc:language>en</dc:language><meta property="dcterms:modified">2026-09-11T00:00:00Z</meta></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
        ),
        (
            "EPUB/nav.xhtml",
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Formatting</a></li></ol></nav></body></html>"#,
        ),
        ("EPUB/chapter.xhtml", chapter.as_str()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}
