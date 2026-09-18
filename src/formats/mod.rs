//! Format probing and import/export boundaries.
//!
//! Third-party parser representations are converted immediately into the
//! application's [`BookDocument`](crate::document::BookDocument). They never
//! cross this module or become database row types.

mod djvu;
mod epub;
mod kfx;
mod kindle;
pub(crate) mod kindle_huff;
mod office;
mod pdf;

use std::{
    cmp::Reverse,
    collections::HashMap,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use html5ever::{
    parse_document,
    serialize::{SerializeOpts, TraversalScope, serialize},
    tendril::TendrilSink as _,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};

use crate::document::{AssetRef, AssetRole, Block, BlockDocument, BookDocument, BookFormat};

pub use djvu::DjvuImporter;
pub(crate) use djvu::{MAX_DJVU_PAGES, MAX_DJVU_SOURCE_BYTES, djvu_form_type};
pub use epub::EpubImporter;
pub use kfx::KfxImporter;
pub use kindle::KindleImporter;
pub use office::OfficeImporter;
pub use pdf::PdfImporter;

/// Whether a stored format can declare a language at all. A caller that would
/// otherwise have to read a whole original — a backfill, say — must check this
/// first: PDF, Office and DjVu never carry a tag we trust, so reading them would
/// be real I/O for a guaranteed `None`.
mod assets;

mod budget;

mod limits;

mod probe;

mod registry;

mod source;

pub(crate) use assets::*;

pub(crate) use budget::*;

pub use limits::*;

pub use probe::*;

pub use registry::*;

pub use source::*;

#[cfg(test)]
mod tests;
pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_UNITS: usize = 20_000;
pub const DEFAULT_MAX_UNIT_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_TEXT_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_ASSETS: usize = 20_000;
pub const DEFAULT_MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_ASSET_BYTES: u64 = 1024 * 1024 * 1024;
