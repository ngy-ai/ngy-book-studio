//! The JSON manifest that identifies a share package and bounds its sections.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{MAX_SHARE_UNITS, SHARE_FORMAT, SHARE_VERSION};

/// What a package says about itself before any section is read. The counts are
/// what the export actually wrote, so a truncated or hand-edited section is
/// rejected instead of being imported partially.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareManifest {
    pub format: String,
    pub version: u32,
    pub created_at: u64,
    pub book: ShareBookInfo,
    pub notes: ShareSectionSummary,
    pub chats: ShareSectionSummary,
    pub translations: ShareTranslationSummary,
}

/// The sender's view of the shared book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareBookInfo {
    pub id: String,
    pub title: String,
    pub author: String,
    pub language: Option<String>,
    pub format: String,
    /// Content units in the exported document. Sections are mapped by this
    /// ordinal space, so the receiving side compares it against its own unit
    /// count and applies no section at all when the two disagree: a shifted
    /// ordinal would attach somebody's note to the wrong chapter.
    pub units: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareSectionSummary {
    pub count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareTranslationSummary {
    pub count: usize,
    /// The language the shared rows are written in. Import keeps them under the
    /// receiving library's own default language when one is configured.
    pub target_language: Option<String>,
}

impl ShareManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.format == SHARE_FORMAT,
            "这不是墨页分享包（格式标识为「{}」）",
            self.format
        );
        ensure!(
            self.version == SHARE_VERSION,
            "分享包版本 {} 不受本版本支持（当前支持 {}）",
            self.version,
            SHARE_VERSION
        );
        ensure!(!self.book.title.trim().is_empty(), "分享包缺少图书名称");
        ensure!(
            self.book.units <= MAX_SHARE_UNITS,
            "分享包声明的章节数超过上限"
        );
        ensure!(self.book.format.len() <= 32, "分享包中的图书格式标识无效");
        Ok(())
    }
}
