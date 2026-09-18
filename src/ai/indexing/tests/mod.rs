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

mod embedding_test;
mod endpoints_test;
mod job_control_test;
mod scheduling_test;
mod translation_retry_test;
mod translation_test;
mod translation_walk_test;
mod unit_test;
mod vision_publication_test;
mod vision_test;

/// How long one test waits for the coordinator to reach a state or to make a
/// provider call. Every test in this module runs in parallel while other
/// test binaries may be loading the same machine, so a progress wait has to
/// outlast a busy one: a coordinator that really is stuck still fails, only
/// later and with the state it was stuck in. Several fixtures also hand the
/// provider a deliberate 5 second delay, so the old 5 second budget raced
/// the very work it was waiting for.
pub(crate) const TEST_PROGRESS_WAIT: Duration = Duration::from_secs(30);

pub(crate) fn test_model_config(
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

mod fixture;

mod merge;

mod mocks;

mod probe;

mod sources;

pub(crate) use fixture::*;

pub(crate) use merge::*;

pub(crate) use mocks::*;

pub(crate) use probe::*;

pub(crate) use sources::*;
