//! A provider that merges fragmented responses.

use super::*;

/// 请求也回一个不合格的答案。
#[derive(Default)]
pub(crate) struct MockMergeProvider {
    pub(crate) requests: AtomicUsize,
    pub(crate) merged_requests: AtomicUsize,
    pub(crate) fragment_requests: AtomicUsize,
    pub(crate) fail_fragments: AtomicBool,
    /// 现场（2026-09-15，block 1762，qwen3.5:0.8b）：模型在输出上限处停下，
    /// `finish_reason="length"`，响应里连一个完整容器都没有。
    pub(crate) truncated: AtomicBool,
    /// 现场（2026-09-15，block 1786/1787，qwen3.5:0.8b）：纠正提示把「合并成一条」
    /// 换成了「开始按段写、但 JSON 不闭合」。这个开关让多片段请求按 attempt 序号
    /// 交替给出这两种形状，用于固定「整轮失败类别里只要有一次可回退，就要回退」。
    pub(crate) correction_breaks_json: AtomicBool,
    /// 每个请求的模拟耗时。0 表示立刻返回，其余用例的行为不变；并发用例靠它把
    /// 请求真正叠在一起，再用 `peak` 读出窗口到底开了多大。
    pub(crate) delay_ms: AtomicU64,
    /// 同时打开的请求数与其峰值。
    pub(crate) active: AtomicUsize,
    pub(crate) peak: AtomicUsize,
    /// 设置后，每个请求开始时读一次持久游标并记下窗口。
    pub(crate) probe: std::sync::OnceLock<WindowProbe>,
    /// 前 N 次请求一律回一个不合协议的答案（`segment_count_mismatch`），用于固定
    /// 块级重试：模型先答错、重发时答对。
    pub(crate) reject_requests: AtomicUsize,
    /// provider 直接失败（HTTP/连接类，不是协议失败），用于固定「非协议失败不重试」。
    pub(crate) fail_provider: AtomicBool,
    /// 每个请求都声明「不需要翻译」：`{"id":N,"text":"","keep":true}`。
    pub(crate) keep_segments: AtomicBool,
    /// 每个请求的开始时刻，用于固定「请求之间必须休眠任务间隔」——
    /// `delay_ms` 只能证明请求叠在一起，证明不了它们被节流。
    pub(crate) starts: Mutex<Vec<std::time::Instant>>,
}

impl MockMergeProvider {
    pub(crate) fn attach_probe(&self, db_path: PathBuf, job_id: String) {
        self.probe
            .set(WindowProbe {
                db_path,
                job_id,
                windows: Mutex::new(Vec::new()),
            })
            .unwrap_or_else(|_| panic!("窗口探针只能装一次"));
    }

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

    /// 每次模型请求的开始时刻，按发起顺序。
    pub(crate) fn request_starts(&self) -> Vec<std::time::Instant> {
        self.starts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
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
                self.starts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(std::time::Instant::now());
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
                let reply = if self.keep_segments.load(Ordering::SeqCst) {
                    // 每个片段都声明「不需要翻译」：`keep` 必须与空 text 一起出现。
                    let items = (0..segments.len())
                        .map(|id| format!("{{\"id\":{id},\"text\":\"\",\"keep\":true}}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("{{\"translations\":[{items}]}}")
                } else if segments.len() == 1 {
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
