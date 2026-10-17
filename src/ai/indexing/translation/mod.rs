//! The whole-book translation task: the durable walk over extracted text
//! blocks and the per-block commit rules.
//!
//! - `blocks` extracts the deterministic, revision-pinned block list.
//! - `prompt` builds the strict-JSON request.
//! - `response` collects and validates one streaming answer.

use super::*;

mod blocks;

mod commit;

mod prompt;

mod response;

mod run;

mod walk;

pub(crate) use blocks::*;

pub(crate) use commit::*;

pub(crate) use prompt::*;

pub(crate) use response::*;

pub(crate) use run::*;

pub(crate) use walk::*;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Result, ensure};
use futures_util::{future::BoxFuture, stream::FuturesUnordered};

use crate::{
    ai::job_diagnostics::{JobLogErrorKind, JobLogEvent, JobLogMetrics, classify_error},
    ai::translation::{StoredTranslation, TranslationContext, TranslationSource},
    db,
};

/// One translatable text block in a deterministic, revision-pinned order.
/// `ordinal` is the flattened index used both as the durable job cursor and as
/// the persisted block ordering within its unit.
/// One translatable text block in a deterministic, revision-pinned order.
/// `ordinal` is the flattened index used both as the durable job cursor and as
/// the persisted block ordering within its unit.
pub(crate) struct TranslationBlock {
    /// 书名是每个文本块都要带给模型的语境，因此和块一起冻结下来：工作线程与
    /// 任务窗口都从这里取上下文，两者发出的请求体才逐字节相同。
    pub(crate) book_title: String,
    pub(crate) unit_id: String,
    pub(crate) unit_revision: u64,
    pub(crate) unit_title: String,
    pub(crate) unit_ordinal: usize,
    pub(crate) block_id: String,
    pub(crate) ordinal: usize,
    pub(crate) source: TranslationSource,
}

impl TranslationBlock {
    pub(crate) fn translation_context(&self) -> TranslationContext<'_> {
        TranslationContext::new(&self.book_title, &self.unit_title)
    }
}

/// One block of a whole-book translation task as inspected from the task
/// window. The preview is derived from the revision-pinned source text and is
/// never persisted; whether the block is done, in flight, or pending stays a
/// pure function of the live durable cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationBlockInfo {
    pub ordinal: usize,
    pub unit_ordinal: usize,
    pub unit_title: String,
    pub source_preview: String,
}

/// Structural block list of one whole-book translation task. `next_ordinal` is
/// the durable cursor observed while inspecting, so the caller can tell a
/// processed block from the one currently in flight without re-parsing the
/// book on every poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationBlockList {
    pub target_language: String,
    pub total: usize,
    pub next_ordinal: usize,
    pub blocks: Vec<TranslationBlockInfo>,
}

/// One text block of a whole-book translation task, with the full revision
/// pinned source and the single-block request the worker builds when this block
/// goes out on its own（多块一批的批信封取决于运行时打包，无法从钉住的修订重建）.
/// Debugging payload of the task window: it is rebuilt on demand from the pinned
/// revision and never persisted, so a book or provider body cannot leak into
/// the durable log or database.
#[derive(Clone, Debug, PartialEq)]
pub struct TranslationBlockDetail {
    pub ordinal: usize,
    pub unit_id: String,
    pub unit_ordinal: usize,
    pub unit_title: String,
    pub unit_revision: u64,
    pub block_id: String,
    /// Normalized visible text of the whole block.
    pub source_text: String,
    /// Text leaves the model is allowed to translate, in document order.
    pub segments: Vec<String>,
    pub request: TranslationBlockRequest,
}

/// One on-demand replay of a single text block, for the task window's view of a
/// *failed* block.
///
/// The durable diagnostics deliberately keep only fixed categories, so the only
/// way to show what a model actually answered for a block it could not translate
/// is to ask it again with the same frozen input. A replay is read-only with
/// respect to the task: it never advances the cursor, never stores a translation
/// and never records a diagnostic. Like [`TranslationBlockDetail`]'s request
/// body, the answer lives only in the window and is never persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationBlockProbe {
    pub ordinal: usize,
    /// Model the replay was sent to: the configured translation model, i.e. the
    /// one the neighbouring「重试」button would use.
    pub model: String,
    /// The answer, byte for byte: no fence stripping, no trimming, no parsing.
    /// Seeing the untouched text is the whole point — a wrapped or truncated
    /// answer has to look wrapped or truncated here.
    pub response_text: String,
    pub response_bytes: usize,
    /// How the stream ended: `done` (the model sent a terminal chunk),
    /// `unexpected_finish_reason` (it stopped early — `finish_reason` says how),
    /// `stream_eof` (the stream closed without a terminal chunk), `size_limit`,
    /// `tool_call`, or `stream_error` (the provider failed mid-stream).
    pub end: &'static str,
    /// Model-reported finish reason, mapped through the fixed label table so no
    /// provider-supplied text can reach the window.
    pub finish_reason: Option<&'static str>,
    /// Segments the strict decoder accepted, and the fixed category plus
    /// coordinates it rejected the answer with. `decode_error_kind: None` means
    /// the answer would have been accepted.
    pub decoded_segments: Option<usize>,
    pub decode_error_kind: Option<JobLogErrorKind>,
    pub decode_error_line: Option<usize>,
    pub decode_error_column: Option<usize>,
    pub elapsed_ms: u64,
}

/// The translation request one block produces, in the exact shape the worker
/// sends when this block goes out on its own（「每请求文本块数」为 1 或批回退到
/// 单块；多块一批的请求另走批信封，形状不同）: the system prompt and the user
/// message are reported separately so the window can show either, and `body` is
/// the serialized `ChatRequest`.
#[derive(Clone, Debug, PartialEq)]
pub struct TranslationBlockRequest {
    pub model: String,
    pub target_language: String,
    pub source_language: Option<String>,
    pub max_tokens: u32,
    /// 采样温度。`None` 表示设置里留空、由服务端默认值决定，与请求体里没有这个键
    /// 是同一件事。
    pub temperature: Option<f32>,
    /// Serialized chat-completions body, including the system and user message.
    pub body: String,
    pub system_prompt: String,
    pub user_content: String,
}

pub(crate) fn translation_target_language(job: &db::index_jobs::IndexJob) -> Result<String> {
    let source_id = job.source_id.as_deref().context("翻译任务缺少来源")?;
    let prefix = format!("{TRANSLATION_JOB_KIND}:{source_id}:");
    job.id
        .strip_prefix(&prefix)
        .map(str::to_string)
        .filter(|language| !language.is_empty())
        .context("翻译任务标识与来源不匹配")
}

/// Identity one whole-book translation task is pinned to: the source revision
/// its blocks come from, the target language, and the source language the
/// request prompt reports. Both the block list and the single-block detail read
/// the task through this, so a block the window shows is always the block the
/// worker would translate.
pub(crate) struct TranslationJobContext {
    pub(crate) source_id: String,
    pub(crate) target_language: String,
    pub(crate) revision: u64,
    /// Durable progress of the inspected task, i.e. the first block it has not
    /// finished yet.
    pub(crate) next_ordinal: usize,
    /// Model recorded by the durable cursor when the execution started. A task
    /// that never ran has no model yet; the configured one is used instead.
    pub(crate) model: Option<String>,
    pub(crate) source_language: Option<String>,
}

pub(crate) fn translation_job_context(
    conn: &rusqlite::Connection,
    job: &db::index_jobs::IndexJob,
) -> Result<TranslationJobContext> {
    ensure!(
        job.kind == TRANSLATION_JOB_KIND,
        "只有图书翻译任务可以查看文本块明细"
    );
    let source_id = job.source_id.clone().context("翻译任务缺少来源")?;
    let target_language = translation_target_language(job)?;
    let cursor = serde_json::from_str::<JobCursor>(&job.cursor_json).context("翻译任务游标无效")?;
    ensure!(
        cursor.schema_version == 1
            && cursor.book_id == job.book_id
            && cursor.source_id == source_id
            && cursor.kind == TRANSLATION_JOB_KIND,
        "翻译任务游标与任务身份不匹配"
    );
    let source_language = db::books::get(conn, &job.book_id)?.and_then(|book| book.language);
    Ok(TranslationJobContext {
        source_id,
        target_language,
        revision: cursor.revision,
        next_ordinal: cursor.next_ordinal,
        model: cursor.model,
        source_language,
    })
}

/// 一次翻译运行的计数，用于收尾汇总：一行说明翻了多少、复用了多少、跳过多少，
/// 有多少文本块走了逐片段回退，以及为连续请求失败冷却过几次，不必再通读整段运行日志。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TranslationRunStats {
    pub(crate) saved: usize,
    pub(crate) cached: usize,
    pub(crate) no_segments: usize,
    pub(crate) untranslated: usize,
    pub(crate) fragment_fallback_blocks: usize,
    pub(crate) fragment_fallback_requests: usize,
    /// 真正等过的冷却次数（书末没有新请求要发时那次不算）。
    pub(crate) cooldowns: usize,
    /// 带走了多于一个文本块的请求数（端点的「每请求文本块数」大于 1 时才可能非零）。
    pub(crate) batched_requests: usize,
    /// 这些批量请求一共带走了多少块。与 [`Self::batched_requests`] 一起给出「省了多少
    /// 发请求」的读数：单块请求数 = 总请求数 - `batched_requests`。
    pub(crate) batched_blocks: usize,
}
