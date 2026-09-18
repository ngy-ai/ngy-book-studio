//! The Palm database header, its record table and the field offsets.

use super::*;

/// `extra_record_flags`: the MOBI header field naming the trailing data regions
/// each text record carries.
///
/// Bits 15..1 each name a region whose size is a big-endian varint written at
/// the region's end; bit 0 names the multibyte overlap bytes, stripped last.
/// The field only exists in a header long enough to reach it (offset `0xF0`
/// needs 228 bytes), so a shorter header reports `None`.
pub(crate) fn read_extra_record_flags(record0: &[u8]) -> Option<u32> {
    let mobi_header_len = read_u32(record0, 20)? as usize;
    if mobi_header_len < EXTRA_RECORD_FLAGS_FIELD + 4 - 16 {
        return None;
    }
    read_u32(record0, EXTRA_RECORD_FLAGS_FIELD)
}

/// One-off layout dump for diagnosing an import that fails before any record is
/// decoded.
pub(crate) fn log_palmdb_layout(bytes: &[u8]) {
    let Some(palm) = PalmDb::parse(bytes).ok().flatten() else {
        tracing::warn!(target: "ngy_import", "Kindle 导入：无法解析 PalmDB 记录表");
        return;
    };
    let record0 = palm.record(0);
    tracing::info!(
        target: "ngy_import",
        records = palm.count(),
        mobi_version = ?mobi_version(bytes),
        compression = ?read_u16(record0, 0),
        text_length = ?read_u32(record0, 4),
        text_record_count = ?read_u16(record0, 8),
        text_record_size = ?read_u16(record0, 10),
        encryption = ?read_u16(record0, 12),
        mobi_header_len = ?read_u32(record0, 20),
        first_non_book_index = ?read_u32(record0, 80),
        first_image_index = ?read_u32(record0, 108),
        huff_index = ?read_u32(record0, HUFF_RECORD_INDEX_FIELD),
        huff_count = ?read_u32(record0, HUFF_RECORD_COUNT_FIELD),
        extra_record_flags = ?read_u32(record0, EXTRA_RECORD_FLAGS_FIELD),
        "Kindle 导入：PalmDB 布局"
    );
}

/// Record table of a PalmDB container: the record count from the file header
/// plus each record's start offset.
pub(crate) struct PalmDb<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) starts: Vec<usize>,
}

impl<'a> PalmDb<'a> {
    /// Returns `None` when the container has no readable record table, which
    /// the caller treats as "nothing to rewrite".
    pub(crate) fn parse(bytes: &'a [u8]) -> Result<Option<Self>> {
        if bytes.len() < 86 {
            return Ok(None);
        }
        let count = usize::from(u16::from_be_bytes([bytes[76], bytes[77]]));
        let table_end = 78_usize
            .checked_add(
                count
                    .checked_mul(8)
                    .context("PalmDB record table overflowed")?,
            )
            .context("PalmDB record table overflowed")?;
        if count == 0 || table_end > bytes.len() {
            return Ok(None);
        }
        let mut starts = Vec::with_capacity(count);
        for index in 0..count {
            let start = 78 + index * 8;
            let offset = u32::from_be_bytes([
                bytes[start],
                bytes[start + 1],
                bytes[start + 2],
                bytes[start + 3],
            ]) as usize;
            if offset < table_end || offset > bytes.len() {
                bail!("Kindle PalmDB record {index} offset is outside the source");
            }
            starts.push(offset);
        }
        if starts.windows(2).any(|pair| pair[0] >= pair[1]) {
            bail!("Kindle PalmDB record offsets are not strictly increasing");
        }
        Ok(Some(Self { bytes, starts }))
    }

    pub(crate) fn count(&self) -> usize {
        self.starts.len()
    }

    pub(crate) fn record(&self, index: usize) -> &'a [u8] {
        let start = self.starts[index];
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.bytes.len())
            .max(start);
        &self.bytes[start..end]
    }
}

pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]))
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
        *bytes.get(offset + 2)?,
        *bytes.get(offset + 3)?,
    ]))
}

/// MOBI header field `0x60`: record number of the `HUFF` table, relative to
/// the section's record 0.
pub(crate) const HUFF_RECORD_INDEX_FIELD: usize = 16 + 0x60;
/// MOBI header field `0x64`: number of `HUFF` + `CDIC` records.
pub(crate) const HUFF_RECORD_COUNT_FIELD: usize = 16 + 0x64;
/// MOBI header field `0xF0`: which trailing data regions each text record has.
pub(crate) const EXTRA_RECORD_FLAGS_FIELD: usize = 240;
/// MOBI header field `0x6C`: first record that holds a resource.
pub(crate) const FIRST_RESOURCE_INDEX_FIELD: usize = 108;
/// EXTH record 201: the cover's record offset from the first resource record.
pub(crate) const EXTH_COVER_OFFSET_TAG: u32 = 201;
/// How a KF8 container writes a reference to an embedded resource.
pub(crate) const KINDLE_EMBED_PREFIX: &str = "kindle:embed:";
/// How `ebook-rs` rewrites the `kindle:embed:` references it happens to match.
pub(crate) const KINDLE_LEGACY_IMAGE_PREFIX: &str = "images/img_";
/// Kindle numbers an embedded resource in base32, not decimal.
pub(crate) const BASE32_DIGITS: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
/// CSS at-rules that a stylesheet can legitimately start with.
pub(crate) const CSS_AT_RULES: &[&str] = &[
    "@charset",
    "@font-face",
    "@import",
    "@keyframes",
    "@media",
    "@namespace",
    "@page",
    "@supports",
];
/// How much of a dropped orphan rule reaches the log line.
pub(crate) const CSS_LOG_SNIPPET_CHARS: usize = 120;
