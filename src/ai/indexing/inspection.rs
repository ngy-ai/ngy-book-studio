//! Read-only inspector of one whole-book translation job: block enumeration,
//! block detail, and the ad-hoc replay of one block answer.

use super::*;

use std::time::Instant;

use anyhow::Result;

use crate::db;

impl IndexingCoordinator {
    /// Enumerates the translatable blocks of one whole-book translation task
    /// with the same revision-pinned extraction the worker uses. This is a
    /// read-only inspection path: it never claims, advances, or publishes the
    /// job, so it can be called while the task is running. Expensive EPUB
    /// parsing happens once per call, so callers should cache the result per
    /// task identity instead of polling it.
    pub async fn translation_blocks(&self, job_id: &str) -> Result<TranslationBlockList> {
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let (job, context) = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let job = db::index_jobs::get(&conn, &job_id)?.context("后台任务已不存在")?;
            let context = translation_job_context(&conn, &job)?;
            Ok((job, context))
        })
        .await?;

        let blocks = load_translation_blocks(
            &self.inner,
            &job.book_id,
            &context.source_id,
            context.revision,
        )
        .await?;
        let total = blocks.len();
        let infos = blocks
            .into_iter()
            .map(|block| TranslationBlockInfo {
                ordinal: block.ordinal,
                unit_ordinal: block.unit_ordinal,
                unit_title: block.unit_title,
                source_preview: translation_block_preview(&block.source.text),
            })
            .collect();
        Ok(TranslationBlockList {
            target_language: context.target_language,
            total,
            next_ordinal: context.next_ordinal.min(total),
            blocks: infos,
        })
    }

    /// Rebuilds one text block of a whole-book translation task together with
    /// the single-block request the worker sends when this block goes out on
    /// its own（「每请求文本块数」为 1、坏批拆半退回单块、或单块重发；多块一批的
    /// 批信封取决于运行时打包，无法从钉住的修订重建）. Same read-only inspection
    /// contract as [`Self::translation_blocks`]: nothing is claimed, advanced or
    /// published, and the returned body is debug data derived from the pinned
    /// revision that is never persisted.
    pub async fn translation_block_detail(
        &self,
        job_id: &str,
        ordinal: usize,
    ) -> Result<TranslationBlockDetail> {
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let (job, context) = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let job = db::index_jobs::get(&conn, &job_id)?.context("后台任务已不存在")?;
            let context = translation_job_context(&conn, &job)?;
            Ok((job, context))
        })
        .await?;

        let blocks = load_translation_blocks(
            &self.inner,
            &job.book_id,
            &context.source_id,
            context.revision,
        )
        .await?;
        let block = blocks
            .into_iter()
            .find(|block| block.ordinal == ordinal)
            .with_context(|| format!("翻译任务中不存在文本块 #{ordinal}"))?;

        // The configured model is only a fallback: a task that already ran
        // records the model it was pinned to, and the body must describe that
        // execution rather than today's settings. 这里重建的一律是**单块**请求
        // （批路径的线上形状见 `translation_batch_request`，不可重建）。生成参数没有
        // 执行记录 —— 它不是执行身份的一部分，所以展示当天配置的那一份，与旁边
        // 「重试」「重新请求」实际会发出的请求一致。
        let (configured_model, generation) = self
            .inner
            .models
            .read()
            .map(|models| {
                models
                    .translation
                    .as_ref()
                    .map(|translation| (translation.model.clone(), translation.generation.clone()))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let model = context.model.clone().unwrap_or(configured_model);
        let system_prompt = translation_prompt(
            &context.target_language,
            context.source_language.as_deref(),
            &block.source,
            None,
        );
        let block_context = block.translation_context();
        let user_content = block.source.request_input(&block_context);
        let request = translation_request(
            &model,
            &generation,
            &context.target_language,
            context.source_language.as_deref(),
            &block.source,
            &block_context,
            None,
        );
        let body = serde_json::to_string(&request)
            .map_err(|_| anyhow::anyhow!("无法序列化翻译请求，请检查模型配置"))?;
        Ok(TranslationBlockDetail {
            ordinal: block.ordinal,
            unit_id: block.unit_id,
            unit_ordinal: block.unit_ordinal,
            unit_title: block.unit_title,
            unit_revision: block.unit_revision,
            block_id: block.block_id,
            source_text: block.source.text,
            segments: block.source.segments,
            request: TranslationBlockRequest {
                model,
                target_language: context.target_language,
                source_language: context.source_language,
                max_tokens: generation.max_output_tokens,
                temperature: generation.temperature,
                body,
                system_prompt,
                user_content,
            },
        })
    }

    /// Replays one text block against the configured translation model and
    /// returns the raw answer, for the task window's view of a failed block.
    ///
    /// Unlike every other inspection entry point, this one **does** send a real
    /// model request: the durable log keeps only fixed categories, so the only
    /// way to show what the model answered is to ask it again with the same
    /// frozen input. Everything else stays a pure read — the cursor does not
    /// move, no translation is stored, no diagnostic is recorded — and the
    /// answer is returned to the caller rather than persisted, exactly like the
    /// request body of [`Self::translation_block_detail`].
    ///
    /// The request is the single-block one — the shape this block gets when it
    /// goes out on its own: same frozen source, same `translation_request`
    /// builder, same configured model（多块的批重试走批信封，形状不同）。The stream is
    /// collected leniently (see `collect_probe_response`), so a truncated or
    /// malformed answer still reaches the window instead of failing the call.
    pub async fn probe_translation_block_response(
        &self,
        job_id: &str,
        ordinal: usize,
    ) -> Result<TranslationBlockProbe> {
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let (job, context) = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let job = db::index_jobs::get(&conn, &job_id)?.context("后台任务已不存在")?;
            let context = translation_job_context(&conn, &job)?;
            Ok((job, context))
        })
        .await?;

        let blocks = load_translation_blocks(
            &self.inner,
            &job.book_id,
            &context.source_id,
            context.revision,
        )
        .await?;
        let block = blocks
            .into_iter()
            .find(|block| block.ordinal == ordinal)
            .with_context(|| format!("翻译任务中不存在文本块 #{ordinal}"))?;

        // A replay always goes to the model configured today, because that is
        // what the neighbouring retry button would use: the point of asking is
        // to see what the current model answers, not what a past run recorded.
        let translation = {
            let models = self
                .inner
                .models
                .read()
                .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?;
            models.translation.clone()
        }
        .context("翻译服务尚未配置，无法重新请求模型")?;

        let request = translation_request(
            &translation.model,
            &translation.generation,
            &context.target_language,
            context.source_language.as_deref(),
            &block.source,
            &block.translation_context(),
            None,
        );
        let model = translation.model.clone();
        let started = Instant::now();
        let stream = translation
            .provider
            .chat_stream(request)
            .await
            .context("翻译模型请求失败")?;
        let collected = collect_probe_response(stream).await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        // Describe the answer with the worker's own validator — the same
        // `parse_response` the block path calls — so the verdict names exactly
        // the category a real run would have recorded for this answer.
        let parsed = crate::ai::translation::parse_response(
            &block.source,
            &collected.text,
            &translation.execution_identity,
        );
        let (decoded_segments, decode_error) = match &parsed {
            Ok(stored) => (Some(stored.segments.len()), None),
            Err(error) => (None, Some(error)),
        };
        tracing::info!(
            target: "ngy_ai",
            stage = "translation_block_probe",
            block_ordinal = ordinal,
            model = %model,
            end = collected.end,
            finish_reason = collected.finish_reason.unwrap_or("-"),
            response_bytes = collected.text.len(),
            decode_error_kind = decode_error
                .map(crate::ai::job_diagnostics::classify_error)
                .map(|kind| kind.label())
                .unwrap_or("-"),
            elapsed_ms,
            "Replayed one text block against the configured translation model"
        );

        Ok(TranslationBlockProbe {
            ordinal: block.ordinal,
            model,
            response_bytes: collected.text.len(),
            response_text: collected.text,
            end: collected.end,
            finish_reason: collected.finish_reason,
            decoded_segments,
            decode_error_kind: decode_error.map(crate::ai::job_diagnostics::classify_error),
            decode_error_line: decode_error
                .and_then(|error| error.downcast_ref::<crate::ai::translation::ResponseError>())
                .and_then(|error| error.json_line()),
            decode_error_column: decode_error
                .and_then(|error| error.downcast_ref::<crate::ai::translation::ResponseError>())
                .and_then(|error| error.json_column()),
            elapsed_ms,
        })
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_state(
        &self,
        job_id: &str,
        expected: IndexingJobStatus,
        timeout: Duration,
    ) -> Result<IndexingJobSnapshot> {
        let deadline = Instant::now() + timeout;
        loop {
            let current = self.job(job_id).await?;
            if let Some(job) = current.as_ref()
                && job.status == expected
            {
                return Ok(job.clone());
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for {expected:?} on {job_id} after {}s; current state: {current:?}",
                timeout.as_secs()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
