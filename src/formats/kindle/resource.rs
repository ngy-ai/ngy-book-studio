//! Collecting the images embedded in a Kindle container.

use super::*;

/// One retained resource record of the container.
pub(crate) struct KindleResource {
    /// One-based distance from the first resource record, which is the number
    /// a `kindle:embed:<base32>` reference and a MOBI `recindex` both name.
    pub(crate) number: usize,
    pub(crate) media_type: &'static str,
    pub(crate) extension: &'static str,
    pub(crate) is_cover: bool,
    pub(crate) bytes: Vec<u8>,
}

/// Reads the resource records that follow the text records.
///
/// A `kindle:embed:<base32>` reference names a record by its one-based distance
/// from MOBI header field `0x6C`, and that same counter is what a MOBI
/// `recindex` names — so every record after the text records takes a number,
/// whether it holds a picture or not. Keeping the numbering tied to the record
/// index is what makes the reference rewriting correct.
pub(crate) fn collect_kindle_resources(
    palm: &PalmDb<'_>,
    budget: &mut AssetBudget,
) -> Result<Vec<KindleResource>> {
    let record0 = palm.record(0);
    let first_record = first_resource_record(record0, palm.count());
    let cover_record =
        exth_cover_offset(record0).and_then(|offset| first_record.checked_add(offset as usize));
    let mut resources = Vec::new();
    for record in first_record..palm.count() {
        let payload = palm.record(record);
        let Some((media_type, extension)) = kindle_image_kind(payload) else {
            continue;
        };
        budget.add_usize(payload.len(), &format!("Kindle resource record {record}"))?;
        resources.push(KindleResource {
            number: record - first_record + 1,
            media_type,
            extension,
            is_cover: cover_record == Some(record),
            bytes: payload.to_vec(),
        });
    }
    tracing::info!(
        target: "ngy_import",
        first_resource_record = first_record,
        records = palm.count(),
        images = resources.len(),
        cover_record = ?cover_record,
        "Kindle 导入：已扫描 PalmDB 资源记录"
    );
    Ok(resources)
}

/// MOBI header field `0x6C`: the first record that holds a resource. Falls back
/// to the record after the text records when the field is absent or unusable.
pub(crate) fn first_resource_record(record0: &[u8], palm_records: usize) -> usize {
    let text_record_count = usize::from(read_u16(record0, 8).unwrap_or(0));
    read_u32(record0, FIRST_RESOURCE_INDEX_FIELD)
        .map(|index| index as usize)
        .filter(|index| *index > 0 && *index < palm_records)
        .unwrap_or_else(|| (1 + text_record_count).min(palm_records.saturating_sub(1)))
}

/// EXTH record 201: the cover's record offset from the first resource record.
pub(crate) fn exth_cover_offset(record0: &[u8]) -> Option<u32> {
    let header_len = read_u32(record0, 20)? as usize;
    let start = 16_usize.checked_add(header_len)?;
    if record0.get(start..start + 4)? != b"EXTH" {
        return None;
    }
    let count = read_u32(record0, start + 8)? as usize;
    let mut cursor = start + 12;
    for _ in 0..count {
        let tag = read_u32(record0, cursor)?;
        let length = read_u32(record0, cursor + 4)? as usize;
        if length < 8
            || cursor
                .checked_add(length)
                .is_none_or(|end| end > record0.len())
        {
            return None;
        }
        if tag == EXTH_COVER_OFFSET_TAG {
            // The payload is a 4-byte offset: a record that declares less than
            // 8 + 4 bytes cannot hold it. Without this check the read below
            // would pick up the following record's bytes as the cover offset.
            if length < 12 {
                return None;
            }
            let offset = read_u32(record0, cursor + 8)?;
            return (offset != u32::MAX).then_some(offset);
        }
        cursor += length;
    }
    None
}
