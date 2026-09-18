//! The visual-understanding pass: turns published page images into searchable
//! text chunks, and re-enqueues embedding for the chunks it writes.

use super::*;

use std::{collections::HashSet, sync::Arc, time::Instant};

use anyhow::{Result, ensure};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Deserialize;

use crate::{
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics},
    ai::{
        ChatMessage, ChatRequest, ChatRole, ContentPart, ImageUrl, MessageContent, ReasoningEffort,
    },
    db,
    document::{DocumentLocator, NormalizedRect, SourceLocator, deterministic_id},
    storage::BlobKey,
};

mod collect;

mod model;

mod parse;

mod request;

mod run;

pub(crate) use collect::*;

pub(crate) use model::*;

pub(crate) use parse::*;

pub(crate) use request::*;

pub(crate) use run::*;
