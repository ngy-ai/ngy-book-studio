//! Language declaration and the import limits every format shares.

use super::*;

/// Whether a stored format can declare a language at all. A caller that would
/// otherwise have to read a whole original — a backfill, say — must check this
/// first: PDF, Office and DjVu never carry a tag we trust, so reading them would
/// be real I/O for a guaranteed `None`.
pub(crate) fn declares_language(format: &str) -> bool {
    matches!(format, "epub" | "mobi" | "azw" | "azw3" | "kfx")
}

/// The language an original file declares, read from container metadata only.
/// EPUB and the Kindle family (`mobi`/`azw`/`azw3`) and KFX declare a tag that
/// our importers record in `books.language`, which is what lets the translation
/// scheduler skip a book that is already in the target language. PDF, Office and
/// DjVu declare nothing we trust, so a backfill has nothing to read for them.
///
/// The first return value is `None` for a container that simply declares no
/// language; an unreadable or rejected container surfaces as an error the
/// caller logs and skips.
pub(crate) fn declared_language(format: &str, bytes: &[u8]) -> Result<Option<String>> {
    match format {
        "epub" => epub::declared_language(bytes),
        "mobi" | "azw" | "azw3" => kindle::declared_language(bytes),
        "kfx" => kfx::declared_language(bytes),
        _ => Ok(None),
    }
}
pub(crate) use pdf::{MAX_PDF_PAGES, MAX_PDF_SOURCE_BYTES};

pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_UNITS: usize = 20_000;
pub const DEFAULT_MAX_UNIT_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_TEXT_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_ASSETS: usize = 20_000;
pub const DEFAULT_MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_ASSET_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportLimits {
    pub max_source_bytes: u64,
    pub max_units: usize,
    pub max_unit_text_bytes: usize,
    pub max_total_text_bytes: usize,
    /// Maximum retained immutable assets, including the original source.
    pub max_assets: usize,
    /// Maximum bytes for any one retained asset after extraction/decompression.
    pub max_asset_bytes: u64,
    /// Maximum aggregate bytes for retained assets after extraction/decompression.
    pub max_total_asset_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_units: DEFAULT_MAX_UNITS,
            max_unit_text_bytes: DEFAULT_MAX_UNIT_TEXT_BYTES,
            max_total_text_bytes: DEFAULT_MAX_TOTAL_TEXT_BYTES,
            max_assets: DEFAULT_MAX_ASSETS,
            max_asset_bytes: DEFAULT_MAX_ASSET_BYTES,
            max_total_asset_bytes: DEFAULT_MAX_TOTAL_ASSET_BYTES,
        }
    }
}
