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
#[test]
pub(crate) fn a_batch_request_carries_the_whole_window_and_every_block_keeps_its_own_row() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "三个段落 + 章标题");

    let (outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4],
        "整窗的块必须装进同一发请求，而不是分成两发"
    );
    assert_eq!(
        provider.sent_sources()[0],
        blocks
            .iter()
            .map(|block| block.source.text.clone())
            .collect::<Vec<_>>(),
        "批里的块顺序与冻结输入一致"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4);
    assert_eq!(
        (stats.batched_requests, stats.batched_blocks),
        (1, 4),
        "收尾汇总要能看出省了几发请求"
    );
    let shapes = provider
        .batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert!(shapes[0].2, "整窗的块走的是批信封，不是四发单块请求");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("整本翻完必须按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, blocks.len());
    assert!(finished.inflight.is_empty());
}

/// 一发批请求里的每一块都先进持久游标：任务窗口的「处理中」就是这一批的块号集合。
///
/// 窗口是游标的一部分，必须在请求发出前写好（`walk.rs` 的窗口段），否则任务窗口会显示
/// 「处理中」为空、而模型其实正在翻这一批。
#[test]
pub(crate) fn a_batch_publishes_every_block_of_its_window_as_inflight() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.attach_probe(fixture.db_path.clone(), job.id.clone());

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);
    assert_eq!(stats.saved, 4);

    let windows = provider.windows();
    assert_eq!(windows.len(), 1, "四块装一发：只有一个请求窗口");
    let (next_ordinal, inflight) = &windows[0];
    assert_eq!(*next_ordinal, blocks[0].ordinal);
    assert_eq!(
        inflight,
        &blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
        "这一发带走几块，游标里就有几块在飞 —— 「处理中」由此而来"
    );
}

/// 两个并发槽位、每发最多两块：谁都不许多带一块，也不许多开一发 —— 于是「处理中」的
/// 上界就是「任务并发 × 每请求文本块数」。
///
/// 请求形状与持久窗口各查一半：`batch_sizes` 说明两发各带满两块；探针在请求**开始时**读
/// 持久游标，所以读数就是那一发发出时「处理中」的实际块集合 —— 第一发可能在第二发还没攒
/// 完时就被轮询（攒批要拿门闩，等锁期间窗口照常被驱动，见 `walk.rs` 的
/// `lock_transitions`），因此读数只承诺两件事：永远是块列表的一段前缀（发布与提交都按
/// 序号），而且填到过 4 块 ——「任务并发 × 每请求文本块数」的上界既没有被越过，也不是靠
/// 少发请求凑出来的。
#[test]
pub(crate) fn a_batch_window_never_exceeds_concurrency_times_the_block_limit() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 2);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.attach_probe(fixture.db_path.clone(), job.id.clone());
    let ordinals = blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>();

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 2);

    assert_eq!(stats.saved, 4);
    assert_eq!(
        provider.batch_sizes(),
        vec![2, 2],
        "两发各带满「每请求文本块数」= 2 块：没有哪一发越界，也没有多发一发"
    );
    let windows = provider.windows();
    assert_eq!(windows.len(), 2, "两个并发槽位：两发请求各读一次窗口");
    for (next_ordinal, inflight) in &windows {
        assert_eq!(
            *next_ordinal, ordinals[0],
            "两个请求都在第一个块提交之前发出：窗口从头开始"
        );
        assert!(
            inflight.len() <= 4,
            "「处理中」最多是「任务并发 × 每请求文本块数」= 4 块，读到 {inflight:?}"
        );
        assert!(
            ordinals.starts_with(inflight),
            "窗口是块列表的一段前缀：发布与提交都按块序号，读到 {inflight:?}"
        );
    }
    assert_eq!(
        windows.iter().map(|(_, inflight)| inflight.len()).max(),
        Some(4),
        "第二发在四块全部进窗口之后才被轮询：窗口确实填到了上界"
    );
}

/// 一块自己的形状不合格时只有它退回单块请求，同批其余块照常落库。
///
/// 坏形状必须落在**多片段**块上才成立：整段合并成一条时，只有片段数大于一的块才数得
/// 出「少了」。
#[test]
pub(crate) fn a_rejected_block_is_retried_on_its_own_while_its_neighbours_are_stored() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    // 中间那一段带行内标记，因此有两个片段。
    fixture.rewrite_first_unit("<p>alpha</p><p>beta <em>inner</em></p><p>gamma</p>");
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    let troubled = &blocks[2];
    assert!(
        troubled.source.segments.len() >= 2,
        "被拒的那块必须是多片段块，否则「合并成一条」不是坏形状"
    );
    // 那一块在批里只答一条：片段数量不符 —— 这是块级错误，整批仍然可用。
    provider
        .merged_in_batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(troubled.source.text.clone());

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4, 1],
        "先是一发四块，然后是坏块自己的一发（单块路径）"
    );
    assert_eq!(
        provider.sent_sources()[1],
        vec![troubled.source.text.clone()],
        "回退的那一发只带坏块自己"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4, "坏块的邻居不该被牵连");
    // 坏块走的是单块路径：那条路径自己的读数留在任务日志里（这一发是它的第一次尝试）。
    let retried = recorded_events(&fixture, &job.id)
        .into_iter()
        .filter(|entry| entry.event == JobLogEvent::ModelRequested)
        .count();
    assert_eq!(
        retried, 5,
        "四块各有一发批请求，坏块另有它自己的一发单块请求"
    );
    let shapes = provider
        .batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert!(
        shapes[0].2 && !shapes[1].2,
        "首发是批信封，回退的那一发是单块请求"
    );
}

/// 信封不合格（块数与输入不符）时整批拆半重发，四块最终都落库。
#[test]
pub(crate) fn a_batch_with_a_wrong_envelope_is_split_in_half() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    // 第一发回一个块数不符的信封：整批一个块都不能用，必须拆半。
    provider.reject_envelopes.store(1, Ordering::SeqCst);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4, 2, 2],
        "一发四块 → 两发两块，块数每层减半"
    );
    assert_eq!(
        provider.sent_sources()[1..].to_vec(),
        vec![
            vec![blocks[0].source.text.clone(), blocks[1].source.text.clone()],
            vec![blocks[2].source.text.clone(), blocks[3].source.text.clone()],
        ],
        "拆半后每一半仍是连续的块，而且都按自己的块号落库"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4);
    assert_eq!(
        (stats.batched_requests, stats.batched_blocks),
        (1, 4),
        "walk 只记它自己打包的那一发；拆半的两发由批层直接发出，不进这项读数"
    );
}

/// 整批被输出上限截断时同样拆半，一直拆到单块请求为止。
#[test]
pub(crate) fn a_truncated_batch_splits_until_the_blocks_go_out_on_their_own() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    fixture.rewrite_first_unit("<p>alpha</p>");
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 2);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 2, "章标题 + 一个段落");
    provider.truncate_requests.store(1, Ordering::SeqCst);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![2, 1, 1],
        "截断的那一批先拆成两块，再各自走单块路径"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 2);
    assert_eq!(
        stats.batched_requests, 1,
        "只有第一发是真正的批量请求，拆到单块之后已经不再打包"
    );
}

/// 一发批请求自己失败（传输层）：这一批的每一块都按请求级失败跳过，运行照常走完。
///
/// 与「坏批拆半」分开：连接类失败拆半只会对同一个坏端点多发几发请求，所以整批一起认输，
/// 连续失败由既有的冷却兜住。
#[test]
pub(crate) fn a_failed_batch_request_skips_its_blocks_without_splitting() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.fail_requests.store(1, Ordering::SeqCst);

    let (outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![4],
        "整批认输：不再拆半，也不补发"
    );
    assert!(stored_translations(&fixture).is_empty(), "一块都没有译文");
    assert_eq!(stats.untranslated, 4, "这一批的四块都算跳过");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("个别块的请求失败不该让整次运行失败");
    };
    assert_eq!(finished.next_ordinal, blocks.len(), "游标照常走到书末");
    // 每一块都在任务日志里留下自己的失败读数（块级明细靠它们）。
    let skipped = recorded_events(&fixture, &job.id)
        .into_iter()
        .filter(|entry| entry.event == JobLogEvent::ProviderSkipped)
        .count();
    assert_eq!(skipped, 4);
}

/// 端点收流之前就以「超出上下文长度」拒掉整批：这是容量问题，拆半重发 ——
/// 一直拆到单块路径，而不是像传输层失败那样整批逐块跳过。
#[test]
pub(crate) fn a_context_rejected_batch_is_split_until_every_block_goes_out_on_its_own() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 2);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    provider.reject_context.store(1, Ordering::SeqCst);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![2, 1, 1, 2],
        "被拒的两块批拆成两个单块请求，下一窗的两块批照常发出"
    );
    assert_eq!(
        provider
            .batches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(_, _, batch)| *batch)
            .collect::<Vec<_>>(),
        vec![true, false, false, true],
        "拆半之后的两发走单块路径：一条请求只装一块时批信封没有意义"
    );
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4, "每一块都翻出来了，没有哪一块被整批跳过");
}

/// 一批只装同一个内容单元（同一章）的块：跨过章边界就必须另起一发。
///
/// `book`/`chapter` 是每批一份的语境字段，混进另一章的块会让这两个字段撒谎 —— 所以这条
/// 用例断言第二发请求的 `chapter` 就是第二章自己的标题。
#[test]
pub(crate) fn a_batch_never_mixes_two_chapters() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    fixture.rewrite_first_unit("<p>alpha</p><p>beta</p>");
    fixture.add_second_unit("第二章");
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "第一章的章标题与两段正文 + 第二章自己的块");
    assert_ne!(blocks[0].unit_id, blocks[3].unit_id);

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    assert_eq!(
        provider.batch_sizes(),
        vec![3, 1],
        "章边界把一批拆成两发，即使「每请求文本块数」还装得下"
    );
    let batches = provider
        .batches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert_eq!(
        batches[1].0, "第二章",
        "第二发的语境字段必须是第二章自己的标题"
    );
    assert_ne!(batches[0].0, "第二章", "第一发不许带上第二章的标题");
    for block in &blocks {
        assert_block_stored(&fixture, block.ordinal, &block.source);
    }
    assert_eq!(stats.saved, 4);
}

/// 收窄的重试只发它被允许发的块：重试集合之外的块既不进批，也不发请求，更不会被跳过。
#[test]
pub(crate) fn a_narrowed_retry_only_batches_the_blocks_it_is_allowed_to_send() {
    let provider = Arc::new(MockBatchProvider::default());
    let mut fixture = Fixture::new();
    three_paragraphs(&mut fixture);
    let (fixture, inner, job, mut cursor, translation) = batch_case_in(fixture, &provider, 4);
    let blocks = blocks_of(&fixture, &inner);
    assert_eq!(blocks.len(), 4, "章标题 + 三个段落");
    // 第二块已经有译文（同模型、同版本）：本次重试只针对它之后的两块。
    {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: fixture.book_id.clone(),
                content_unit_id: blocks[1].unit_id.clone(),
                block_id: blocks[1].block_id.clone(),
                ordinal: 1,
                document_revision: fixture.revision,
                unit_revision: blocks[1].unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("en".to_string()),
                model: translation.model.clone(),
                source_text: blocks[1].source.text.clone(),
                translated_text: serde_json::to_string(&StoredTranslation {
                    execution_identity: translation.execution_identity.clone(),
                    segments: blocks[1]
                        .source
                        .segments
                        .iter()
                        .map(|segment| crate::ai::translation::TranslationSegment {
                            source: segment.clone(),
                            translated: "已有译文".to_string(),
                        })
                        .collect(),
                })
                .unwrap(),
                created_at: unix_timestamp().unwrap(),
                updated_at: unix_timestamp().unwrap(),
            },
        )
        .unwrap();
    }
    cursor.retry_ordinals = vec![2, 3];
    cursor.next_ordinal = 0;
    // 运行开始时拿数据库里的任务行对账，所以这次重试的游标也要落库、并刷新手里的任务行。
    let job = {
        let conn = db::open_conn(&fixture.db_path).unwrap();
        db::index_jobs::update_state(
            &conn,
            &job.id,
            db::index_jobs::IndexJobStatus::Running,
            &cursor.encode().unwrap(),
            None,
            1,
            Some(1),
            None,
        )
        .unwrap();
        db::index_jobs::get(&conn, &job.id).unwrap().unwrap()
    };

    let (_outcome, stats) = run_batch_walk(&fixture, &inner, &job, &mut cursor, &translation, 1);

    let sent = provider.sent_sources();
    assert_eq!(
        sent.concat(),
        vec![blocks[2].source.text.clone(), blocks[3].source.text.clone()],
        "重试集合之外的块绝不出现在请求里（缓存命中也不会被顺手带上）"
    );
    assert_eq!(
        provider.batch_sizes().len(),
        1,
        "两发目标是同一章、又装得下：仍然可以合并成一发"
    );
    assert_block_stored(&fixture, 2, &blocks[2].source);
    assert_block_stored(&fixture, 3, &blocks[3].source);
    let stored = stored_translations(&fixture);
    assert!(
        stored[&1]
            .segments
            .iter()
            .all(|segment| segment.translated == "已有译文"),
        "没被重试的块保持原样"
    );
    assert_eq!(
        (stats.saved, stats.cached),
        (2, 0),
        "重试目标之外、但还在重试范围内的块按「不必处理」走过：既不算缓存命中，也不重新落库"
    );
}
