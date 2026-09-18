//! Stable exports from the application's canonical document model.
//!
//! Exporters resolve immutable assets through [`AssetResolver`]. They neither
//! know object-store paths nor expose third-party document representations.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::{Cursor, Write},
    path::Path,
};

use anyhow::{Context as _, Result, bail, ensure};
use image::ImageReader;
use tempfile::NamedTempFile;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::document::{
    AssetRef, AssetRole, Block, BookDocument, BookSource, Inline, ListItem, TableRow, TocNode,
    TocTarget,
};

mod epub;

mod epub_blocks;

mod epub_nav;

mod epub_xml;

mod exporter;

mod format;

mod original;

mod pdf;

mod pdf_build;

mod pdf_image;

mod resolver;

mod write;

pub use format::*;

pub use resolver::*;

pub use write::*;

pub(crate) use epub::*;

pub(crate) use epub_blocks::*;

pub(crate) use epub_nav::*;

pub(crate) use epub_xml::*;

pub(crate) use original::*;

pub(crate) use pdf::*;

pub(crate) use pdf_build::*;

pub(crate) use pdf_image::*;

#[cfg(test)]
mod tests;
pub(crate) const EPUB_MIME: &[u8] = b"application/epub+zip";
pub(crate) const EPUB_STYLE: &str = r#"body { margin: 5%; line-height: 1.65; }
img, video { max-width: 100%; height: auto; }
table { border-collapse: collapse; max-width: 100%; }
th, td { border: 1px solid #888; padding: 0.25em 0.5em; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; }
figure { margin: 1em 0; }
figcaption { color: #555; font-size: 0.9em; }
"#;
pub(crate) const PDF_WIDTH: f32 = 595.0;
pub(crate) const PDF_HEIGHT: f32 = 842.0;
pub(crate) const PDF_MARGIN: f32 = 48.0;
pub(crate) const PDF_LINES_PER_PAGE: usize = 49;
pub(crate) const PDF_LINE_WIDTH: usize = 84;
pub(crate) const MAX_PDF_IMAGE_DIMENSION: u32 = 10_000;
pub(crate) const MAX_PDF_IMAGE_ALLOCATION: u64 = 256 * 1024 * 1024;
