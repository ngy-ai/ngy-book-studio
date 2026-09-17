//! Fixtures shared by the indexing tests: one in-process library plus a mock
//! provider that counts and, when asked, blocks its calls.

use std::{io::Cursor, sync::atomic::AtomicUsize};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::{FutureExt as _, future::BoxFuture, stream};

use super::*;
use crate::{
    ai::translation::{TranslationContext, TranslationSource},
    ai::{
        ChatEventStream, ChatRequest, ChatStreamEvent, ContentPart, EmbeddingBatch,
        EmbeddingRequest, MessageContent, ModelInfo,
    },
    document::{DocumentLocator, SourceLocator},
    library::LibraryStore,
    runtime::IoRuntime,
    storage::LocalBlobStore,
};

mod embedding;
mod endpoints;
mod job_control;
mod scheduling;
mod translation;
mod translation_retry;
mod translation_walk;
mod unit;
mod vision;
mod vision_publication;

/// How long one test waits for the coordinator to reach a state or to make a
/// provider call. Every test in this module runs in parallel while other
/// test binaries may be loading the same machine, so a progress wait has to
/// outlast a busy one: a coordinator that really is stuck still fails, only
/// later and with the state it was stuck in. Several fixtures also hand the
/// provider a deliberate 5 second delay, so the old 5 second budget raced
/// the very work it was waiting for.
pub(super) const TEST_PROGRESS_WAIT: Duration = Duration::from_secs(30);

pub(super) fn test_model_config(
    embedding_model: &str,
    embedding_execution_identity: &str,
    vision_model: &str,
    vision_execution_identity: &str,
) -> IndexingModelConfig {
    IndexingModelConfig::new(
        embedding_model,
        crate::ai::MAX_EMBEDDING_DIMENSIONS,
        embedding_execution_identity,
        vision_model,
        vision_execution_identity,
    )
    .unwrap()
}

#[derive(Default)]
pub(super) struct MockProvider {
    pub(super) embedding_calls: AtomicUsize,
    pub(super) vision_png_calls: AtomicUsize,
    pub(super) fail_embeddings: AtomicBool,
    pub(super) block_embeddings: AtomicBool,
    pub(super) embedding_marker: AtomicUsize,
    pub(super) embedding_delay_ms: AtomicUsize,
    pub(super) vision_delay_ms: AtomicUsize,
}

impl OpenAiCompatibleProvider for MockProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        let vision_png_calls = &self.vision_png_calls;
        let vision_delay_ms = self.vision_delay_ms.load(Ordering::SeqCst);
        let model = request.model.clone();
        async move {
                let image_url = request
                    .messages
                    .first()
                    .and_then(|message| message.content.as_ref())
                    .and_then(|content| match content {
                        MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
                            ContentPart::ImageUrl { image_url } => Some(image_url.url.as_str()),
                            ContentPart::Text { .. } => None,
                        }),
                        MessageContent::Text(_) => None,
                    })
                    .context("mock vision request did not include an image")?;
                ensure!(
                    image_url.starts_with("data:image/png;base64,"),
                    "mock vision request did not use an inline data URL"
                );
                let encoded = image_url
                    .strip_prefix("data:image/png;base64,")
                    .context("mock vision request did not use PNG")?;
                let bytes = BASE64_STANDARD
                    .decode(encoded)
                    .context("mock vision request used invalid base64")?;
                ensure!(
                    bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                    "mock vision request did not contain PNG bytes"
                );
                vision_png_calls.fetch_add(1, Ordering::SeqCst);
                if vision_delay_ms != 0 {
                    tokio::time::sleep(Duration::from_millis(vision_delay_ms as u64)).await;
                }
                let events = vec![Ok(ChatStreamEvent {
                    content_delta: Some(serde_json::json!({
                        "regions": [
                            {"ocr": "", "description": format!("{model} 一页测试文档"), "region": null},
                            {"ocr": "页面文字", "description": "标题区域", "region": {"left": 20, "top": 30, "right": 500, "bottom": 200}},
                            {"ocr": "第二段", "description": "正文区域", "region": {"left": 20, "top": 250, "right": 900, "bottom": 900}}
                        ]
                    }).to_string()),
                    done: true,
                    ..Default::default()
                })];
                Ok(Box::pin(stream::iter(events)) as ChatEventStream)
            }
            .boxed()
    }

    fn embeddings(&self, request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        self.embedding_calls.fetch_add(1, Ordering::SeqCst);
        let delay_ms = self.embedding_delay_ms.load(Ordering::SeqCst);
        async move {
            while self.block_embeddings.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if delay_ms != 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;
            }
            if self.fail_embeddings.load(Ordering::SeqCst) {
                anyhow::bail!("mock embedding unavailable");
            }
            let marker = self.embedding_marker.load(Ordering::SeqCst).max(1) as f32;
            Ok(EmbeddingBatch {
                model: request.model,
                vectors: request
                    .input
                    .iter()
                    .enumerate()
                    .map(|(index, _)| vec![marker, index as f32 + 1.0, 0.5])
                    .collect(),
                usage: None,
            })
        }
        .boxed()
    }
}

pub(super) struct Fixture {
    pub(super) _temp: tempfile::TempDir,
    pub(super) runtime: IoRuntime,
    pub(super) db_path: PathBuf,
    pub(super) blobs: Arc<LocalBlobStore>,
    pub(super) book_id: String,
    pub(super) source_id: String,
    pub(super) revision: u64,
    pub(super) unit_id: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let runtime = IoRuntime::new(2).unwrap();
        let mut library =
            LibraryStore::load_from_with_runtime(temp.path().to_path_buf(), runtime.clone())
                .unwrap();
        // These tests exercise queue/run semantics, so new derivative jobs
        // must be created queued instead of relying on the product default.
        library
            .background_job_auto_run_flag()
            .store(true, Ordering::Release);
        let book = library.create_book("Index test", "Author").unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_conn(&db_path).unwrap();
        let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap();
        let unit = db::content_units::list_for_source(&conn, &source.id)
            .unwrap()
            .remove(0);
        let blobs = Arc::new(LocalBlobStore::new(temp.path().join("objects")).unwrap());
        Self {
            _temp: temp,
            runtime,
            db_path,
            blobs,
            book_id: book.id,
            source_id: source.id,
            revision: book.revision,
            unit_id: unit.id,
        }
    }

    fn coordinator(&self, provider: Arc<MockProvider>) -> Arc<IndexingCoordinator> {
        self.coordinator_with_providers(provider.clone(), provider)
    }

    /// Creates one more book in the same data directory so the queue holds
    /// two canonical embedding jobs for scheduling tests.
    fn add_second_book(&self) -> String {
        let data_dir = self
            .db_path
            .parent()
            .expect("fixture database lives in the temporary data directory")
            .to_path_buf();
        let mut library =
            LibraryStore::load_from_with_runtime(data_dir, self.runtime.clone()).unwrap();
        library
            .background_job_auto_run_flag()
            .store(true, Ordering::Release);
        let book = library.create_book("Second index test", "Author").unwrap();
        let conn = db::open_conn(&self.db_path).unwrap();
        db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap()
            .id
    }

    fn embedding_status_count(&self, status: db::index_jobs::IndexJobStatus) -> usize {
        let conn = db::open_conn(&self.db_path).unwrap();
        db::index_jobs::list_by_kind(&conn, "embedding")
            .unwrap()
            .into_iter()
            .filter(|job| job.status == status)
            .count()
    }

    fn wait_until(&self, description: &str, timeout: Duration, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + timeout;
        while !predicate(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn coordinator_with_providers(
        &self,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        vision_provider: Arc<dyn OpenAiCompatibleProvider>,
    ) -> Arc<IndexingCoordinator> {
        let blobs: Arc<dyn BlobStore> = self.blobs.clone();
        IndexingCoordinator::start(
            self.runtime.handle(),
            &self.db_path,
            blobs,
            embedding_provider,
            vision_provider,
            test_model_config(
                "embed-test",
                "embedding-endpoint-a:embed-test",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        )
        .unwrap()
    }

    fn indexing_inner(&self) -> Arc<IndexingInner> {
        let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
        Arc::new(IndexingInner {
            runtime: self.runtime.handle(),
            db_path: self.db_path.clone(),
            blobs: self.blobs.clone(),
            models: RwLock::new(ModelServices {
                embedding_provider: provider.clone(),
                vision_provider: provider,
                config: test_model_config(
                    "embed-test",
                    "embedding-endpoint-a:embed-test",
                    "vision-test",
                    "vision-endpoint-a:vision-test",
                ),
                translation: None,
            }),
            scheduling: RwLock::new(JobScheduling::default()),
            transitions: AsyncMutex::new(()),
            wake: Notify::new(),
            reconcile_translations: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        })
    }

    fn wait_for_post_vision_embedding(
        &self,
        coordinator: &IndexingCoordinator,
        embedding_identity: &str,
        vision_identity: &str,
    ) {
        // Vision queues a separate embedding phase before it succeeds;
        // the canonical text embedding job can already be complete.
        let jobs = self
            .runtime
            .block_on(coordinator.jobs_for_book(&self.book_id))
            .unwrap();
        let post_vision = jobs
            .iter()
            .find(|job| {
                job.kind == "embedding"
                    && job.source_id.as_deref() == Some(self.source_id.as_str())
                    && serde_json::from_str::<JobCursor>(&job.cursor_json).is_ok_and(|cursor| {
                        cursor.execution_identity.as_deref() == Some(embedding_identity)
                            && cursor.input_execution_identity.as_deref() == Some(vision_identity)
                    })
            })
            .expect("successful vision must queue matching embedding work");
        self.runtime
            .block_on(coordinator.wait_for_state(
                &post_vision.id,
                IndexingJobStatus::Succeeded,
                TEST_PROGRESS_WAIT,
            ))
            .unwrap();
    }

    fn publish_test_visual_page(&self) {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let bytes = bytes.into_inner();
        let key = self.runtime.block_on(self.blobs.put(&bytes)).unwrap();
        let blob = db::blobs::BlobRecord {
            object_key: key.to_string(),
            media_type: "image/png".to_string(),
            byte_len: bytes.len() as u64,
            hash: blake3::hash(&bytes).to_hex().to_string(),
            created_at: unix_timestamp().unwrap(),
        };
        let page = db::visual_pages::VisualPage {
            id: "singleton-test-page".to_string(),
            book_id: self.book_id.clone(),
            source_id: self.source_id.clone(),
            content_unit_id: Some(self.unit_id.clone()),
            page_index: 0,
            object_key: key.to_string(),
            width: 2,
            height: 3,
            render_scale: 1.0,
            renderer: "test-renderer".to_string(),
            renderer_version: "1".to_string(),
            document_revision: self.revision,
            unit_revision: self.revision,
            profile_id: "test-profile".to_string(),
            fidelity: "structural".to_string(),
            locator_json: serde_json::to_string(
                &DocumentLocator::unit(&self.book_id, &self.unit_id)
                    .with_source(SourceLocator::created()),
            )
            .unwrap(),
            created_at: unix_timestamp().unwrap(),
        };
        db::transactions::replace_visual_pages(
            &mut db::open_conn(&self.db_path).unwrap(),
            &self.book_id,
            &self.source_id,
            self.revision,
            &[blob],
            &[page],
        )
        .unwrap();
    }
}

/// Puts one job in the terminal state a finished run leaves: succeeded with a
/// durable cursor at `next_ordinal`. Used by the retry tests, which care about
/// where the cursor starts rather than about how a run gets there.
pub(super) fn finish_job_at(fixture: &Fixture, job_id: &str, next_ordinal: usize) {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job = db::index_jobs::get(&conn, job_id).unwrap().unwrap();
    let mut cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
    cursor.next_ordinal = next_ordinal;
    cursor.retry_ordinals.clear();
    db::index_jobs::update_state(
        &conn,
        job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &cursor.encode().unwrap(),
        None,
        unix_timestamp().unwrap(),
        None,
        Some(unix_timestamp().unwrap()),
    )
    .unwrap();
}

/// 现场行为（2026-09-13，qwen3.5:0.8b）：模型会翻译，但把整段译文合并进一条
/// id=0 的片段；只有单片段请求才会得到合规答案。`fail_fragments` 让它对单片段
/// 单个请求的可观察窗口：并发用例要在请求真的打开时读一次持久游标。
pub(super) struct WindowProbe {
    pub(super) db_path: PathBuf,
    pub(super) job_id: String,
    /// 每个请求开始时读到的 `(next_ordinal, inflight)`。
    pub(super) windows: Mutex<Vec<(usize, Vec<usize>)>>,
}

impl WindowProbe {
    fn record(&self) {
        let Ok(conn) = db::open_conn(&self.db_path) else {
            return;
        };
        let Ok(Some(job)) = db::index_jobs::get(&conn, &self.job_id) else {
            return;
        };
        let Ok(cursor) = serde_json::from_str::<serde_json::Value>(&job.cursor_json) else {
            return;
        };
        let next_ordinal = cursor["next_ordinal"].as_u64().unwrap_or(0) as usize;
        let inflight = cursor["inflight"]
            .as_array()
            .map(|ordinals| {
                ordinals
                    .iter()
                    .filter_map(|ordinal| ordinal.as_u64())
                    .map(|ordinal| ordinal as usize)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.windows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((next_ordinal, inflight));
    }
}

/// 请求也回一个不合格的答案。
#[derive(Default)]
pub(super) struct MockMergeProvider {
    pub(super) requests: AtomicUsize,
    pub(super) merged_requests: AtomicUsize,
    pub(super) fragment_requests: AtomicUsize,
    pub(super) fail_fragments: AtomicBool,
    /// 现场（2026-09-15，block 1762，qwen3.5:0.8b）：模型在输出上限处停下，
    /// `finish_reason="length"`，响应里连一个完整容器都没有。
    pub(super) truncated: AtomicBool,
    /// 现场（2026-09-15，block 1786/1787，qwen3.5:0.8b）：纠正提示把「合并成一条」
    /// 换成了「开始按段写、但 JSON 不闭合」。这个开关让多片段请求按 attempt 序号
    /// 交替给出这两种形状，用于固定「整轮失败类别里只要有一次可回退，就要回退」。
    pub(super) correction_breaks_json: AtomicBool,
    /// 每个请求的模拟耗时。0 表示立刻返回，其余用例的行为不变；并发用例靠它把
    /// 请求真正叠在一起，再用 `peak` 读出窗口到底开了多大。
    pub(super) delay_ms: AtomicU64,
    /// 同时打开的请求数与其峰值。
    pub(super) active: AtomicUsize,
    pub(super) peak: AtomicUsize,
    /// 设置后，每个请求开始时读一次持久游标并记下窗口。
    pub(super) probe: std::sync::OnceLock<WindowProbe>,
    /// 前 N 次请求一律回一个不合协议的答案（`segment_count_mismatch`），用于固定
    /// 块级重试：模型先答错、重发时答对。
    pub(super) reject_requests: AtomicUsize,
    /// provider 直接失败（HTTP/连接类，不是协议失败），用于固定「非协议失败不重试」。
    pub(super) fail_provider: AtomicBool,
}

impl MockMergeProvider {
    fn attach_probe(&self, db_path: PathBuf, job_id: String) {
        self.probe
            .set(WindowProbe {
                db_path,
                job_id,
                windows: Mutex::new(Vec::new()),
            })
            .unwrap_or_else(|_| panic!("窗口探针只能装一次"));
    }

    fn windows(&self) -> Vec<(usize, Vec<usize>)> {
        self.probe
            .get()
            .map(|probe| {
                probe
                    .windows
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
            })
            .unwrap_or_default()
    }
}

impl OpenAiCompatibleProvider for MockMergeProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        let requests = &self.requests;
        let merged_requests = &self.merged_requests;
        let fragment_requests = &self.fragment_requests;
        let fail_fragments = self.fail_fragments.load(Ordering::SeqCst);
        let truncated = self.truncated.load(Ordering::SeqCst);
        let delay_ms = self.delay_ms.load(Ordering::SeqCst);
        let active = &self.active;
        let peak = &self.peak;
        let probe = self.probe.get();
        async move {
                requests.fetch_add(1, Ordering::SeqCst);
                if self.fail_provider.load(Ordering::SeqCst) {
                    // provider 级别的失败：重发同一个请求解决不了，整次运行必须失败。
                    anyhow::bail!("mock provider is unavailable");
                }
                if delay_ms > 0 {
                    // 窗口的读数必须在请求「打开」的时候取：这里记下同时打开的请求数，
                    // 也顺手读一遍持久游标，证明窗口真的写进了库里。
                    let open = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(open, Ordering::SeqCst);
                    if let Some(probe) = probe {
                        probe.record();
                    }
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                }
                // 前 N 次请求一律回一个不合协议的答案（一条片段都没有），用于固定块级
                // 重试：模型先答错、重发时答对。
                let rejected = self
                    .reject_requests
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                    .is_ok();
                if rejected {
                    return Ok(Box::pin(stream::iter(vec![Ok(ChatStreamEvent {
                        content_delta: Some("{\"translations\":[]}".to_string()),
                        done: true,
                        ..Default::default()
                    })])) as ChatEventStream);
                }
                if truncated {
                    // 一个没写完的字符串值之后直接撞上输出上限：正文里没有任何完整容器，
                    // 唯一的 `"id"` 也还在那个没闭合的字符串里。
                    return Ok(Box::pin(stream::iter(vec![
                        Ok(ChatStreamEvent {
                            content_delta: Some(
                                "{\"translations\":[{\"id\":0,\"text\":\"译文".to_string(),
                            ),
                            ..Default::default()
                        }),
                        Ok(ChatStreamEvent {
                            finish_reason: Some("length".to_string()),
                            ..Default::default()
                        }),
                    ])) as ChatEventStream);
                }
                let input = request
                    .messages
                    .iter()
                    .rev()
                    .find_map(|message| match message.content.as_ref() {
                        Some(MessageContent::Text(text)) => Some(text.clone()),
                        _ => None,
                    })
                    .context("mock translation request had no text input")?;
                let value: serde_json::Value =
                    serde_json::from_str(&input).context("mock translation input was not JSON")?;
                let segments = value["segments"]
                    .as_array()
                    .context("mock translation input had no segments")?;
                let reply = if segments.len() == 1 {
                    fragment_requests.fetch_add(1, Ordering::SeqCst);
                    if fail_fragments {
                        // 单片段请求都没有通过协议：整个文本块必须保持未翻译。
                        "{\"translations\":[]}".to_string()
                    } else {
                        assert_eq!(
                            value["source"], segments[0]["text"],
                            "片段请求只能带上该片段本身，否则弱模型会把整段译文当成这一段的答案"
                        );
                        format!(
                            "{{\"translations\":[{{\"id\":0,\"text\":\"译文{}\"}}]}}",
                            segments[0]["text"]
                                .as_str()
                                .unwrap_or_default()
                                .chars()
                                .count()
                        )
                    }
                } else {
                    merged_requests.fetch_add(1, Ordering::SeqCst);
                    // 第一轮严格尝试：整段合并进 id=0（`segment_count_mismatch`）。
                    // 纠正尝试：模型确实开始按段写了，但 JSON 在中途断掉，于是最后一次
                    // 尝试的固定分类变成 `incomplete_json`。两次描述的是同一个能力上限，
                    // 回退判定必须看整轮而不是只看最后一次。
                    if self.correction_breaks_json.load(Ordering::SeqCst)
                        && merged_requests.load(Ordering::SeqCst) > 1
                    {
                        "{\"translations\":[{\"id\":0,\"text\":\"第一段译文\"},{\"id\":1,\"text\":\"第二"
                            .to_string()
                    } else {
                        "{\"translations\":[{\"id\":0,\"text\":\"整段合并的译文\"}]}".to_string()
                    }
                };
                Ok(Box::pin(stream::iter(vec![Ok(ChatStreamEvent {
                    content_delta: Some(reply),
                    done: true,
                    ..Default::default()
                })])) as ChatEventStream)
            }
            .boxed()
    }

    fn embeddings(&self, _request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async { anyhow::bail!("mock merge provider does not embed") }.boxed()
    }
}

/// 建好一个处于运行态的翻译任务，让测试可以直接调用 `translate_block`。
pub(super) fn translation_block_fixture(
    provider: &Arc<MockMergeProvider>,
    identity: &str,
) -> (
    Fixture,
    db::index_jobs::IndexJob,
    JobCursor,
    TranslationServices,
) {
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
            identity.to_string(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let mut job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    let mut cursor = JobCursor::from_job(&job, "chat-test", Some(identity)).unwrap();
    cursor.next_ordinal = 0;
    let cursor_json = cursor.encode().unwrap();
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Running,
        &cursor_json,
        None,
        1,
        Some(1),
        None,
    )
    .unwrap();
    job.status = db::index_jobs::IndexJobStatus::Running;
    job.cursor_json = cursor_json;
    let translation = TranslationServices {
        provider: provider.clone(),
        model: "chat-test".to_string(),
        execution_identity: identity.to_string(),
        target_language: Some("zh-Hans".to_string()),
        auto_run: false,
    };
    (fixture, job, cursor, translation)
}

/// 一个把内联强调切开的段落：首段是短名字，其余是正文。
pub(super) fn author_bio_source() -> TranslationSource {
    TranslationSource {
        text: "Andrew Stellman is a developer and an author.".to_string(),
        segments: vec![
            "Andrew Stellman".to_string(),
            " is a developer and an author.".to_string(),
        ],
    }
}

/// 直接在单个文本块上跑一次 `translate_block`。
///
/// 作用域必须由一次运行给出：块请求靠运行最后持久化的游标 JSON 判定「这一行还是我的」，
/// 单独造一个 scope 会立刻被判成已被替换。这里用 fixture 里那份游标建运行，块号固定为
/// 0。
pub(super) fn translate_block_source(
    fixture: &Fixture,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
    translation: &TranslationServices,
    source: &TranslationSource,
) -> Result<BlockWalk> {
    let run = TranslationRun::new(cursor.clone())?;
    let scope = run.scope(0);
    // 这些用例关心的是协议与重试行为，语境不影响它们；带语境的请求体由
    // 文本块明细与请求构造的用例单独固定。
    fixture.runtime.block_on(translate_block(
        &fixture.indexing_inner(),
        job,
        &scope,
        translation,
        "zh-Hans",
        None,
        source,
        TranslationContext::default(),
    ))
}

pub(super) fn translate_author_bio_block(
    fixture: &Fixture,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
    translation: &TranslationServices,
) -> Result<BlockWalk> {
    translate_block_source(fixture, job, cursor, translation, &author_bio_source())
}

/// 只有一个片段的文本块：没有逐片段回退可走，块级重试是它唯一的补救手段。
pub(super) fn single_fragment_source() -> TranslationSource {
    TranslationSource {
        text: "Hello world".to_string(),
        segments: vec!["Hello world".to_string()],
    }
}

pub(super) fn recorded_events(
    fixture: &Fixture,
    job_id: &str,
) -> Vec<crate::ai::job_diagnostics::BackgroundJobLogEntry> {
    crate::ai::job_diagnostics::JobDiagnosticStore::for_database(&fixture.db_path)
        .unwrap()
        .read(job_id)
        .unwrap()
        .entries
}
