//! The reply modes, endpoint statistics and the in-process fixture endpoint.

use super::*;

pub(crate) const FORMATTED_BODY: &str = r#"<h1>Formatting <em>matters</em></h1>
<p>Start <strong>bold</strong> and <em>italic</em>.<br/>Next <span style="color: #b24a00">colored</span> and <code>do_not_translate()</code> end.</p>
<ul><li>First <strong>item</strong></li><li>Second item</li></ul>
<table><thead><tr><th>Key</th><th>Value</th></tr></thead><tbody><tr><td>Alpha</td><td><em>Beta</em></td></tr></tbody></table>
<p>Read <a href="https://example.invalid">this</a> now</p>
<p><span>adjacent</span><span>parts</span></p>
<pre><code>print("leave this code unchanged")</code></pre>"#;

pub(crate) const INVALID_RESPONSE_SENTINEL: &str =
    "PRIVATE_PROVIDER_RESPONSE_MUST_NOT_BECOME_JOB_ERROR";

/// A source block carrying this marker always receives an invalid response, so
/// the per-block skip path can be exercised without a real model.
pub(crate) const POISON_SENTINEL: &str = "POISON_BLOCK";

#[derive(Clone, Copy, Debug)]
pub(crate) enum ReplyMode {
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
pub(crate) static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// How long the fixture waits for one request before it treats the connection
/// as stray. It has to stay clearly above the clients' own timeouts: a fixture
/// that gives up first turns a slow machine into what looks like a product
/// hang, and the real failure then has to be inferred with no evidence.
pub(crate) const FIXTURE_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one fixture may take to see a request or to finish writing an
/// answer. Every case in this file runs in parallel, so a loaded machine can
/// need several times the idle time for the same run; the budget only has to
/// outlast that, while a request that truly never arrives still fails and now
/// prints the fixture's own timeline as evidence.
pub(crate) const FIXTURE_WAIT: Duration = Duration::from_secs(30);

pub(crate) fn elapsed_ms() -> u128 {
    PROCESS_START.elapsed().as_millis()
}

/// Observable state of one mock endpoint, registered process-wide so a failure
/// can tell "the fixture never received that request" apart from "the fixture
/// answered and the client never used the answer" without threading the fixture
/// through every helper that only takes the services handle.
pub(crate) struct EndpointStats {
    pub(crate) label: &'static str,
    pub(crate) accepted: AtomicUsize,
    pub(crate) requests: AtomicUsize,
    pub(crate) responses: AtomicUsize,
    pub(crate) events: Mutex<Vec<String>>,
}

impl EndpointStats {
    pub(super) fn new(label: &'static str) -> Self {
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
    pub(super) fn note(&self, event: impl Into<String>) {
        let mut events = self.events.lock().unwrap();
        if events.len() < 512 {
            events.push(format!("+{}ms {}", elapsed_ms(), event.into()));
        }
    }
}

pub(crate) static ENDPOINTS: LazyLock<Mutex<Vec<Arc<EndpointStats>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Every fixture's timeline in this test process, appended to job failures so a
/// dead or starved fixture is never mistaken for a hung product. Other cases in
/// the same binary are included on purpose: they show the machine-wide picture
/// at the moment of the failure.
pub(crate) fn fixture_timeline() -> String {
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
pub(crate) struct TranslationEndpoint {
    pub(crate) url: String,
    pub(crate) requests: Arc<Mutex<Vec<Value>>>,
    pub(crate) full_requests: Arc<Mutex<Vec<Value>>>,
    pub(crate) correction_released: Arc<AtomicBool>,
    pub(crate) responses_finished: Arc<AtomicUsize>,
    pub(crate) stopped: Arc<AtomicBool>,
    pub(crate) worker: Option<thread::JoinHandle<()>>,
}

impl TranslationEndpoint {
    pub(super) fn start(label: &'static str, mode: ReplyMode) -> Self {
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
                assert_eq!(body["model"], "same-translation-model");
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

    pub(super) fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    pub(super) fn full_requests(&self) -> Vec<Value> {
        self.full_requests.lock().unwrap().clone()
    }

    pub(super) fn release_correction(&self) {
        self.correction_released.store(true, Ordering::Release);
    }

    pub(super) async fn wait_for_requests(&self, count: usize) {
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

    pub(super) async fn wait_for_responses(&self, count: usize) {
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
