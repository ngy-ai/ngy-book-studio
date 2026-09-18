//! Bounded, content-free diagnostics for durable background jobs.
//!
//! This is a separate SQLite file under the owning library's data directory.
//! Recording never changes a job's outcome. Only fixed events, fixed error
//! categories and numeric measurements can enter the log; book text, responses,
//! credentials, URLs and arbitrary error strings have no input field.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub const BACKGROUND_JOB_LOG_LIMIT: usize = 500;
pub const BACKGROUND_JOB_LOG_TOTAL_LIMIT: usize = 50_000;
pub(crate) const MAX_DATABASE_PAGES: u64 = 16_384;
pub(crate) const MAX_PAYLOAD_BYTES: usize = 4096;

mod entry;

mod error;

mod event;

mod level;

mod metrics;

mod snapshot;

mod store;

pub use entry::*;

pub use error::*;

pub use event::*;

pub use level::*;

pub use metrics::*;

pub use snapshot::*;

pub(crate) use store::*;

#[cfg(test)]
mod tests;
