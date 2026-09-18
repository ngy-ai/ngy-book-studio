//! Matching a probe result and validating the OOXML archive against its
//! entry and expansion limits.

use super::*;

pub(crate) fn matched(format: BookFormat, detail: &str) -> ProbeResult {
    ProbeResult {
        format,
        confidence: ProbeConfidence::Container,
        detail: Some(detail.to_string()),
    }
}

pub(crate) fn zip_contains(bytes: &[u8], name: &str) -> bool {
    zip::ZipArchive::new(Cursor::new(bytes)).is_ok_and(|mut archive| archive.by_name(name).is_ok())
}

pub(crate) fn validate_ooxml_archive(bytes: &[u8], limits: &ImportLimits) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("Office OOXML archive is invalid")?;
    if archive.len() > MAX_OFFICE_ARCHIVE_ENTRIES {
        bail!("Office OOXML archive contains too many entries");
    }

    let mut total_uncompressed = 0_u64;
    let mut asset_budget =
        AssetBudget::with_original(limits, bytes.len(), "Office original source")?;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("cannot inspect Office OOXML archive entry {index}"))?;
        if entry.encrypted() {
            bail!("encrypted Office OOXML entries are not supported");
        }
        if entry.enclosed_name().is_none() {
            bail!("Office OOXML archive contains an unsafe entry path");
        }
        let uncompressed = entry.size();
        if uncompressed > MAX_OFFICE_ENTRY_UNCOMPRESSED_BYTES {
            bail!("Office OOXML archive entry exceeds the uncompressed size limit");
        }
        total_uncompressed = total_uncompressed
            .checked_add(uncompressed)
            .context("Office OOXML archive size overflowed")?;
        if total_uncompressed > MAX_OFFICE_ARCHIVE_UNCOMPRESSED_BYTES {
            bail!("Office OOXML archive exceeds the total uncompressed size limit");
        }
        if is_office_media_entry(entry.name()) {
            asset_budget.add(
                uncompressed,
                &format!("Office embedded media {}", entry.name()),
            )?;
        }
        if uncompressed >= MIN_COMPRESSION_RATIO_CHECK_BYTES
            && (entry.compressed_size() == 0
                || entry
                    .compressed_size()
                    .saturating_mul(MAX_OFFICE_COMPRESSION_RATIO)
                    < uncompressed)
        {
            bail!("Office OOXML archive entry has a suspicious compression ratio");
        }
    }
    Ok(())
}

pub(crate) fn is_office_media_entry(name: &str) -> bool {
    let name = name.trim_start_matches('/');
    ["word/media/", "ppt/media/", "xl/media/"]
        .iter()
        .any(|prefix| {
            name.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}
