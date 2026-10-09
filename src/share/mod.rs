//! Share packages: one file carrying a book, its notes, its conversations and
//! its whole-book translations.
//!
//! A package is a ZIP container with a small JSON manifest plus one section per
//! data domain. It never carries object-store keys, native file paths or
//! database rows: the book itself travels as the canonical EPUB projection the
//! library already guarantees, and every note, citation and translation refers
//! to a content unit by its **position in the current document** rather than by
//! an identifier derived from the sender's import.
//!
//! That indirection is required, not cosmetic. Content-unit IDs are derived from
//! the book's own content hash and block IDs from the unit ID, so a receiver who
//! imports the shared EPUB gets a different set of IDs for the same text. The
//! receiving side therefore re-anchors each item against its own document and
//! drops what no longer matches instead of guessing (see
//! `crate::library::share`).

mod manifest;

mod package;

mod sections;

pub use manifest::{ShareBookInfo, ShareManifest, ShareSectionSummary, ShareTranslationSummary};

pub use package::{SharePackage, is_share_package, read_share_package, write_share_package};

pub use sections::{
    SharedChatCitation, SharedChatMessage, SharedChatThread, SharedNote, SharedTranslation,
    SharedUnitRef,
};

pub(crate) use sections::block_index_of;

/// Format identifier written into `manifest.json`.
pub const SHARE_FORMAT: &str = "ngy-share";

/// Current package format version. A package from any other version is refused
/// with the reason shown to the user instead of being imported halfway.
pub const SHARE_VERSION: u32 = 1;

/// Extension offered by the export dialog and the import filter.
pub const SHARE_EXTENSION: &str = "ngyshare";

pub(crate) const MANIFEST_ENTRY: &str = "manifest.json";

pub(crate) const BOOK_ENTRY: &str = "book.epub";

pub(crate) const NOTES_ENTRY: &str = "notes.json";

pub(crate) const CHATS_ENTRY: &str = "chats.json";

pub(crate) const TRANSLATIONS_ENTRY: &str = "translations.json";

/// Largest package this build reads or writes.
///
/// A package is read into memory whole and then parsed, so this is a peak-RAM
/// bound as much as a file-size one. Notes, conversations and译文 of one book
/// are tens of megabytes at most; two gigabytes leaves ample room while keeping
/// the worst case survivable on a normal machine.
pub(crate) const MAX_SHARE_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Largest single JSON section inside a package.
pub(crate) const MAX_SHARE_SECTION_BYTES: u64 = 64 * 1024 * 1024;

/// Largest book projection inside a package.
pub(crate) const MAX_SHARE_BOOK_BYTES: u64 = MAX_SHARE_PACKAGE_BYTES;

/// Entries a package may contain. The format defines five; the slack lets a
/// future revision add one without an older reader treating the file as hostile.
pub(crate) const MAX_SHARE_ENTRIES: usize = 16;

/// Largest content-unit count a manifest may declare.
pub(crate) const MAX_SHARE_UNITS: usize = 200_000;

/// Largest single quote or message body accepted from a package. The notes
/// table has the same ceiling for a quote, and a message without one would only
/// bloat the receiver's database.
pub(crate) const MAX_SHARE_QUOTE_BYTES: usize = 32 * 1024;

/// Largest stored translation payload accepted from a package.
pub(crate) const MAX_SHARE_TRANSLATION_BYTES: usize = 4 * 1024 * 1024;

#[cfg(test)]
mod tests;

/// The receiving library's translation engine, used to adopt the machine
/// translations that arrive inside a package.
///
/// Imported machine translations are hidden by the reader unless their `model`
/// and execution identity match the active translation settings, so a package
/// would otherwise only be visible to a friend who happens to run the exact same
/// endpoint and model. An import is an explicit, user-initiated adoption, so the
/// receiving side re-stamps the shared rows with its own engine instead of
/// pretending the text came from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareTranslationStamp {
    pub model: String,
    pub execution_identity: String,
    /// The receiving library's default译文 language, when one is configured.
    pub target_language: Option<String>,
}
