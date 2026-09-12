use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use moye_epub_editor::{
    credentials::MemoryCredentialStore,
    job_diagnostics::JobLogErrorKind,
    library::ImportOutcome,
    services::{
        AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
        ProviderSettings, TranslatedBlock,
    },
    translation::TranslationSegment,
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
    /// 现场（2026-09-12，`qwen3.5:0.8b`）：答案完全正确，但丢掉了 `translations`
    /// 外壳，被围栏包裹的顶层数组就是整个响应。
    BareSegmentArray,
    /// Valid answers everywhere except blocks carrying [`POISON_SENTINEL`].
    PoisonedBlock,
    /// One complete valid answer, then a stream that never ends: the fixture
    /// declares one byte more than it sends and keeps the socket open.
    AnswerThenSilence,
}

/// Process-wide start marker so every fixture event carries a comparable
/// timestamp without pulling in another dependency.
static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// How long the fixture waits for one request before it treats the connection
/// as stray. It has to stay clearly above the clients' own timeouts: a fixture
/// that gives up first turns a slow machine into what looks like a product
/// hang, and the real failure then has to be inferred with no evidence.
const FIXTURE_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one fixture may take to see a request or to finish writing an
/// answer. Every case in this file runs in parallel, so a loaded machine can
/// need several times the idle time for the same run; the budget only has to
/// outlast that, while a request that truly never arrives still fails and now
/// prints the fixture's own timeline as evidence.
const FIXTURE_WAIT: Duration = Duration::from_secs(30);

fn elapsed_ms() -> u128 {
    PROCESS_START.elapsed().as_millis()
}

/// Observable state of one mock endpoint, registered process-wide so a failure
/// can tell "the fixture never received that request" apart from "the fixture
/// answered and the client never used the answer" without threading the fixture
/// through every helper that only takes the services handle.
struct EndpointStats {
    label: &'static str,
    accepted: AtomicUsize,
    requests: AtomicUsize,
    responses: AtomicUsize,
    events: Mutex<Vec<String>>,
}

impl EndpointStats {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            accepted: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
            responses: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    /// Timestamped one-liner. The list is bounded: the point is the shape of a
    /// failure, not an unbounded log, and a fixture that ran away must not grow
    /// the process while it does.
    fn note(&self, event: impl Into<String>) {
        let mut events = self.events.lock().unwrap();
        if events.len() < 512 {
            events.push(format!("+{}ms {}", elapsed_ms(), event.into()));
        }
    }
}

static ENDPOINTS: LazyLock<Mutex<Vec<Arc<EndpointStats>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Every fixture's timeline in this test process, appended to job failures so a
/// dead or starved fixture is never mistaken for a hung product. Other cases in
/// the same binary are included on purpose: they show the machine-wide picture
/// at the moment of the failure.
fn fixture_timeline() -> String {
    let endpoints = ENDPOINTS.lock().unwrap();
    let mut report = String::from("\n  --- fixture timeline ---");
    for endpoint in endpoints.iter() {
        report.push_str(&format!(
            "\n  fixture {:?}: accepted={} requests={} responses={}",
            endpoint.label,
            endpoint.accepted.load(Ordering::Acquire),
            endpoint.requests.load(Ordering::Acquire),
            endpoint.responses.load(Ordering::Acquire),
        ));
        for event in endpoint.events.lock().unwrap().iter() {
            report.push_str(&format!("\n    {event}"));
        }
    }
    report
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
        let stats = Arc::new(EndpointStats::new(label));
        ENDPOINTS.lock().unwrap().push(Arc::clone(&stats));
        let fixture = Arc::clone(&stats);
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
                stream.set_read_timeout(Some(FIXTURE_READ_TIMEOUT)).unwrap();
                let connection = fixture.accepted.fetch_add(1, Ordering::AcqRel) + 1;
                fixture.note(format!("connection {connection} accepted"));
                // A connection a client opened and then abandoned, or one the
                // machine is too loaded to deliver in time, must not kill the
                // fixture: the request still has to arrive, and the job's own
                // timeouts turn a bad one into a visible failure instead of a
                // stall nobody can attribute.
                let request = match read_request(&mut stream) {
                    Ok(request) => request,
                    Err(error) => {
                        fixture.note(format!("connection {connection} dropped: {error}"));
                        continue;
                    }
                };
                fixture.note(format!("connection {connection}: {} bytes", request.len()));
                assert!(
                    request.starts_with("POST /v1/chat/completions "),
                    "unexpected fixture request: {:?}",
                    request.lines().next().unwrap_or_default()
                );
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
                fixture.requests.fetch_add(1, Ordering::AcqRel);
                fixture.note(format!(
                    "request #{request_number}: {} segments{}",
                    translations.len(),
                    if poisoned { " (poisoned block)" } else { "" }
                ));
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
                    ReplyMode::BareSegmentArray => format!("```json\n{}\n```", json!(translations)),
                    ReplyMode::PoisonedBlock if poisoned => invalid(),
                    _ => valid,
                };
                let gated = matches!(
                    mode,
                    ReplyMode::GatedCorrectionValid | ReplyMode::GatedCorrectionInvalid
                );
                if gated && request_number == 2 {
                    fixture.note("correction gated until the test releases it");
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
                let silent = matches!(mode, ReplyMode::AnswerThenSilence);
                let output = if silent {
                    // 完整的答案，但没有 `[DONE]`：流始终没有结束。
                    format!("data: {first}\n\ndata: {last}\n\n")
                } else {
                    format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n")
                };
                // 静默夹具声明比实际多一字节的响应体：客户端读完已有字节后会继续等待，
                // 直到空闲超时，用来验证「答案完整、流没有结束」的可挽救路径。
                let declared = output.len() + usize::from(silent);
                let result = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n{output}")
                    .and_then(|_| stream.flush());
                // A cancelled or superseded correction may close the socket
                // before the fixture releases its deliberately late response.
                if !gated {
                    result.unwrap();
                }
                fixture.note(format!(
                    "response #{request_number}: {declared} bytes declared{}",
                    if silent { ", stream left open" } else { "" }
                ));
                if silent {
                    fixture.note("holding the answer open for the silence fixture");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
                fixture.responses.fetch_add(1, Ordering::AcqRel);
                finished.fetch_add(1, Ordering::Release);
            }
            fixture.note("fixture stopped");
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
        let deadline = Instant::now() + FIXTURE_WAIT;
        // Full capture happens after the extracted input capture, so reaching
        // this count also guarantees assertions can inspect both snapshots.
        while self.full_requests.lock().unwrap().len() < count {
            assert!(
                Instant::now() < deadline,
                "mock did not receive request {count}{}",
                fixture_timeline()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_responses(&self, count: usize) {
        let deadline = Instant::now() + FIXTURE_WAIT;
        while self.responses_finished.load(Ordering::Acquire) < count {
            assert!(
                Instant::now() < deadline,
                "mock did not finish response {count}{}",
                fixture_timeline()
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

/// Reads one complete request. Everything a stray, abandoned or malformed
/// connection can cause comes back as an error so the fixture drops that one
/// connection and keeps serving instead of dying on the first hiccup.
fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request ended early",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() >= 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "oversized fixture request",
            ));
        }
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>())
                .transpose()
                .map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                })?;
            let Some(length) = length else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "request without a content length",
                ));
            };
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn settings(endpoint: &TranslationEndpoint) -> ProviderSettings {
    settings_with_timeout(endpoint, 10)
}

/// 请求超时是「静默」上限，不再是整段调用的总时长：夹具需要它足够短才能在不拖慢
/// 测试的前提下触发空闲结束。
fn settings_with_timeout(
    endpoint: &TranslationEndpoint,
    request_timeout_secs: u64,
) -> ProviderSettings {
    ProviderSettings {
        base_url: endpoint.url.clone(),
        chat_model: "same-chat-model".into(),
        default_language: Some("zh-Hans".into()),
        auto_run_background_jobs: false,
        background_job_interval_ms: 0,
        request_timeout_secs,
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

/// How long a run may stop making progress before the fixture gives up on it.
/// Every case in this file runs in parallel, so a loaded machine can make an
/// honest run take several times longer than an idle one; what must never be
/// tolerated is a job that stops advancing, which is exactly what this budget
/// watches. A real hang therefore fails in 30 s while a slow but progressing
/// run is allowed to finish.
const TRANSLATION_STALL: Duration = Duration::from_secs(30);
/// Absolute ceiling for one run, so a job that keeps advancing forever still
/// ends the test instead of blocking the suite.
const TRANSLATION_WAIT: Duration = Duration::from_secs(240);

async fn wait_translation(
    services: &AppServices,
    book_id: &str,
    expected: BackgroundJobStatus,
) -> BackgroundJobSnapshot {
    let started = Instant::now();
    let mut observed: Option<(BackgroundJobStatus, usize)> = None;
    let mut progressed_at = Instant::now();
    loop {
        let jobs = background_job_snapshots(services, book_id).await;
        if let Some(job) = jobs.iter().find(|job| job.kind == "translation") {
            if job.status == expected {
                return job.clone();
            }
            assert!(
                job.status != BackgroundJobStatus::Failed,
                "translation failed: {job:?}{}",
                fixture_timeline()
            );
            if observed != Some((job.status, job.progress.completed)) {
                observed = Some((job.status, job.progress.completed));
                progressed_at = Instant::now();
            }
        }
        let silent = progressed_at.elapsed();
        assert!(
            silent < TRANSLATION_STALL && started.elapsed() < TRANSLATION_WAIT,
            "translation did not reach {expected:?}: no progress for {}s (last {:?}, waited {}s): {jobs:?}{}",
            silent.as_secs(),
            observed,
            started.elapsed().as_secs(),
            fixture_timeline()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Reads the task list with the fixture timeline attached, so a database error
/// on a loaded machine keeps its own message instead of looking like a hang.
///
/// A poll can meet a write transaction another connection is holding. The
/// database is healthy: it is this harness's own 20 ms polling, each tick
/// opening a fresh connection through the application's API, that collides with
/// the writes of the run it is watching. A bounded retry therefore keeps a
/// transient lock from failing an untouched run, while a database that stays
/// unavailable still fails the case with the error and the timeline.
async fn background_job_snapshots(
    services: &AppServices,
    book_id: &str,
) -> Vec<BackgroundJobSnapshot> {
    let deadline = Instant::now() + TRANSLATION_STALL;
    loop {
        match services
            .background_jobs_for_books(vec![book_id.to_owned()])
            .await
        {
            Ok(jobs) => return jobs,
            Err(error) if is_database_busy(&error) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!(
                "background job query failed: {error:#}{}",
                fixture_timeline()
            ),
        }
    }
}

/// Whether a failure is the transient "another connection holds the lock" case
/// that a busy machine can provoke, rather than a real database problem.
fn is_database_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(inner, _))
                if inner.code == rusqlite::ErrorCode::DatabaseBusy
                    || inner.code == rusqlite::ErrorCode::DatabaseLocked
        )
    })
}

/// Opens the fixture's own database for a direct assertion or for a deliberate
/// change to one row. It gets the same lock patience the application's own
/// connections use: every case in this file runs in parallel, and the
/// application may be in the middle of a write transaction when it looks.
fn open_fixture_conn(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
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
    open_fixture_conn(services.database_path())
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
fn structured_translation_survives_restart_and_a_changed_endpoint_keeps_its_text() {
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
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        // A changed endpoint or model keeps what is already translated: the rows
        // are restamped with the new identity instead of being discarded, so a
        // reader never loses译文 for a book that was already translated.
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap(),
            initial
        );
        assert_eq!(
            row_count(&services),
            10,
            "existing译文 must survive a changed endpoint"
        );
        assert_eq!(
            second.requests().len(),
            0,
            "already translated blocks are not requested again"
        );

        // The explicit re-translation still discards the stored text and rebuilds
        // the book with the new endpoint.
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Retranslate)
                .await
                .unwrap()
        );
        assert_eq!(row_count(&services), 0);
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_translated_segments(&blocks, &second.requests(), "乙：");
        assert_eq!(second.requests().len(), 10);
        assert_eq!(first.requests().len(), 10);
        assert_eq!(
            row_count(&services),
            10,
            "retranslation replaces rows without duplication"
        );
    });
}

/// 阅读窗口可以手工改写某一段的机器译文。手工译文必须是读者看到的正文，不能被后续
/// 机器运行覆盖，重启与换端点后仍然有效，并且机器原文行始终保留在它背后，所以
/// 「恢复机器译文」不需要重新调用模型。
#[test]
fn a_manual_translation_overrides_the_model_text_and_survives_a_changed_endpoint() {
    let first = TranslationEndpoint::start("译：", ReplyMode::ReverseIds);
    let second = TranslationEndpoint::start("再：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("manual.epub");
    write_epub(
        &path,
        "<p>Manual <strong>edit</strong> target</p><p>Second paragraph</p>",
    );
    let data_dir = temp.path().join("library");
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(&data_dir, credentials.clone()).unwrap();
    let (book_id, unit_id, key, machine, manual) = services.runtime().block_on(async {
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
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|block| !block.manual));
        let target = blocks
            .iter()
            .find(|block| block.source.starts_with("Manual"))
            .unwrap();
        assert_eq!(
            target.segments.len(),
            3,
            "formatting leaves become separate segments"
        );
        let machine = target.segments.clone();
        let untouched = blocks
            .iter()
            .filter(|block| block.key != target.key)
            .cloned()
            .collect::<Vec<_>>();

        // 页面只能描述改动：与已存片段对不上的请求一律拒绝，且必须一个字节都不写。
        let mut rewritten = machine.clone();
        rewritten[0].source = "Rewritten".into();
        let mut short = machine.clone();
        short.pop();
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(rewritten),
                )
                .await
                .is_err(),
            "a rewritten source must be refused"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(short),
                )
                .await
                .is_err(),
            "a truncated segment list must be refused"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(vec![machine[0].clone(); 513]),
                )
                .await
                .is_err(),
            "an oversized payload must be refused before any database work"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    "missing-block".into(),
                    Some(machine.clone()),
                )
                .await
                .is_err(),
            "a block without a stored row must be refused"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(Vec::new())
                )
                .await
                .is_err(),
            "an empty edit must be refused"
        );
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap(),
            blocks,
            "a refused edit leaves the stored translation untouched"
        );

        let manual = machine
            .iter()
            .map(|segment| TranslationSegment {
                source: segment.source.clone(),
                translated: format!("手工：{}", segment.translated.trim()),
            })
            .collect::<Vec<_>>();
        services
            .set_manual_translation(
                book_id.clone(),
                unit_id.clone(),
                target.key.clone(),
                Some(manual.clone()),
            )
            .await
            .unwrap();
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_eq!(
            blocks.iter().filter(|block| block.manual).count(),
            1,
            "only the edited block is manual"
        );
        let edited = blocks.iter().find(|block| block.key == target.key).unwrap();
        assert!(edited.manual);
        assert_eq!(edited.segments, manual);
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.key != target.key)
                .cloned()
                .collect::<Vec<_>>(),
            untouched,
            "the other block keeps its machine text"
        );
        assert_eq!(row_count(&services), 2, "a manual edit adds no row");
        (book_id, unit_id, target.key.clone(), machine, manual)
    });
    drop(services);

    let services = AppServices::open_with_credentials(&data_dir, credentials).unwrap();
    services.runtime().block_on(async {
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        let edited = blocks.iter().find(|block| block.key == key).unwrap();
        assert!(edited.manual, "a manual edit survives a restart");
        assert_eq!(edited.segments, manual);

        services
            .configure_providers(settings(&second), BTreeMap::new())
            .await
            .unwrap();
        wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert_eq!(
            second.requests().len(),
            0,
            "a manually edited block is still a cache hit for the model"
        );
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_eq!(
            blocks
                .iter()
                .find(|block| block.key == key)
                .unwrap()
                .segments,
            manual,
            "a changed endpoint does not hide the reader's own text"
        );

        services
            .set_manual_translation(book_id.clone(), unit_id.clone(), key.clone(), None)
            .await
            .unwrap();
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        let restored = blocks.iter().find(|block| block.key == key).unwrap();
        assert!(!restored.manual, "restoring drops the manual marker");
        assert_eq!(
            restored.segments, machine,
            "the machine text was kept behind the manual edit"
        );
        assert_eq!(
            second.requests().len(),
            0,
            "restoring does not spend a model call"
        );
        assert_eq!(row_count(&services), 2);
    });
}

#[test]
fn editing_one_chapter_keeps_the_other_chapters_translation() {
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("two-chapters.epub");
    write_two_chapter_epub(
        &path,
        "<p>first chapter words</p>",
        "<p>second chapter words</p>",
    );
    let data_dir = temp.path().join("library");
    let services =
        AppServices::open_with_credentials(&data_dir, Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, first_unit_id) = import(&services, &path).await;
        let second_unit_id = services
            .spawn_library_read({
                let book_id = book_id.clone();
                move |library| Ok(library.document(&book_id)?.units[1].id.clone())
            })
            .await
            .unwrap()
            .unwrap();
        translate(&services, &book_id).await;
        assert_eq!(endpoint.requests().len(), 2, "one block per chapter");
        let kept = services
            .translation_blocks_for_unit(book_id.clone(), second_unit_id.clone())
            .await
            .unwrap();
        assert_eq!(kept.len(), 1);

        // Editing the first chapter publishes a new revision, but the untouched
        // chapter keeps its译文 and is not translated again.
        let edited_book = book_id.clone();
        let edited_unit = first_unit_id.clone();
        services
            .spawn_library(move |library| {
                library.update_content_unit_source(
                    &edited_book,
                    &edited_unit,
                    "<p>edited chapter words</p>",
                )
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), second_unit_id.clone())
                .await
                .unwrap(),
            kept,
            "the untouched chapter keeps its译文"
        );

        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        let before_edit_requests = endpoint.requests().len();
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        let requested = endpoint
            .requests()
            .iter()
            .skip(before_edit_requests)
            .map(|request| request["source"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(
            requested
                .iter()
                .any(|source| source == "edited chapter words"),
            "the edited chapter is translated again: {requested:?}"
        );
        assert!(
            !requested
                .iter()
                .any(|source| source == "second chapter words"),
            "an untouched chapter's own text is never translated again: {requested:?}"
        );
        let kept_after = services
            .translation_blocks_for_unit(book_id.clone(), second_unit_id.clone())
            .await
            .unwrap();
        assert!(
            kept_after
                .iter()
                .any(|block| block.source == "second chapter words"),
            "the untouched chapter still reads its译文: {kept_after:?}"
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
fn a_bare_segment_array_answer_is_accepted_without_a_correction() {
    // 现场（2026-09-12，qwen3.5:0.8b）：小模型丢掉了 `translations` 外壳，逐块返回被
    // 围栏包裹的顶层数组。旧协议把每个块都判成 invalid_schema，连续跳过 3 块后整本书
    // 的翻译任务直接失败（translation_run_finish result="failed"）。
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::BareSegmentArray);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bare-array-response.epub");
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
            "a bare segment array is a complete answer and must not need a correction"
        );
        assert_eq!(row_count(&services), 1);
        assert_translated_segments(&blocks, &endpoint.requests(), "译：");
    });
}

/// 手工门禁（默认 ignored）：把现场日志里的三个文本块原样交给真实本地模型，复跑一次
/// 真实的整本翻译。现场（2026-09-12 13:19，`qwen3.5:0.8b`）：模型丢掉了 `translations`
/// 外壳、逐块返回被围栏包裹的顶层数组，整本书的块全部判 `invalid_schema`，连续 3 块未译
/// 后 `translation_run_finish result="failed"`。
///
/// ```text
/// cargo test --test translation_flow -- --ignored --nocapture a_local_model
/// ```
///
/// 需要 127.0.0.1:11434 上的 Ollama（`MOYE_REPLAY_ENDPOINT` / `MOYE_REPLAY_MODEL`
/// 可覆盖）；`MOYE_REPLAY_LOG` 调诊断级别。默认测试不运行它，也不触碰用户图书库。
#[test]
#[ignore = "manual gate: needs a local OpenAI-compatible model endpoint"]
fn a_local_model_replays_the_previously_rejected_blocks() {
    // 诊断行必须能直接在输出里看到，才能和现场日志逐条比对。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("MOYE_REPLAY_LOG")
                .map(tracing_subscriber::EnvFilter::new)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,moye_ai=debug")),
        )
        .try_init();
    let base_url = std::env::var("MOYE_REPLAY_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:11434/v1/".to_string());
    let model = std::env::var("MOYE_REPLAY_MODEL").unwrap_or_else(|_| "qwen3.5:0.8b".to_string());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local-model-replay.epub");
    // 与现场日志逐字一致：连片段切分方式（inline 元素边界）都照抄。
    write_epub(
        &path,
        "<h1>Praise for <em>Head First Agile</em></h1>\
         <p>Praise for other <em>Head First books</em></p>\
         <p>Your name could be here! We’re looking for early praise from project managers, \
         developers, business anaylsts, and anyone else who’s read the early release of our book. \
         Contact us at <a href=\"mailto:info@stellman-greene.com\">info@stellman-greene.com</a>.</p>",
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        let listing: Value = reqwest::get(format!("{base_url}models"))
            .await
            .unwrap_or_else(|error| {
                panic!("本机没有可用的 OpenAI 兼容端点 {base_url}（先启动模型服务）：{error}")
            })
            .json()
            .await
            .unwrap();
        assert!(
            listing["data"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["id"] == model)),
            "端点没有 {model}：{listing}"
        );
        services
            .configure_providers(
                ProviderSettings {
                    base_url: base_url.clone(),
                    chat_model: model.clone(),
                    default_language: Some("zh-Hans".into()),
                    auto_run_background_jobs: false,
                    background_job_interval_ms: 0,
                    // 与现场一致：120 秒只是静默上限，不是整段请求的总时长。
                    request_timeout_secs: 120,
                    ..ProviderSettings::default()
                },
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id)
            .await
            .unwrap();
        for block in &blocks {
            println!(
                "{} => {:?}",
                block.source,
                block
                    .segments
                    .iter()
                    .map(|segment| segment.translated.as_str())
                    .collect::<Vec<_>>()
            );
        }
        // 关键回归：现场那批块被拒的原因只有外壳。持久化任务日志里不允许再出现
        // `invalid_schema`；剩下的不足只能是「模型把多段合并成一段」的数量检查失败，
        // 那是协议该做的正当拒绝。
        let logs = services
            .background_job_logs(job.id.clone(), vec![book_id.clone()])
            .await
            .unwrap();
        let rejected = logs
            .entries
            .iter()
            .filter(|entry| entry.metrics.error_kind == Some(JobLogErrorKind::InvalidSchema))
            .map(|entry| entry.format_line())
            .collect::<Vec<_>>();
        assert!(
            rejected.is_empty(),
            "顶层数组外壳仍被判 invalid_schema：{rejected:?}"
        );
        // 两个 2 段块在旧协议下只会回 invalid_schema，而模型的 id 是齐的，必须落库。
        for source in [
            "Praise for Head First Agile",
            "Praise for other Head First books",
        ] {
            assert!(
                blocks.iter().any(|block| block.source == source),
                "{source:?} 必须有译文行，实际拿到：{:?}",
                blocks
                    .iter()
                    .map(|block| block.source.as_str())
                    .collect::<Vec<_>>()
            );
        }
        assert!(row_count(&services) >= 2);
    });
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
        let persisted_error: String = open_fixture_conn(services.database_path())
            .query_row(
                "SELECT error FROM index_jobs WHERE book_id = ?1 AND kind = 'translation'",
                [&book_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_error, error);
    });
}

/// 2026-09-11 现场：本地模型的流在 120 秒请求超时里已经输出了完整译文，却一直没有
/// 结束（1579 个正文块、7024 字节正文、没有 `[DONE]`），旧的整体请求超时把整本图书
/// 判成失败。请求超时改为「静默」上限后，已经收到的完整答案必须被采用。
#[test]
fn a_complete_answer_survives_a_stream_that_never_finishes() {
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::AnswerThenSilence);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unfinished-stream.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings_with_timeout(&endpoint, 1), BTreeMap::new())
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
            "a salvaged answer must not be requested a second time"
        );
        assert_eq!(row_count(&services), 1);
        assert_translated_segments(&blocks, &endpoint.requests(), "译：");
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
            let conn = open_fixture_conn(services.database_path());
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
            let conn = open_fixture_conn(services.database_path());
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
        let conn = open_fixture_conn(&db_path);
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

/// The retry in [`background_job_snapshots`] is only worth having if a real
/// lock conflict is recognised, so this pins the error shape SQLite produces
/// for a second writer: a rusqlite change must not silently turn the retry into
/// dead code that lets the poller fail again.
#[test]
fn a_second_writer_reports_the_lock_conflict_the_poller_retries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("locked.db");
    let first = rusqlite::Connection::open(&path).unwrap();
    first.pragma_update(None, "journal_mode", "WAL").unwrap();
    first
        .execute_batch("CREATE TABLE probe (id TEXT); BEGIN IMMEDIATE;")
        .unwrap();

    let second = rusqlite::Connection::open(&path).unwrap();
    second.busy_timeout(Duration::from_millis(0)).unwrap();
    let error = anyhow::Error::new(second.execute_batch("BEGIN IMMEDIATE;").unwrap_err());
    assert!(is_database_busy(&error), "unexpected lock error: {error:#}");

    first.execute_batch("ROLLBACK;").unwrap();
}

fn write_two_chapter_epub(path: &Path, first_body: &str, second_body: &str) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let chapter = |title: &str, body: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title></head><body>{body}</body></html>"#
        )
    };
    for (name, content) in [
        ("mimetype", "application/epub+zip".to_string()),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_string(),
        ),
        (
            "EPUB/package.opf",
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="book-id">urn:uuid:translation-incremental-fixture</dc:identifier><dc:title>Translation Incremental Fixture</dc:title><dc:creator>Fixture</dc:creator><dc:language>en</dc:language><meta property="dcterms:modified">2026-09-11T00:00:00Z</meta></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/><item id="chapter2" href="chapter2.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter1"/><itemref idref="chapter2"/></spine></package>"#.to_string(),
        ),
        (
            "EPUB/nav.xhtml",
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="chapter1.xhtml">First</a></li><li><a href="chapter2.xhtml">Second</a></li></ol></nav></body></html>"#.to_string(),
        ),
        (
            "EPUB/chapter1.xhtml",
            chapter("First", first_body),
        ),
        (
            "EPUB/chapter2.xhtml",
            chapter("Second", second_body),
        ),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
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
