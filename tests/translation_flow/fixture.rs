//! Import, translation and assertion helpers shared by the flow tests.

use super::*;

/// Reads one complete request. Everything a stray, abandoned or malformed
/// connection can cause comes back as an error so the fixture drops that one
/// connection and keeps serving instead of dying on the first hiccup.
pub(crate) fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
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

pub(crate) fn settings(endpoint: &TranslationEndpoint) -> ProviderSettings {
    settings_with_timeout(endpoint, 10)
}

/// 请求超时是「静默」上限，不再是整段调用的总时长：夹具需要它足够短才能在不拖慢
/// 测试的前提下触发空闲结束。
pub(crate) fn settings_with_timeout(
    endpoint: &TranslationEndpoint,
    request_timeout_secs: u64,
) -> ProviderSettings {
    ProviderSettings {
        base_url: endpoint.url.clone(),
        // 问答模型与翻译模型取两个不同的名字：翻译请求必须带翻译模型，所以夹具里
        // 的模型断言能发现「翻译误用问答模型」。
        chat_model: "same-chat-model".into(),
        translation_model: "same-translation-model".into(),
        default_language: Some("zh-Hans".into()),
        auto_run_background_jobs: false,
        background_job_interval_ms: 0,
        request_timeout_secs,
        ..ProviderSettings::default()
    }
}

pub(crate) async fn import(services: &AppServices, path: &Path) -> (String, String) {
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

/// 一个「模型答了、但形状不对」的文本块在块级重发预算内会发出的整块请求次数。
///
/// 每轮两次严格尝试（`TRANSLATION_RESPONSE_ATTEMPTS`），共
/// `TRANSLATION_BLOCK_RETRIES + 1` 轮，所以 = 2 × 6 = 12；最后一轮还会为多片段文本块
/// 追加逐片段回退的请求（每次只问一个文字叶）。这两个常量都只在 lib 内部可见，这里按
/// 同一份规则写下推导，改动预算时必须同时改这里 —— 用例断言的是「模型答垮一整块要花
/// 多少请求」，正是这个数字把「答了但形状不对」和「根本没答」分开。
pub(crate) const EXHAUSTED_BLOCK_ATTEMPTS: usize = 12;

/// How long a run may stop making progress before the fixture gives up on it.
/// Every case in this file runs in parallel, so a loaded machine can make an
/// honest run take several times longer than an idle one; what must never be
/// tolerated is a job that stops advancing, which is exactly what this budget
/// watches. A real hang therefore fails in 30 s while a slow but progressing
/// run is allowed to finish.
pub(crate) const TRANSLATION_STALL: Duration = Duration::from_secs(30);
/// Absolute ceiling for one run, so a job that keeps advancing forever still
/// ends the test instead of blocking the suite.
pub(crate) const TRANSLATION_WAIT: Duration = Duration::from_secs(240);

pub(crate) async fn wait_translation(
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
pub(crate) async fn background_job_snapshots(
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
pub(crate) fn is_database_busy(error: &anyhow::Error) -> bool {
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
pub(crate) fn open_fixture_conn(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

pub(crate) async fn translate(services: &AppServices, book_id: &str) -> BackgroundJobSnapshot {
    let job = wait_translation(services, book_id, BackgroundJobStatus::Paused).await;
    assert!(
        services
            .control_background_job(job.id, BackgroundJobAction::Resume)
            .await
            .unwrap()
    );
    wait_translation(services, book_id, BackgroundJobStatus::Succeeded).await
}

pub(crate) fn assert_translated_segments(
    blocks: &[TranslatedBlock],
    requests: &[Value],
    label: &str,
) {
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

pub(crate) fn row_count(services: &AppServices) -> usize {
    open_fixture_conn(services.database_path())
        .query_row("SELECT count(*) FROM translations", [], |row| row.get(0))
        .unwrap()
}

/// 主路径就是两次请求：冻结输入的严格尝试 + 一次协议纠正。
///
/// 这条断言只适用于「纠正之后就通过」的用例。块级重发要等这两次都失败才开始，所以一个
/// 答垮的块会走到 `EXHAUSTED_BLOCK_ATTEMPTS`（12 次）—— 用满预算的用例不能用这个 helper。
pub(crate) fn assert_one_correction(endpoint: &TranslationEndpoint) {
    let requests = endpoint.requests();
    assert_eq!(
        requests.len(),
        2,
        "one correction is the complete main-path budget"
    );
    assert_corrected_main_path(endpoint);
}

/// 「冻结输入 + 一次纠正」这两次请求的全部约束，与总请求数无关：逐片段回退只允许在
/// 这两步**之后**再发请求（见 `docs/agent/code-map-ai.md`），但主路径这两次本身必须
/// 逐字一致。
pub(crate) fn assert_corrected_main_path(endpoint: &TranslationEndpoint) {
    let requests = endpoint.requests();
    assert!(
        requests.len() >= 2,
        "the main path must ask twice before anything else: {requests:?}"
    );
    assert_eq!(
        requests[0], requests[1],
        "correction must preserve the source and all segment IDs"
    );
    let full = endpoint.full_requests();
    assert!(full.len() >= 2, "both main-path bodies must be captured");
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

/// Segments asked for by each captured request, in arrival order. A fragment request
/// carries exactly one leaf, so the shape alone tells the main path and the
/// fallback apart.
pub(crate) fn request_segment_counts(requests: &[Value]) -> Vec<usize> {
    requests
        .iter()
        .map(|request| request["segments"].as_array().unwrap().len())
        .collect()
}

pub(crate) fn write_two_chapter_epub(path: &Path, first_body: &str, second_body: &str) {
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

pub(crate) fn write_epub(path: &Path, body: &str) {
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
