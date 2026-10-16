//! The section payloads carried inside a share package.
//!
//! Every item locates its content unit by **ordinal** — the zero-based position
//! in the exported document. The sender's IDs are deliberately absent: a
//! receiver's own import derives different unit and block IDs from the same
//! text, so an ID from the sender would only be a promise this side cannot
//! keep. The only exception is a conversation citation, which may carry the
//! sender's own `locator_json` verbatim: when the receiver turns out to own the
//! very same original, its IDs *are* the sender's and the locator can be kept
//! as it is (see `library::share`).

use serde::{Deserialize, Serialize};

use crate::{annotations::AnnotationKind, document::ContentUnit};

/// Where an item lives in the sender's document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedUnitRef {
    pub ordinal: usize,
    /// The sender's chapter title. Not a mapping key and not a guard — the
    /// receiving text is what decides — but it is the only human-readable
    /// handle that survives the transfer, so the import log can name the
    /// chapter whose items were dropped.
    pub title: Option<String>,
}

impl SharedUnitRef {
    /// Builds the reference for one unit of an exported document.
    pub fn from_unit(ordinal: usize, unit: &ContentUnit) -> Self {
        let title = unit.title.trim();
        Self {
            ordinal,
            title: (!title.is_empty()).then(|| title.to_string()),
        }
    }
}

/// One note: the three exclusive marks and both kinds of thought share the notes
/// table, so they share a section here too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedNote {
    pub unit: SharedUnitRef,
    pub kind: String,
    pub quote: String,
    pub start: u32,
    pub end: u32,
    pub comment: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl SharedNote {
    pub fn kind(&self) -> Option<AnnotationKind> {
        AnnotationKind::parse(&self.kind)
    }
}

/// A citation travelling with a message. Web citations keep their URL and carry
/// no unit; book citations keep the quote and the chapter they came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedChatCitation {
    pub unit: Option<SharedUnitRef>,
    pub source_kind: String,
    pub quote: String,
    pub url: Option<String>,
    pub source_title: Option<String>,
    /// The sender's own [`DocumentLocator`](crate::document::DocumentLocator),
    /// serialized exactly as it was stored.
    ///
    /// It is only usable when the receiver owns the very same original — then
    /// the book id, unit ids and block ids all line up and the citation can
    /// keep pointing at the exact block. The receiving side revalidates it
    /// before use and falls back to a chapter-level locator, so a locator that
    /// does not describe the receiving document is never trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator_json: Option<String>,
}

/// One message of a shared conversation. `parent_ordinal` refers to a message in
/// the same thread by its ordinal, because the sender's message IDs are not
/// reproducible on the receiving side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedChatMessage {
    pub ordinal: usize,
    pub parent_ordinal: Option<usize>,
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub created_at: u64,
    pub citations: Vec<SharedChatCitation>,
}

/// One conversation of the shared book, without its sender-side identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedChatThread {
    pub title: String,
    pub window_kind: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<SharedChatMessage>,
}

/// One translated text block.
///
/// `block_index` is the sender's position of the block inside its unit
/// (`<unit>::h<index>`). The receiving side restores that shape against its own
/// unit so the reading window can still address the block; the text itself is
/// matched by `source_text`, which is what the reader pairs译文 with. A row is
/// only accepted where the receiving unit's own block at that position still
/// carries `source_text` — anything else would overwrite the receiver's译文 of
/// a different paragraph and could never be displayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedTranslation {
    pub unit: SharedUnitRef,
    pub block_index: usize,
    pub ordinal: usize,
    pub target_language: String,
    pub source_language: Option<String>,
    pub model: String,
    pub source_text: String,
    pub translated_text: String,
    pub manual_text: Option<String>,
}

/// Splits `<unit>::h<index>` back into its index.
pub(crate) fn block_index_of(block_id: &str, unit_id: &str) -> Option<usize> {
    block_id
        .strip_prefix(unit_id)?
        .strip_prefix("::h")?
        .parse()
        .ok()
}
