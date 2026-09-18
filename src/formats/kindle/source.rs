//! Preparing a Kindle source: language, container sniffing and the
//! Palmdoc/repack paths.

use super::*;

/// Rebuilds a PalmDB container into a layout `ebook-rs` accepts.
///
/// Two things `ebook-rs` cannot read on its own. HUFF/CDIC text compression
/// (`17480`), which `kindlegen -c2` and most Amazon-published KF8 files use, so
/// those records are decoded here and re-emitted uncompressed. And the trailing
/// data regions `extra_record_flags` declares, which `ebook-rs` concatenates
/// into the text instead of skipping. Record indices are preserved exactly in
/// both cases: the resource records keep their positions and `first_image_index`
/// in record 0 stays valid, so image extraction and `recindex=` rewriting are
/// unaffected.
/// Reads only the declared language from the container, normalizing no chapter.
/// It runs the same HUFF/CDIC rewrite as a real import so the startup backfill
/// sees exactly what `KindleImporter::import` would have recorded. A container
/// whose metadata table is empty reports `None`, and a file `ebook-rs` rejects
/// (encrypted or malformed) surfaces as an error the caller logs and skips.
pub(crate) fn declared_language(bytes: &[u8]) -> Result<Option<String>> {
    let prepared = prepare_kindle_source(bytes)?;
    let parsed = ebook_rs::MobiBook::parse(prepared.as_ref())
        .map_err(|error| anyhow::anyhow!(error).context("ebook-rs rejected the Kindle file"))?;
    Ok(parsed
        .metadata()
        .languages
        .first()
        .map(|language| language.trim().to_string())
        .filter(|language| !language.is_empty()))
}

pub(crate) fn prepare_kindle_source(bytes: &[u8]) -> Result<Cow<'_, [u8]>> {
    let Some(palm) = PalmDb::parse(bytes)? else {
        return Ok(Cow::Borrowed(bytes));
    };
    let record0 = palm.record(0);
    let huff_index = read_u32(record0, HUFF_RECORD_INDEX_FIELD);
    let huff_count = read_u32(record0, HUFF_RECORD_COUNT_FIELD);
    let compression = read_u16(record0, 0);
    if compression != Some(kindle_huff::COMPRESSION_HUFFCDIC) {
        return prepare_palmdoc_source(bytes, &palm);
    }

    let text_length = read_u32(record0, 4).context("Kindle PalmDOC header is truncated")? as usize;
    let text_record_count =
        usize::from(read_u16(record0, 8).context("Kindle PalmDOC header is truncated")?);
    let text_record_size =
        usize::from(read_u16(record0, 10).context("Kindle PalmDOC header is truncated")?);
    // The flags mark trailing data that sits outside the compressed stream, so
    // it is removed before decoding; those bits would otherwise decode into
    // stray symbols at the end of every record.
    let extra_record_flags = match read_extra_record_flags(record0) {
        Some(flags) => flags,
        None => {
            tracing::warn!(
                target: "ngy_import",
                mobi_header_len = ?read_u32(record0, 20),
                "Kindle 导入：MOBI 头部未声明尾部数据标志，按无尾部数据解压"
            );
            0
        }
    };
    tracing::info!(
        target: "ngy_import",
        compression = kindle_huff::COMPRESSION_HUFFCDIC,
        text_length,
        text_record_count,
        text_record_size,
        extra_record_flags = format_args!("{extra_record_flags:#x}"),
        huff_index,
        huff_count,
        "Kindle 导入：检测到 HUFF/CDIC 压缩，开始自行解压"
    );

    let (Some(huff_index), Some(huff_count)) = (huff_index, huff_count) else {
        bail!(
            "Kindle file declares HUFF/CDIC compression but its header has no HUFF record pointer"
        );
    };
    let huff_index = huff_index as usize;
    let huff_count = huff_count as usize;
    if huff_count < 2 || huff_index + huff_count > palm.count() {
        bail!(
            "Kindle file declares HUFF/CDIC compression but points at records \
             {huff_index}..{} of {}",
            huff_index + huff_count,
            palm.count()
        );
    }
    let huff_record = palm.record(huff_index);
    let cdic_records = (1..huff_count)
        .map(|offset| palm.record(huff_index + offset))
        .collect::<Vec<_>>();
    tracing::debug!(
        target: "ngy_import",
        huff_bytes = huff_record.len(),
        cdic_records = cdic_records.len(),
        "Kindle 导入：读取 HUFF/CDIC 表"
    );
    let mut model = kindle_huff::Huffcdic::load(huff_record, &cdic_records)?;
    tracing::info!(
        target: "ngy_import",
        phrases = model.phrase_count(),
        "Kindle 导入：HUFF/CDIC 词典加载完成"
    );

    let mut text = Vec::with_capacity(text_length);
    for index in 1..=text_record_count {
        if index >= palm.count() {
            bail!(
                "Kindle file declares {text_record_count} text records but the PalmDB is shorter"
            );
        }
        let record = palm.record(index);
        let body_len = kindle_huff::text_body_len(record, extra_record_flags);
        let decoded = model
            .decompress(&record[..body_len])
            .with_context(|| format!("failed to decompress Kindle text record {index}"))?;
        tracing::debug!(
            target: "ngy_import",
            record = index,
            compressed_bytes = record.len(),
            body_bytes = body_len,
            trailing_bytes = record.len() - body_len,
            decoded_bytes = decoded.len(),
            "Kindle 导入：文本记录已解压"
        );
        text.extend_from_slice(&decoded);
    }
    let decoded_total = text.len();
    // The bit padding that aligns each record's compressed stream decodes into
    // a few stray symbols at the very end of the last record. The PalmDOC
    // header's `text_length` is the exact size of the real text.
    text.truncate(text_length);
    tracing::info!(
        target: "ngy_import",
        decoded_bytes = decoded_total,
        text_length,
        padding_bytes = decoded_total.saturating_sub(text_length),
        "Kindle 导入：HUFF/CDIC 解压完成，按头部声明长度截断"
    );
    if text.is_empty() {
        bail!("Kindle file's HUFF/CDIC text records decoded to nothing");
    }

    // Spread the text over the text-record slots the header already declares so
    // no later record index moves. A well-formed file satisfies
    // `text_length <= text_record_count * text_record_size`, so every chunk
    // fits; a file that does not is rejected rather than silently truncated.
    let chunk = text.len().div_ceil(text_record_count).max(1);
    if chunk > text_record_size {
        bail!(
            "Kindle HUFF/CDIC text needs {chunk} byte records but the header declares \
             {text_record_size}"
        );
    }
    let text_payloads = (0..text_record_count)
        .map(|index| {
            let start = (index * chunk).min(text.len());
            let end = (start + chunk).min(text.len());
            text[start..end].to_vec()
        })
        .collect::<Vec<_>>();
    let mut record0_out = record0.to_vec();
    // Uncompressed: `ebook-rs` concatenates the text records verbatim.
    record0_out[0..2].copy_from_slice(&1_u16.to_be_bytes());
    let out = repack_palm_db(&palm, &record0_out, text_payloads);
    tracing::info!(
        target: "ngy_import",
        source_bytes = bytes.len(),
        rewritten_bytes = out.len(),
        records = palm.count(),
        text_records = text_record_count,
        bytes_per_record = chunk,
        "Kindle 导入：已重写为未压缩容器，交给 ebook-rs 解析"
    );
    Ok(Cow::Owned(out))
}

/// Assembles a PalmDB from a patched record 0 plus replacement text records.
///
/// Everything the original kept after its text records — resources, the
/// FLIS/FCIS pair, the HUFF/CDIC tables, a packed source archive — is copied
/// verbatim at the same index, so `first_image_index` and the `recindex=`
/// rewriting stay valid.
pub(crate) fn repack_palm_db(
    palm: &PalmDb<'_>,
    record0: &[u8],
    text_payloads: Vec<Vec<u8>>,
) -> Vec<u8> {
    let text_record_count = text_payloads.len();
    let mut record_payloads = Vec::with_capacity(palm.count());
    record_payloads.push(record0.to_vec());
    record_payloads.extend(text_payloads);
    for index in (1 + text_record_count)..palm.count() {
        record_payloads.push(palm.record(index).to_vec());
    }
    debug_assert_eq!(record_payloads.len(), palm.count());

    let payload_bytes = record_payloads.iter().map(Vec::len).sum::<usize>();
    let mut out = Vec::with_capacity(78 + palm.count() * 8 + payload_bytes);
    out.extend_from_slice(&palm.bytes[..78]);
    // The record count in the file header is unchanged; only the offsets move
    // because the text records now hold different payloads.
    let mut offset = 78 + palm.count() * 8;
    for payload in &record_payloads {
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        offset += payload.len();
    }
    for payload in &record_payloads {
        out.extend_from_slice(payload);
    }
    out
}

/// Trims the trailing data regions `extra_record_flags` declares off the
/// still-compressed PalmDOC text records.
///
/// `ebook-rs` concatenates every text record verbatim and only then tries
/// UTF-8, falling back to WINDOWS-1252 when that fails. A trailing region makes
/// the run invalid UTF-8, so a CJK book decodes byte-for-byte as CP1252 and the
/// reader shows `ä½œè€…ç®€ä»‹` where `作者简介` belongs. The regions sit outside the
/// compressed stream, so trimming them before `ebook-rs` decodes is lossless
/// and leaves every PalmDOC back-reference intact.
pub(crate) fn prepare_palmdoc_source<'a>(
    bytes: &'a [u8],
    palm: &PalmDb<'a>,
) -> Result<Cow<'a, [u8]>> {
    let record0 = palm.record(0);
    let compression = read_u16(record0, 0);
    let text_record_count =
        usize::from(read_u16(record0, 8).context("Kindle PalmDOC header is truncated")?);
    let Some(flags) = read_extra_record_flags(record0).filter(|flags| *flags != 0) else {
        tracing::info!(
            target: "ngy_import",
            ?compression,
            records = palm.count(),
            "Kindle 导入：正文为 PalmDOC 直读压缩且无尾部数据区，交给 ebook-rs 解析"
        );
        return Ok(Cow::Borrowed(bytes));
    };
    if text_record_count == 0 || 1 + text_record_count > palm.count() {
        bail!("Kindle file declares {text_record_count} text records but the PalmDB is shorter");
    }
    let mut text_payloads = Vec::with_capacity(text_record_count);
    let mut trailing_bytes = 0_usize;
    for index in 1..=text_record_count {
        let record = palm.record(index);
        let body_len = kindle_huff::text_body_len(record, flags);
        if body_len == 0 {
            bail!("Kindle text record {index} holds nothing but its trailing data region");
        }
        trailing_bytes += record.len() - body_len;
        text_payloads.push(record[..body_len].to_vec());
    }
    tracing::info!(
        target: "ngy_import",
        extra_record_flags = format_args!("{flags:#x}"),
        text_record_count,
        trailing_bytes,
        records = palm.count(),
        "Kindle 导入：已剥掉正文记录的尾部数据区，交给 ebook-rs 解析"
    );
    Ok(Cow::Owned(repack_palm_db(palm, record0, text_payloads)))
}
