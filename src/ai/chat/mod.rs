//! Persistent, window-scoped AI conversations.
//!
//! SQLite row types stay in `db`; this module exposes validated domain types
//! and runs every database operation on the application's dedicated I/O
//! runtime so GPUI callbacks never have to execute SQLite work directly.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ai::ChatRole,
    ai::agent::AgentCitationSourceKind,
    db,
    document::{DocumentLocator, Revision, SourceLocator, deterministic_id},
    runtime::IoRuntime,
};

pub(crate) const MAX_SCOPE_BOOKS: usize = 512;
pub(crate) const MAX_TITLE_CHARS: usize = 256;
pub(crate) const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CITATIONS: usize = 256;
pub(crate) const MAX_QUOTE_BYTES: usize = 256 * 1024;

pub(crate) static ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

mod citation;

mod message;

mod repository;

mod row;

mod scope;

mod session;

mod thread;

mod util;

mod validate;

mod window;

pub use citation::*;

pub use message::*;

pub use repository::*;

pub use scope::*;

pub use session::*;

pub use thread::*;

pub use window::*;

pub(crate) use row::*;

pub(crate) use util::*;

pub(crate) use validate::*;

#[cfg(test)]
mod tests;
