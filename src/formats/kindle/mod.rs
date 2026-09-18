use std::{borrow::Cow, collections::HashMap, sync::Arc};

use anyhow::{Context as _, Result, bail};

use crate::{
    document::{
        AssetRole, BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind,
        SourceLocator, TocNode, TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, asset_from_bytes, kindle_huff,
        normalized_archive_href, original_asset, rewrite_imported_html_assets, safe_title,
    },
};

mod chapter;

mod css;

mod entity;

mod format;

mod header;

mod importer;

mod palm;

mod resource;

mod rewrite;

mod source;

pub(crate) use chapter::*;

pub(crate) use css::*;

pub(crate) use entity::*;

pub(crate) use format::*;

pub(crate) use header::*;

pub(crate) use palm::*;

pub(crate) use resource::*;

pub(crate) use rewrite::*;

pub(crate) use source::*;

#[cfg(test)]
mod tests;
#[derive(Clone, Copy, Debug, Default)]
pub struct KindleImporter;

pub(crate) const KINDLE_FORMATS: &[BookFormat] =
    &[BookFormat::Mobi, BookFormat::Azw, BookFormat::Azw3];
