//! Durable embedding and visual-understanding workers.
//!
//! Canonical text and FTS are committed before these jobs are queued. Model
//! failures therefore degrade semantic/visual recall without making a book or
//! keyword search unavailable. Every externally visible state and batch
//! cursor lives in SQLite so process restarts can resume safely.
//!
//! The implementation is split by responsibility: `worker` drives the queue,
//! `embedding` / `vision` are the two model passes, `translation` owns the
//! whole-book translation task, `control` owns pause/cancel observation and
//! outcome publication, `inspection` owns the read-only inspector, and
//! `support` holds the helpers they all share.

/// Everything a submodule reaches through `use super::*`.
///
/// Kept whole on purpose: each submodule uses a subset of it, and re-deriving
/// the set per file is exactly how the two drifted apart before.
#[allow(unused_imports)]
mod prelude {
    pub(crate) use std::{
        collections::{BTreeMap, HashMap, HashSet},
        future::Future,
        path::PathBuf,
        pin::Pin,
        sync::{
            Arc, Mutex, RwLock,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    pub(crate) use anyhow::{Context, Result, ensure};
    pub(crate) use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
    pub(crate) use futures_util::{
        FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered,
    };
    pub(crate) use serde::{Deserialize, Serialize};
    pub(crate) use tokio::{
        runtime::Handle,
        sync::{Mutex as AsyncMutex, Notify},
        task::JoinHandle,
    };
    pub(crate) use tracing::Instrument;

    pub(crate) use crate::{
        ai::job_diagnostics::{
            JobLogErrorKind, JobLogEvent, JobLogMetrics, classify_error, record_for_database,
        },
        ai::translation::{StoredTranslation, TranslationContext, TranslationSource},
        ai::{
            ChatMessage, ChatRequest, ChatRole, ContentPart, DEFAULT_TRANSLATION_OUTPUT_TOKENS,
            EmbeddingRequest, ImageUrl, MessageContent, OpenAiCompatibleProvider, ReasoningEffort,
            TranslationGenerationSettings,
        },
        db,
        document::{
            BlockDocument, DocumentLocator, NormalizedRect, SourceLocator, deterministic_id,
        },
        storage::{BlobKey, BlobStore},
    };
}

pub(crate) use prelude::*;

const QUEUE_SCAN_LIMIT: usize = 64;

const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);

const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(400);

const PROVIDER_STEP_TIMEOUT: Duration = Duration::from_secs(180);

/// 流式翻译响应允许的最长静默。**静默，不是总时长**：持续产出数据的慢模型照旧
/// 可以慢慢翻完（`max_tokens`、响应字节上限与输出上限截断仍然只管输出规模）。
///
/// 「静默由 provider 层的空闲超时负责」（`ProviderTimeout::stream`）在正常路径上成立，
/// 但它并不兜住「流本身再也不产生任何事件」：现场 2026-09-17 的整本翻译在开出 8 个
/// 块请求后彻底静默——job-logs 自 14:50:45 起再无写入、进程没有任何 TCP 连接、
/// `await_translation_step` 的 180 秒上限也没有触发——于是整个文本块、进而整本书
/// 永久停在同一块上，并占住索引协调器的 `transitions` 门闩，把设置保存一起卡住。
/// 静默上限与 provider 步上限同值，避免出现两套“多久算死”的读数。
const TRANSLATION_STREAM_SILENCE_LIMIT: Duration = PROVIDER_STEP_TIMEOUT;

const EMBEDDING_BATCH_SIZE: usize = 16;

const MAX_EMBEDDING_INPUT_BYTES: usize = 24 * 1024;

const MAX_EMBEDDING_BATCH_BYTES: usize = 256 * 1024;

const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;

const VISION_PAGE_BATCH_SIZE: usize = 1;

const MAX_VISION_IMAGE_BYTES: usize = 12 * 1024 * 1024;

const MAX_VISION_RESPONSE_BYTES: usize = 128 * 1024;

const MAX_VISION_REGION_TEXT_CHARS: usize = 16 * 1024;

const MAX_VISION_TOTAL_TEXT_CHARS: usize = 64 * 1024;

const MAX_VISION_REGIONS_PER_PAGE: usize = 32;

const VISUAL_CHUNK_ORDINAL_BASE: usize = 1_000_000_000;

const VISUAL_CHUNK_ORDINALS_PER_PAGE: usize = MAX_VISION_REGIONS_PER_PAGE;

const MAX_PERSISTED_ERROR_CHARS: usize = 4_096;

const TRANSLATION_JOB_KIND: &str = "translation";

const MAX_TRANSLATION_SOURCE_CHARS: usize = 8_000;

const MAX_TRANSLATION_REQUEST_BYTES: usize = 64 * 1024;

const MAX_TRANSLATION_RESPONSE_BYTES: usize = 64 * 1024;

const MAX_TRANSLATION_BLOCKS: usize = 500_000;

/// Display-only preview length of one block in the task window.
const TRANSLATION_BLOCK_PREVIEW_CHARS: usize = 96;

const TRANSLATION_RESPONSE_ATTEMPTS: usize = 2;

/// 连续多少个文本块的**请求级**失败就停止本次运行。
///
/// 协议类跳过刻意没有连续上限（弱模型连续不合格是常态，现场 2026-09-15 已把那个上限
/// 去掉），但「请求本身失败」是另一回事：偶尔一块连不上很常见，连续若干块都连不上
/// 说明模型或端点已经不可用。没有这条上限，一本三千块的书会被逐块跳过、最后报「完成」
/// 而一行译文都没有（每次都等超时的话还要跑几十小时）。5 是「个别抖动不会误判」与
/// 「真的挂了就尽快停」之间的取值：单次失败率 5% 时，三千块里凑出连续 5 次的概率约
/// 千分之一。
const TRANSLATION_CONSECUTIVE_REQUEST_FAILURES: usize = 5;

/// 一个文本块整轮失败后再重发的次数。一轮 = 两次严格尝试 + 一次协议纠正，最后一轮
/// 还包含逐片段回退，所以一个块最多发出 `TRANSLATION_BLOCK_RETRIES + 1` 轮请求。
/// 只有协议类失败（模型答了、但形状不对）才重发；provider、数据库与取消照旧让整次
/// 运行失败。重试轮的第一发带着上一轮的失败类别，因此重发不是逐字相同的请求。
const TRANSLATION_BLOCK_RETRIES: usize = 5;

/// 文本块提取规则的版本，`markup.rs` 里「这一块是源代码」的判定变化时必须 +1：
/// 块数会变，旧游标的序号不再指向同一块（见 `JobCursor::from_job`）。
const TRANSLATION_BLOCKS_VERSION: u32 = 1;

/// Interval of the content-free progress line of one translation stream. A slow
/// local model can answer one block for minutes; without a periodic line a
/// truncated run cannot be told apart from a stalled provider.
const TRANSLATION_STREAM_PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

/// 逐片段回退的上限（等于一个文本块的片段数）。一个多片段文本块在两次严格尝试后
/// 仍失败时，改用「每个文字叶单独一次请求」：单片段请求与弱模型的自然输出一致
/// （它只需要写一个 id=0 的答案），因此不需要模型理解「N 个片段」这件事。超过上限
/// 的文本块不再回退，按原来的规则跳过，避免一个病态段落耗光整本图书的请求预算。
const MAX_TRANSLATION_FRAGMENT_FALLBACKS: usize = 32;

/// 进日志的片段长度列表最多打印多少个数字，超出的只报个数。
const TRANSLATION_SEGMENT_LABEL_LIMIT: usize = 64;

static NEXT_TRANSLATION_RUN_ID: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    static INDEXING_LOG_RUN: (u64, Instant);
}

mod control;
mod coordinator;
mod embedding;
mod inspection;
mod job;
mod state;
mod support;
pub(crate) mod translation;
mod translation_jobs;
mod vision;
mod worker;

pub(crate) use control::*;
pub(crate) use coordinator::*;
pub(crate) use embedding::*;
pub(crate) use job::*;
pub(crate) use state::*;
pub(crate) use support::*;
pub(crate) use translation::*;
pub(crate) use vision::*;
pub(crate) use worker::*;

// The task window block inspection types are part of the public service API
// (`AppServices` re-exports them), so they keep their `pub` visibility instead
// of the crate-wide one the internal helpers above use.
pub use translation::{
    TranslationBlockDetail, TranslationBlockInfo, TranslationBlockList, TranslationBlockProbe,
    TranslationBlockRequest,
};

#[cfg(test)]
mod tests;
