//! The mock OpenAI-compatible provider used by the indexing tests.

use super::*;

#[derive(Default)]
pub(crate) struct MockProvider {
    pub(crate) embedding_calls: AtomicUsize,
    pub(crate) vision_png_calls: AtomicUsize,
    pub(crate) fail_embeddings: AtomicBool,
    pub(crate) block_embeddings: AtomicBool,
    pub(crate) embedding_marker: AtomicUsize,
    pub(crate) embedding_delay_ms: AtomicUsize,
    pub(crate) vision_delay_ms: AtomicUsize,
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
