//! Format-neutral book document model.
//!
//! The types in this module deliberately do not expose database rows, native
//! file paths, object-store keys, or WebView-specific state. They form the
//! stable boundary shared by importers, exporters, the editor, search, and AI
//! citations.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::mem;

use html5ever::{parse_document, tendril::TendrilSink as _};
use markup5ever_rcdom::{NodeData, RcDom};
use serde::{Deserialize, Serialize};

mod asset;

mod block;

mod inline;

mod locator;

mod model;

mod text;

mod toc;

mod validate_content;

mod validate_refs;

mod validation;

pub(crate) use text::*;

pub(crate) use validate_content::*;

pub(crate) use validate_refs::*;

pub(crate) use validation::*;

// The public model surface, unchanged by the split.
pub use asset::{AssetRef, AssetRole};
pub use block::{Block, BlockDocument};
pub use inline::{Inline, ListItem, TableCell, TableRow};
pub use locator::{DocumentLocator, NormalizedRect, TextRange};
pub use model::{
    BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind, SourceLocator,
};
pub use toc::{TocNode, TocTarget};
pub use validation::{ValidationError, ValidationIssue};

#[cfg(test)]
mod tests;
pub const DOCUMENT_SCHEMA_VERSION: u32 = 2;

pub const BLOCK_SCHEMA_VERSION: u32 = 1;

/// Visual coordinates are normalized to this inclusive upper bound so a
/// persisted locator does not depend on the renderer's pixel dimensions.
pub const NORMALIZED_COORDINATE_MAX: u16 = 1_000;
// Keep the semantic model limit comfortably below serde_json's default 128
// container limit. A nested list level adds an item object and two arrays in
// addition to the child block object, so a limit close to 64 cannot make a
// valid BookDocument JSON-round-trippable.

pub const MAX_DOCUMENT_DEPTH: usize = 24;

thread_local! {
    static MODEL_SERDE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct ModelSerdeDepthGuard {
    pub(crate) previous: usize,
}

impl Drop for ModelSerdeDepthGuard {
    fn drop(&mut self) {
        MODEL_SERDE_DEPTH.with(|depth| depth.set(self.previous));
    }
}

pub(crate) fn enter_model_serde_depth() -> Option<ModelSerdeDepthGuard> {
    MODEL_SERDE_DEPTH.with(|depth| {
        let previous = depth.get();
        if previous > MAX_DOCUMENT_DEPTH {
            return None;
        }
        depth.set(previous + 1);
        Some(ModelSerdeDepthGuard { previous })
    })
}

/// A persisted revision. Editor-session revisions should use a separate
/// monotonic counter and must not be confused with this value.
#[derive(
    Clone, Copy, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub const INITIAL: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Creates a deterministic, content-derived identifier without requiring a
/// random-number generator. The full BLAKE3 digest is retained.
pub fn deterministic_id(prefix: &str, seed: impl AsRef<[u8]>) -> String {
    let prefix = prefix.trim();
    let seed = seed.as_ref();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(prefix.len() as u64).to_le_bytes());
    hasher.update(prefix.as_bytes());
    hasher.update(&(seed.len() as u64).to_le_bytes());
    hasher.update(seed);
    let digest = hasher.finalize().to_hex();
    if prefix.is_empty() {
        digest.to_string()
    } else {
        format!("{prefix}-{digest}")
    }
}
