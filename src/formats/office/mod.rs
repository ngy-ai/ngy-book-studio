use std::{collections::HashMap, io::Cursor, sync::Arc};

use anyhow::{Context as _, Result, bail};
use office_oxide::{Document, DocumentFormat, DocumentIR};

use crate::{
    document::{
        AssetRole, Block, BlockDocument, BookDocument, BookFormat, BookSource, ContentUnit,
        ContentUnitKind, SourceLocator, TocNode, TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedAsset, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, asset_from_bytes, original_asset,
        safe_title,
    },
};

mod archive;

mod importer;

mod units;

mod worksheet;

pub(crate) use archive::*;

pub(crate) use units::*;

pub(crate) use worksheet::*;

#[cfg(test)]
mod tests;
#[derive(Clone, Copy, Debug, Default)]
pub struct OfficeImporter;

pub(crate) const MAX_OFFICE_ARCHIVE_ENTRIES: usize = 20_000;
pub(crate) const MAX_OFFICE_ENTRY_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_OFFICE_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MIN_COMPRESSION_RATIO_CHECK_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_OFFICE_COMPRESSION_RATIO: u64 = 1_000;
pub(crate) const MAX_XLSX_GRID_ROWS: u32 = 10_000;
pub(crate) const MAX_XLSX_GRID_CELLS: u64 = 1_000_000;
pub(crate) const MAX_XLSX_COLUMNS: u32 = 16_384;
pub(crate) const MAX_XLSX_ROWS: u32 = 1_048_576;

pub(crate) const OFFICE_FORMATS: &[BookFormat] = &[
    BookFormat::Doc,
    BookFormat::Docx,
    BookFormat::Pptx,
    BookFormat::Xlsx,
];
