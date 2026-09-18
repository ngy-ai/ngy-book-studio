//! Reading the MOBI header and the expansion limits it declares.

use super::*;

pub(crate) fn is_mobi_container(bytes: &[u8]) -> bool {
    if bytes.len() < 68 {
        return false;
    }
    let marker = &bytes[60..68];
    marker == b"BOOKMOBI"
        || marker == b"TEXtRECD"
        || &bytes[60..64] == b"BOOK"
        || &bytes[64..68] == b"MOBI"
}

pub(crate) fn mobi_version(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 78 {
        return None;
    }
    let records = u16::from_be_bytes([bytes[76], bytes[77]]) as usize;
    if records == 0 || bytes.len() < 86 {
        return None;
    }
    let offset = u32::from_be_bytes([bytes[78], bytes[79], bytes[80], bytes[81]]) as usize;
    let end = offset.checked_add(40)?;
    if end > bytes.len() || &bytes[offset + 16..offset + 20] != b"MOBI" {
        return None;
    }
    Some(u32::from_be_bytes([
        bytes[offset + 36],
        bytes[offset + 37],
        bytes[offset + 38],
        bytes[offset + 39],
    ]))
}

pub(crate) fn validate_mobi_header(bytes: &[u8], limits: &ImportLimits) -> Result<()> {
    if bytes.len() < 86 {
        bail!("Kindle PalmDB header is truncated");
    }
    let palm_record_count = u16::from_be_bytes([bytes[76], bytes[77]]) as usize;
    if palm_record_count == 0 {
        bail!("Kindle PalmDB contains no records");
    }
    let record_table_end = 78_usize
        .checked_add(
            palm_record_count
                .checked_mul(8)
                .context("Kindle PalmDB record table overflowed")?,
        )
        .context("Kindle PalmDB record table overflowed")?;
    if record_table_end > bytes.len() {
        bail!("Kindle PalmDB record table is truncated");
    }

    let mut record_offsets = Vec::with_capacity(palm_record_count);
    for index in 0..palm_record_count {
        let start = 78 + index * 8;
        let offset = u32::from_be_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]) as usize;
        if offset < record_table_end || offset >= bytes.len() {
            bail!("Kindle PalmDB record offset is outside the source");
        }
        if record_offsets
            .last()
            .is_some_and(|previous| offset <= *previous)
        {
            bail!("Kindle PalmDB record offsets are not strictly increasing");
        }
        record_offsets.push(offset);
    }

    let record_offset = record_offsets[0];
    let record_end = record_offsets.get(1).copied().unwrap_or(bytes.len());
    let palmdoc_end = record_offset
        .checked_add(24)
        .context("Kindle record offset overflowed")?;
    if palmdoc_end > record_end {
        bail!("Kindle PalmDOC header is truncated");
    }
    let compression = u16::from_be_bytes([bytes[record_offset], bytes[record_offset + 1]]);
    if !matches!(compression, 1 | 2 | 17_480) {
        bail!("Kindle PalmDOC compression method is invalid");
    }
    let text_length = u32::from_be_bytes([
        bytes[record_offset + 4],
        bytes[record_offset + 5],
        bytes[record_offset + 6],
        bytes[record_offset + 7],
    ]);
    let text_record_count =
        u16::from_be_bytes([bytes[record_offset + 8], bytes[record_offset + 9]]);
    let text_record_size =
        u16::from_be_bytes([bytes[record_offset + 10], bytes[record_offset + 11]]);
    if text_length == 0 || text_record_count == 0 || text_record_size == 0 {
        bail!("Kindle PalmDOC text record metadata is invalid");
    }
    if u64::from(text_length) > u64::try_from(limits.max_total_text_bytes).unwrap_or(u64::MAX) {
        bail!("Kindle decompressed text exceeds the total text safety limit");
    }
    if usize::from(text_record_count) >= palm_record_count {
        bail!("Kindle PalmDOC text record count exceeds the PalmDB record table");
    }
    let mut possible_text_bytes = 0_u64;
    for record_index in 1..=usize::from(text_record_count) {
        let start = record_offsets[record_index];
        let end = record_offsets
            .get(record_index + 1)
            .copied()
            .unwrap_or(bytes.len());
        let record_bytes = u64::try_from(end - start).context("Kindle text record is too large")?;
        let possible_record_bytes = if compression == 2 {
            // A two-byte PalmDOC back-reference emits at most ten bytes.
            record_bytes
                .checked_mul(5)
                .context("Kindle decompressed text size overflowed")?
        } else {
            record_bytes
        };
        possible_text_bytes = possible_text_bytes
            .checked_add(possible_record_bytes)
            .context("Kindle decompressed text size overflowed")?;
        if possible_text_bytes > u64::try_from(limits.max_total_text_bytes).unwrap_or(u64::MAX) {
            bail!("Kindle text records can exceed the total text safety limit");
        }
    }
    let encryption = u16::from_be_bytes([bytes[record_offset + 12], bytes[record_offset + 13]]);
    if encryption != 0 {
        bail!("encrypted Kindle files are not supported");
    }
    if &bytes[record_offset + 16..record_offset + 20] != b"MOBI" {
        bail!("Kindle record does not contain a MOBI header");
    }
    let mobi_header_length = u32::from_be_bytes([
        bytes[record_offset + 20],
        bytes[record_offset + 21],
        bytes[record_offset + 22],
        bytes[record_offset + 23],
    ]) as usize;
    if mobi_header_length < 116
        || record_offset
            .checked_add(16)
            .and_then(|offset| offset.checked_add(mobi_header_length))
            .is_none_or(|end| end > record_end)
    {
        bail!("Kindle MOBI header length is invalid");
    }

    let first_image_record = u32::from_be_bytes([
        bytes[record_offset + 108],
        bytes[record_offset + 109],
        bytes[record_offset + 110],
        bytes[record_offset + 111],
    ]) as usize;
    let first_image_record = if first_image_record > 0 && first_image_record < palm_record_count {
        first_image_record
    } else {
        1 + usize::from(text_record_count)
    };
    let mut asset_budget =
        AssetBudget::with_original(limits, bytes.len(), "Kindle original source")?;
    for record_index in first_image_record.max(1)..palm_record_count {
        let start = record_offsets[record_index];
        let end = record_offsets
            .get(record_index + 1)
            .copied()
            .unwrap_or(bytes.len());
        let record = &bytes[start..end];
        if is_kindle_image_record(record) {
            asset_budget.add_usize(record.len(), &format!("Kindle image record {record_index}"))?;
        }
    }
    Ok(())
}

pub(crate) fn is_kindle_image_record(bytes: &[u8]) -> bool {
    kindle_image_kind(bytes).is_some()
}

/// Media type and file extension of an image record, or `None` when the record
/// holds anything else.
pub(crate) fn kindle_image_kind(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else if bytes.starts_with(b"BM") {
        Some(("image/bmp", "bmp"))
    } else {
        None
    }
}
