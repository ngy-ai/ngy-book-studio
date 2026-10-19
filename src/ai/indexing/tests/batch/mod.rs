//! 「每请求文本块数」接进 walk 的用例：按批成包、逐块归位、坏块回退单块、坏批拆半
//! 与重试边界。
//!
//! 批**协议**的形状（请求信封、打包预算、逐块解析、信封级与块级错误的分类）由
//! `ai::translation::tests::batch_test` 固定；这里固定的是**调用方**：一次请求带走几块、
//! 批里的每一块怎么落回它自己的块号、哪一块退回单块请求、信封坏或整批被截断时怎么拆半、
//! 以及哪些块根本不许进批。

use super::*;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use futures_util::{future::BoxFuture, stream};

use crate::{
    ai::translation::TranslationSource,
    ai::{
        ChatEventStream, ChatRequest, ChatStreamEvent, ContextWindowExceeded, EmbeddingBatch,
        EmbeddingRequest, MessageContent, ModelInfo,
    },
    db,
};

/// 一个答得出**批**与**单块**两种请求形状的 provider。
///
/// 两支的形状由请求体自己区分：有 `blocks` 数组的是批请求，只有 `source`/`segments` 的是
/// 单块请求（坏块回退、以及「每请求文本块数」为 1 时的正常路径）。每块都用「原片段文字 +
/// `#译<片段号>`」作答，所以落库的译文自带它的来源，测试可以断言「哪一块进了哪一发」。
#[derive(Default)]
pub(crate) struct MockBatchProvider {
    /// 每一发请求收到的（章标题，块数，是不是批形状），按发起顺序。
    pub(crate) batches: Mutex<Vec<(String, usize, bool)>>,
    /// 每一发请求收到的块来源文字，按发起顺序。
    pub(crate) sent: Mutex<Vec<Vec<String>>>,
    /// 前 N 发请求回一个信封不合格的答案（块数与输入不符）：整批必须拆半重发。
    pub(crate) reject_envelopes: AtomicUsize,
    /// 前 N 发请求在输出上限处截断：整批同样必须拆半重发。
    pub(crate) truncate_requests: AtomicUsize,
    /// 前 N 发请求传输层直接失败：这一批的块按请求级失败处理，不拆半。
    pub(crate) fail_requests: AtomicUsize,
    /// 前 N 发**批形状**请求被端点以「超出上下文长度」直接拒绝（收流之前的 HTTP 400
    /// 封套）：这是容量问题，整批必须拆半重发，而不是按请求级失败整批跳过。
    pub(crate) reject_context: AtomicUsize,
    /// 这些来源文字**在批里**给出的答案不合格（整段合并成一条）：只有那一块要退回单块。
    pub(crate) merged_in_batches: Mutex<Vec<String>>,
    /// 设置后，每个请求开始时读一次持久游标并记下窗口。
    ///
    /// 读数放在请求**开始**：walk 先把这一批的块写进游标（`open_inflight` + persist）才会
    /// 把请求交给 `FuturesUnordered`，所以在请求被 poll 到的那一刻，游标里已经有这一批的
    /// 全部块号 —— 断言因此与几个请求先后被 poll 无关。
    pub(crate) probe: std::sync::OnceLock<WindowProbe>,
}

impl MockBatchProvider {
    pub(crate) fn attach_probe(&self, db_path: PathBuf, job_id: String) {
        self.probe
            .set(WindowProbe {
                db_path,
                job_id,
                windows: Mutex::new(Vec::new()),
            })
            .unwrap_or_else(|_| panic!("窗口探针只能装一次"));
    }

    /// 每个请求开始时读到的持久窗口（`next_ordinal` 与 `inflight`）。
    pub(crate) fn windows(&self) -> Vec<(usize, Vec<usize>)> {
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

    /// 每一发请求装了几块，按发起顺序。
    pub(crate) fn batch_sizes(&self) -> Vec<usize> {
        self.batches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(_, blocks, _)| *blocks)
            .collect()
    }

    /// 每一发请求带上了哪些块的来源文字，按发起顺序。
    pub(crate) fn sent_sources(&self) -> Vec<Vec<String>> {
        self.sent
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl OpenAiCompatibleProvider for MockBatchProvider {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>>> {
        async { Ok(Vec::new()) }.boxed()
    }

    fn chat_stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatEventStream>> {
        let probe = self.probe.get();
        async move {
            // 请求一开始就读一次持久游标：这一批的块在请求发出前就已经进了窗口。
            if let Some(probe) = probe {
                probe.record();
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
            // 每一发先在案：请求体已经构造好，测试要靠它数「发了多少发、各带几块」——
            // 传输层失败的那一发与在输出上限处截断的那一发都在里面。
            let sources = match value["blocks"].as_array() {
                Some(blocks) => blocks
                    .iter()
                    .map(|block| block["source"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>(),
                None => vec![value["source"].as_str().unwrap_or_default().to_string()],
            };
            self.batches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((
                    value["chapter"].as_str().unwrap_or_default().to_string(),
                    sources.len(),
                    value["blocks"].is_array(),
                ));
            self.sent
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(sources);
            if self
                .fail_requests
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                // 传输层失败：重发同一个请求解决不了，整批按请求级失败处理。
                anyhow::bail!("mock batch provider is temporarily unavailable");
            }
            if value["blocks"].is_array()
                && self
                    .reject_context
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        left.checked_sub(1)
                    })
                    .is_ok()
            {
                // 收流前被拒：与 `ai::http` 解析 HTTP 400 的 context-length 封套同一种
                // 错误类型。批太大，不是端点坏了 —— 更小的批才装得下。
                return Err(anyhow::Error::new(ContextWindowExceeded {
                    prompt_tokens: Some(4096),
                    context_tokens: Some(4096),
                }));
            }
            if self
                .truncate_requests
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                // 现场（2026-09-15，block 1762）：在输出上限处停下，正文里连一个完整容器
                // 都没有。批里出现它意味着这一批没写完，只有更小的批才装得下。
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ChatStreamEvent {
                        content_delta: Some(
                            "{\"blocks\":[{\"id\":0,\"translations\":[{\"id\":0,\"text\":\"译文"
                                .to_string(),
                        ),
                        ..Default::default()
                    }),
                    Ok(ChatStreamEvent {
                        finish_reason: Some("length".to_string()),
                        ..Default::default()
                    }),
                ])) as ChatEventStream);
            }
            let reply = match value["blocks"].as_array() {
                Some(blocks) => {
                    if self
                        .reject_envelopes
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                            left.checked_sub(1)
                        })
                        .is_ok()
                    {
                        // 信封不合格：块数与输入不符。这是**整批**的问题，拆半才有机会。
                        return Ok(single_event("{\"blocks\":[]}"));
                    }
                    let merged = self
                        .merged_in_batches
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    let mut answered = Vec::with_capacity(blocks.len());
                    for (index, block) in blocks.iter().enumerate() {
                        let segments = block["segments"]
                            .as_array()
                            .context("mock batch block had no segments")?;
                        let source = block["source"].as_str().unwrap_or_default().to_string();
                        // 整段合并成一条：这一块的片段数量不符，只有它该被拒（块级错误）。
                        let merge_into_one = merged.iter().any(|candidate| *candidate == source);
                        answered.push(serde_json::json!({
                            "id": index,
                            "translations": segment_replies(segments, merge_into_one),
                        }));
                    }
                    serde_json::json!({ "blocks": answered }).to_string()
                }
                None => {
                    // 单块请求（坏块回退，或「每请求文本块数」为 1 的正常路径）。
                    let segments = value["segments"]
                        .as_array()
                        .context("mock translation input had no segments")?;
                    serde_json::json!({ "translations": segment_replies(segments, false) })
                        .to_string()
                }
            };
            Ok(single_event(&reply))
        }
        .boxed()
    }

    fn embeddings(&self, _request: EmbeddingRequest) -> BoxFuture<'_, Result<EmbeddingBatch>> {
        async { anyhow::bail!("mock batch provider does not embed") }.boxed()
    }
}

fn single_event(content: &str) -> ChatEventStream {
    Box::pin(stream::iter(vec![Ok(ChatStreamEvent {
        content_delta: Some(content.to_string()),
        done: true,
        ..Default::default()
    })])) as ChatEventStream
}

/// 模型对一组片段的答案：每个片段都用「原片段文字 + `#译<片段号>`」回答。
///
/// `merge_into_one` 是整个块合并成一条（`segment_count_mismatch`）——弱模型在多片段块上
/// 最典型的坏形状，用来固定「块自己的错误只拒绝那一块」。
fn segment_replies(segments: &[serde_json::Value], merge_into_one: bool) -> serde_json::Value {
    if merge_into_one {
        return serde_json::json!([{ "id": 0, "text": "整段合并的译文" }]);
    }
    serde_json::Value::Array(
        segments
            .iter()
            .enumerate()
            .map(|(id, segment)| {
                let text = segment["text"].as_str().unwrap_or_default().trim();
                serde_json::json!({ "id": id, "text": format!("{text}#译{id}") })
            })
            .collect(),
    )
}

/// 建好一个处于运行态的翻译任务，并显式给出「每请求文本块数」。
///
/// 配好 provider 的 `IndexingInner` 要跟任务一起交出去：`models.translation` 只写在配置
/// 它的那一份上，而 `Fixture::indexing_inner()` 每次调用都新建一份。
fn batch_case_in(
    fixture: Fixture,
    provider: &Arc<MockBatchProvider>,
    batch_blocks: usize,
) -> (
    Fixture,
    Arc<IndexingInner>,
    db::index_jobs::IndexJob,
    JobCursor,
    TranslationServices,
) {
    let identity = "translation-v2:chat-test";
    let inner = fixture.indexing_inner();
    let coordinator = IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            identity.to_string(),
            TranslationGenerationSettings::default(),
            batch_blocks,
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
    let translation = inner.models_snapshot().translation.unwrap();
    (fixture, inner, job, cursor, translation)
}

/// 跑一次整本翻译，返回值与收尾读数。
fn run_batch_walk(
    fixture: &Fixture,
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    cursor: &mut JobCursor,
    translation: &TranslationServices,
    concurrency: usize,
) -> (RunOutcome, TranslationRunStats) {
    IndexingCoordinator {
        inner: Arc::clone(inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(concurrency, Duration::ZERO);
    let mut stats = TranslationRunStats::default();
    let outcome = fixture
        .runtime
        .block_on(run_translation_blocks_inner(
            inner,
            job,
            translation,
            "zh-Hans",
            cursor,
            &mut stats,
        ))
        .expect("批运行不该失败");
    (outcome, stats)
}

/// 这次运行抽出来的文本块：必须走工作线程那条加载路径。
///
/// EPUB 来源按阅读章节（spine）解析、其它来源回落到规范 HTML，两条路径对同一本书会给出
/// 不同的块列表（EPUB 路径把章标题也算成一块）。测试助手从这里取块，断言才不会错位。
fn blocks_of(fixture: &Fixture, inner: &Arc<IndexingInner>) -> Vec<TranslationBlock> {
    let revision = fixture.revision;
    fixture
        .runtime
        .block_on(load_translation_blocks(
            inner,
            &fixture.book_id,
            &fixture.source_id,
            revision,
        ))
        .unwrap()
}

/// 落库的译文：块号 → 译文。
fn stored_translations(fixture: &Fixture) -> BTreeMap<usize, StoredTranslation> {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::translations::list_for_source(&conn, &fixture.book_id, "zh-Hans", fixture.revision)
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.ordinal as usize,
                serde_json::from_str::<StoredTranslation>(&row.translated_text).unwrap(),
            )
        })
        .collect()
}

/// 第 `ordinal` 块落库的译文必须来自它**自己**那一块：逐片段对上「原片段文字 + `#译<号>`」。
fn assert_block_stored(fixture: &Fixture, ordinal: usize, source: &TranslationSource) {
    let stored = stored_translations(fixture);
    let stored = stored
        .get(&ordinal)
        .unwrap_or_else(|| panic!("第 {ordinal} 块没有落库：{:?}", stored.keys()));
    assert_eq!(
        stored
            .segments
            .iter()
            .map(|segment| segment.translated.trim().to_string())
            .collect::<Vec<_>>(),
        source
            .segments
            .iter()
            .enumerate()
            .map(|(id, segment)| format!("{}#译{id}", segment.trim()))
            .collect::<Vec<_>>(),
        "第 {ordinal} 块落库的译文必须来自它自己那一块"
    );
}

/// 三个段落的正文：同一章里每段一个文本块、每块只有一个片段。
///
/// 块列表由 `blocks_of` 那条加载路径给出，EPUB 来源里它前面还有这章的章标题。
fn three_paragraphs(fixture: &mut Fixture) {
    fixture.rewrite_first_unit("<p>alpha</p><p>beta</p><p>gamma</p>");
}

/// 一批带走整窗的块，而且每一块都落回它自己的块号。
///
/// 这是打包的意义所在：`任务并发` 只决定同时打开几个**请求**，一批里的块共用一发请求，
/// 但落库、游标与缓存仍按块 —— 所以逐块的译文必须各自正确，游标也必须停在书末。
///
/// 块列表按 EPUB 阅读章节解析，所以三个段落加上章标题一共四块。
mod skip_test;

mod splitting_test;

mod window_test;
