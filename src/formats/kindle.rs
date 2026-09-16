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

#[derive(Clone, Copy, Debug, Default)]
pub struct KindleImporter;

const KINDLE_FORMATS: &[BookFormat] = &[BookFormat::Mobi, BookFormat::Azw, BookFormat::Azw3];

impl DocumentImporter for KindleImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "ebook-rs",
            formats: KINDLE_FORMATS,
            parser_version: "0.16.4",
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        let extension = source.extension();
        let format = kindle_format(source.bytes.as_slice(), extension.as_deref());
        if is_mobi_container(source.bytes.as_slice()) {
            ProbeResult {
                format,
                confidence: ProbeConfidence::Magic,
                detail: Some(match mobi_version(source.bytes.as_slice()) {
                    Some(version) => format!("PalmDB MOBI/KF{version} container"),
                    None => "PalmDB MOBI container".to_string(),
                }),
            }
        } else if matches!(extension.as_deref(), Some("mobi" | "azw" | "azw3")) {
            ProbeResult {
                format,
                confidence: ProbeConfidence::Extension,
                detail: Some("Kindle extension without a MOBI container signature".to_string()),
            }
        } else {
            ProbeResult::no_match(BookFormat::Mobi)
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        let bytes = source.bytes.as_slice();
        if !is_mobi_container(bytes) {
            bail!("source is not a MOBI/AZW PalmDB container");
        }
        let mut asset_budget =
            AssetBudget::with_original(limits, bytes.len(), "Kindle original source")?;
        validate_mobi_header(bytes, limits)?;
        let format = kindle_format(bytes, source.extension().as_deref());
        tracing::info!(
            target: "ngy_import",
            file_name = ?source.file_name,
            extension = ?source.extension(),
            bytes = bytes.len(),
            ?format,
            "Kindle 导入：PalmDB 容器与 MOBI 头部校验通过"
        );
        log_palmdb_layout(bytes);

        // `ebook-rs` rejects HUFF/CDIC text outright, so those containers are
        // decoded here and re-emitted uncompressed. Everything else is handed
        // over untouched.
        let prepared = prepare_kindle_source(bytes)?;
        let parsed = ebook_rs::MobiBook::parse(prepared.as_ref()).map_err(|error| {
            anyhow::anyhow!(error).context(
                "ebook-rs rejected the Kindle file; encrypted Kindle books are not supported",
            )
        })?;
        tracing::info!(
            target: "ngy_import",
            sections = parsed.sections().len(),
            manifest_entries = parsed.manifest().len(),
            toc_points = flatten_nav(parsed.toc()).len(),
            title = ?parsed.metadata().title,
            "Kindle 导入：正文解析完成"
        );
        if parsed.sections().is_empty() {
            bail!("Kindle file does not contain any readable sections");
        }
        if parsed.sections().len() > limits.max_units {
            bail!("Kindle document contains too many sections");
        }

        let media_type = match format {
            BookFormat::Azw3 => "application/vnd.amazon.mobi8-ebook",
            BookFormat::Azw => "application/vnd.amazon.ebook",
            _ => "application/x-mobipocket-ebook",
        };
        let original = original_asset(source, format, media_type);
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        let metadata = parsed.metadata();
        let title = safe_title(Some(&metadata.title), source.stem().as_str());

        // `ebook-rs` reports neither a manifest nor a cover for a PalmDB book:
        // `MobiBook::parse` leaves `opf.manifest` empty and never fills in
        // `cover_href`. Every `kindle:embed:` reference therefore has nothing to
        // resolve to, and the shared rewrite pass drops the attribute — the
        // imported book ends up with no pictures at all. The resource records
        // are read from the container directly instead.
        let resources = match PalmDb::parse(bytes)? {
            Some(palm) => collect_kindle_resources(&palm, &mut asset_budget)?,
            None => Vec::new(),
        };
        let mut assets = vec![original.clone()];
        let mut asset_ids_by_href = HashMap::new();
        let mut resource_paths = HashMap::new();
        let mut cover_asset_id = None;
        for resource in &resources {
            let path = format!(
                "kindle-resource/{:04}.{}",
                resource.number, resource.extension
            );
            let roles = if resource.is_cover {
                vec![AssetRole::Cover, AssetRole::ContentImage]
            } else {
                vec![AssetRole::ContentImage]
            };
            let asset = asset_from_bytes(
                &format!("{book_id}\0kindle-resource\0{}", resource.number),
                roles,
                resource.media_type,
                Some(path.clone()),
                Arc::new(resource.bytes.clone()),
            );
            if resource.is_cover {
                cover_asset_id = Some(asset.metadata.id.clone());
            }
            if let Some(key) = normalized_archive_href(&path) {
                asset_ids_by_href.insert(key, asset.metadata.id.clone());
            }
            resource_paths.insert(resource.number, path);
            assets.push(asset);
        }
        tracing::info!(
            target: "ngy_import",
            retained = resources.len(),
            cover = cover_asset_id.is_some(),
            "Kindle 导入：资源记录处理完成"
        );

        let chapters = kindle_chapters(parsed.sections());
        let mut units = Vec::with_capacity(chapters.len());
        let mut toc = Vec::with_capacity(chapters.len());
        for (index, chapter) in chapters.iter().enumerate() {
            tracing::debug!(
                target: "ngy_import",
                chapter = index + 1,
                href = %chapter.href,
                raw_html_bytes = chapter.html.len(),
                "Kindle 导入：正在规范化章节"
            );
            if chapter.html.len() > limits.max_unit_text_bytes {
                bail!("Kindle chapter {} exceeds the text safety limit", index + 1);
            }
            let unit_id = deterministic_id(
                "unit",
                format!("{book_id}\0kindle\0{}\0{}", index + 1, chapter.href).as_bytes(),
            );
            let chapter_title = safe_title(Some(&chapter.title), &format!("Chapter {}", index + 1));
            let rewritten = rewrite_kindle_media_references(&chapter.html, &resource_paths);
            let rewritten = rewrite_imported_html_assets(
                &strip_orphan_css(&rewritten),
                &chapter.href,
                &asset_ids_by_href,
            )?;
            let normalized = crate::markup::parse_source_for_unit(&rewritten, &unit_id)
                .with_context(|| format!("failed to normalize Kindle chapter {}", index + 1))?;
            units.push(
                ContentUnit::new(
                    unit_id.clone(),
                    ContentUnitKind::Chapter,
                    chapter_title.clone(),
                    normalized.canonical_source,
                    normalized.document,
                )
                .with_source_locator(SourceLocator::kindle_section(
                    u32::try_from(index + 1).unwrap_or(u32::MAX),
                    Some(chapter.href.clone()),
                )),
            );
            toc.push(TocNode::new(
                deterministic_id(
                    "toc",
                    format!("{book_id}\0kindle-chapter\0{index}").as_bytes(),
                ),
                chapter_title,
                TocTarget::unit(unit_id),
            ));
        }

        let mut document = BookDocument::new(
            book_id,
            title,
            BookSource::imported(
                format,
                original.metadata.id.clone(),
                source.file_name.clone(),
            ),
        );
        document.authors = metadata
            .creators
            .iter()
            .map(|author| author.trim().to_string())
            .filter(|author| !author.is_empty())
            .collect();
        document.language = metadata
            .languages
            .first()
            .map(|language| language.trim().to_string())
            .filter(|language| !language.is_empty());
        document.description = metadata.description.clone();
        document.units = units;
        document.toc = toc;
        document.cover_asset_id = cover_asset_id;
        document.assets = assets.iter().map(|asset| asset.metadata.clone()).collect();
        tracing::info!(
            target: "ngy_import",
            book_id = %document.id,
            title = %document.title,
            authors = ?document.authors,
            language = ?document.language,
            units = document.units.len(),
            toc_nodes = document.toc.len(),
            assets = document.assets.len(),
            cover = document.cover_asset_id.is_some(),
            "Kindle 导入：统一模型构建完成"
        );
        Ok(ImportedBook { document, assets })
    }
}

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

fn prepare_kindle_source(bytes: &[u8]) -> Result<Cow<'_, [u8]>> {
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
fn repack_palm_db(palm: &PalmDb<'_>, record0: &[u8], text_payloads: Vec<Vec<u8>>) -> Vec<u8> {
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
fn prepare_palmdoc_source<'a>(bytes: &'a [u8], palm: &PalmDb<'a>) -> Result<Cow<'a, [u8]>> {
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

/// `extra_record_flags`: the MOBI header field naming the trailing data regions
/// each text record carries.
///
/// Bits 15..1 each name a region whose size is a big-endian varint written at
/// the region's end; bit 0 names the multibyte overlap bytes, stripped last.
/// The field only exists in a header long enough to reach it (offset `0xF0`
/// needs 228 bytes), so a shorter header reports `None`.
fn read_extra_record_flags(record0: &[u8]) -> Option<u32> {
    let mobi_header_len = read_u32(record0, 20)? as usize;
    if mobi_header_len < EXTRA_RECORD_FLAGS_FIELD + 4 - 16 {
        return None;
    }
    read_u32(record0, EXTRA_RECORD_FLAGS_FIELD)
}

/// One-off layout dump for diagnosing an import that fails before any record is
/// decoded.
fn log_palmdb_layout(bytes: &[u8]) {
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
struct PalmDb<'a> {
    bytes: &'a [u8],
    starts: Vec<usize>,
}

impl<'a> PalmDb<'a> {
    /// Returns `None` when the container has no readable record table, which
    /// the caller treats as "nothing to rewrite".
    fn parse(bytes: &'a [u8]) -> Result<Option<Self>> {
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

    fn count(&self) -> usize {
        self.starts.len()
    }

    fn record(&self, index: usize) -> &'a [u8] {
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

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
        *bytes.get(offset + 2)?,
        *bytes.get(offset + 3)?,
    ]))
}

/// MOBI header field `0x60`: record number of the `HUFF` table, relative to
/// the section's record 0.
const HUFF_RECORD_INDEX_FIELD: usize = 16 + 0x60;
/// MOBI header field `0x64`: number of `HUFF` + `CDIC` records.
const HUFF_RECORD_COUNT_FIELD: usize = 16 + 0x64;
/// MOBI header field `0xF0`: which trailing data regions each text record has.
const EXTRA_RECORD_FLAGS_FIELD: usize = 240;
/// MOBI header field `0x6C`: first record that holds a resource.
const FIRST_RESOURCE_INDEX_FIELD: usize = 108;
/// EXTH record 201: the cover's record offset from the first resource record.
const EXTH_COVER_OFFSET_TAG: u32 = 201;
/// How a KF8 container writes a reference to an embedded resource.
const KINDLE_EMBED_PREFIX: &str = "kindle:embed:";
/// How `ebook-rs` rewrites the `kindle:embed:` references it happens to match.
const KINDLE_LEGACY_IMAGE_PREFIX: &str = "images/img_";
/// Kindle numbers an embedded resource in base32, not decimal.
const BASE32_DIGITS: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
/// CSS at-rules that a stylesheet can legitimately start with.
const CSS_AT_RULES: &[&str] = &[
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
const CSS_LOG_SNIPPET_CHARS: usize = 120;

fn is_mobi_container(bytes: &[u8]) -> bool {
    if bytes.len() < 68 {
        return false;
    }
    let marker = &bytes[60..68];
    marker == b"BOOKMOBI"
        || marker == b"TEXtRECD"
        || &bytes[60..64] == b"BOOK"
        || &bytes[64..68] == b"MOBI"
}

fn mobi_version(bytes: &[u8]) -> Option<u32> {
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

fn validate_mobi_header(bytes: &[u8], limits: &ImportLimits) -> Result<()> {
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

fn is_kindle_image_record(bytes: &[u8]) -> bool {
    kindle_image_kind(bytes).is_some()
}

/// Media type and file extension of an image record, or `None` when the record
/// holds anything else.
fn kindle_image_kind(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
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

/// One retained resource record of the container.
struct KindleResource {
    /// One-based distance from the first resource record, which is the number
    /// a `kindle:embed:<base32>` reference and a MOBI `recindex` both name.
    number: usize,
    media_type: &'static str,
    extension: &'static str,
    is_cover: bool,
    bytes: Vec<u8>,
}

/// Reads the resource records that follow the text records.
///
/// A `kindle:embed:<base32>` reference names a record by its one-based distance
/// from MOBI header field `0x6C`, and that same counter is what a MOBI
/// `recindex` names — so every record after the text records takes a number,
/// whether it holds a picture or not. Keeping the numbering tied to the record
/// index is what makes the reference rewriting correct.
fn collect_kindle_resources(
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
fn first_resource_record(record0: &[u8], palm_records: usize) -> usize {
    let text_record_count = usize::from(read_u16(record0, 8).unwrap_or(0));
    read_u32(record0, FIRST_RESOURCE_INDEX_FIELD)
        .map(|index| index as usize)
        .filter(|index| *index > 0 && *index < palm_records)
        .unwrap_or_else(|| (1 + text_record_count).min(palm_records.saturating_sub(1)))
}

/// EXTH record 201: the cover's record offset from the first resource record.
fn exth_cover_offset(record0: &[u8]) -> Option<u32> {
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
            let offset = read_u32(record0, cursor + 8)?;
            return (offset != u32::MAX).then_some(offset);
        }
        cursor += length;
    }
    None
}

/// Points every Kindle media reference at the retained resource.
///
/// Two spellings reach this point: the `kindle:embed:<base32>` a KF8 container
/// writes, and the `images/img_NNNN.ext` form `ebook-rs` substitutes for the
/// references its own decimal, zero-padded search happens to match. Both name
/// the same record counter, so both are mapped back onto it.
fn rewrite_kindle_media_references(html: &str, paths: &HashMap<usize, String>) -> String {
    let html = rewrite_kindle_embed_references(html, paths);
    rewrite_kindle_legacy_references(&html, paths)
}

fn rewrite_kindle_embed_references(html: &str, paths: &HashMap<usize, String>) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find(KINDLE_EMBED_PREFIX) {
        let start = cursor + offset;
        let tail = &html[start + KINDLE_EMBED_PREFIX.len()..];
        let end = tail
            .find(|character: char| {
                !character.is_ascii()
                    || !BASE32_DIGITS.contains(&(character.to_ascii_uppercase() as u8))
            })
            .unwrap_or(tail.len());
        output.push_str(&html[cursor..start]);
        match resource_number_from_base32(&tail[..end]).and_then(|number| paths.get(&number)) {
            Some(path) => {
                output.push_str(path);
                // `?mime=image/jpg` is left over from the original Kindle URL
                // and is meaningless once the reference names a retained asset.
                cursor = start + KINDLE_EMBED_PREFIX.len() + end + mime_query_len(&tail[end..]);
            }
            None => {
                output.push_str(KINDLE_EMBED_PREFIX);
                output.push_str(&tail[..end]);
                cursor = start + KINDLE_EMBED_PREFIX.len() + end;
            }
        }
    }
    output.push_str(&html[cursor..]);
    output
}

fn rewrite_kindle_legacy_references(html: &str, paths: &HashMap<usize, String>) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find(KINDLE_LEGACY_IMAGE_PREFIX) {
        let start = cursor + offset;
        let tail = &html[start + KINDLE_LEGACY_IMAGE_PREFIX.len()..];
        let digits_end = tail
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(tail.len());
        let Some(path) = tail[..digits_end]
            .parse::<usize>()
            .ok()
            .and_then(|number| paths.get(&number))
        else {
            output.push_str(&html[cursor..start + KINDLE_LEGACY_IMAGE_PREFIX.len()]);
            cursor = start + KINDLE_LEGACY_IMAGE_PREFIX.len();
            continue;
        };
        let extension_start = digits_end + usize::from(tail[digits_end..].starts_with('.'));
        let extension_len = tail[extension_start..]
            .find(|character: char| !character.is_ascii_alphanumeric())
            .unwrap_or(tail.len() - extension_start);
        let consumed = extension_start + extension_len;
        output.push_str(&html[cursor..start]);
        output.push_str(path);
        let after = &tail[consumed..];
        cursor = start + KINDLE_LEGACY_IMAGE_PREFIX.len() + consumed + mime_query_len(after);
    }
    output.push_str(&html[cursor..]);
    output
}

fn mime_query_len(value: &str) -> usize {
    let Some(query) = value.strip_prefix("?mime=") else {
        return 0;
    };
    let end = query
        .find(|character: char| {
            !character.is_ascii_alphanumeric()
                && character != '/'
                && character != '-'
                && character != '+'
        })
        .unwrap_or(query.len());
    "?mime=".len() + end
}

fn resource_number_from_base32(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let mut value = 0_usize;
    for byte in text.bytes() {
        let digit = BASE32_DIGITS
            .iter()
            .position(|candidate| *candidate == byte.to_ascii_uppercase())?;
        value = value.checked_mul(32)?.checked_add(digit)?;
    }
    Some(value)
}

/// Drops CSS that reached the text stream without its `<style>` wrapper.
///
/// A KF8 container keeps each stylesheet in a *flow* record, and the merged text
/// `ebook-rs` assembles ends with that flow appended verbatim. The flow carries
/// no tags, so by then its rules are ordinary text and would be rendered as body
/// copy. This book ends with `@page Section1 { … }` and
/// `div.Section1 { page:Section1 }`.
///
/// The flow lands after the markup that precedes it, so the run that follows a
/// closing tag is the only run the heuristic may touch. A run opened by an
/// element — `<p>@media (max-width: 600px) { … }</p>` in a CSS tutorial — is
/// body copy and is left alone.
fn strip_orphan_css(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while cursor < html.len() {
        let Some(offset) = html[cursor..].find('>') else {
            break;
        };
        let text_start = cursor + offset + 1;
        let text_end = html[text_start..]
            .find('<')
            .map(|end| text_start + end)
            .unwrap_or(html.len());
        output.push_str(&html[cursor..text_start]);
        let text = &html[text_start..text_end];
        if is_orphan_css_rule(text) && !opens_element(&html[cursor..text_start]) {
            tracing::debug!(
                target: "ngy_import",
                bytes = text.len(),
                snippet = %log_snippet(text, CSS_LOG_SNIPPET_CHARS),
                "Kindle 导入：删除游离样式表文本"
            );
        } else {
            output.push_str(text);
        }
        cursor = text_end;
    }
    output.push_str(&html[cursor..]);
    output
}

/// A whole text run that is nothing but CSS at-rules.
fn is_orphan_css_rule(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.ends_with('}')
        && trimmed.contains('{')
        && CSS_AT_RULES.iter().any(|rule| trimmed.starts_with(rule))
}

/// Whether `markup` — everything up to and including the tag that ends right
/// before a text run — ends with an opening tag, which makes that run element
/// content rather than a stylesheet flow.
fn opens_element(markup: &str) -> bool {
    let Some(open) = markup.rfind('<') else {
        return false;
    };
    markup.as_bytes()[open + 1].is_ascii_alphabetic()
}

/// First `limit` characters of a log snippet, so a stray rule cannot flood the
/// log file.
fn log_snippet(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &trimmed[..end]),
        None => trimmed.to_string(),
    }
}

/// One chapter of the rebuilt book.
struct KindleChapter {
    /// Title taken from the chapter's level-one heading, empty when the chapter
    /// opens without one.
    title: String,
    /// Href of the first section that fed this chapter.
    href: String,
    html: String,
}

/// Regroups the sections `ebook-rs` produced into the chapters the book has.
///
/// `ebook-rs` does not read a MOBI table of contents: it splits the merged
/// markup at every `<h1>`/`<h2>`/`<h3>` and then labels each piece
/// `Section <n>`, which for a converted document turns a chapter into hundreds
/// of pieces, each displayed under a meaningless title. The real chapter list
/// lives on the level-one headings — a converted book writes one `<h1>` per
/// chapter, and the `_Toc…` bookmarks its navigation points at sit inside them —
/// so only a level-one heading that carries text opens a new chapter, and the
/// rest of the headings stay with the chapter they belong to.
fn kindle_chapters(sections: &[ebook_rs::Section]) -> Vec<KindleChapter> {
    let mut chapters: Vec<KindleChapter> = Vec::new();
    for section in sections {
        if let Some(title) = leading_heading(&section.raw_html, "h1") {
            chapters.push(KindleChapter {
                title,
                href: section.href.clone(),
                html: section.raw_html.clone(),
            });
            continue;
        }
        match chapters.last_mut() {
            Some(current) => {
                current.html.push('\n');
                current.html.push_str(&section.raw_html);
            }
            None => chapters.push(KindleChapter {
                title: String::new(),
                href: section.href.clone(),
                html: section.raw_html.clone(),
            }),
        }
    }
    chapters
}

/// Text of the heading that opens this markup, when it is `tag`.
fn leading_heading(html: &str, tag: &str) -> Option<String> {
    let trimmed = html.trim_start();
    let open = format!("<{tag}");
    if !trimmed.get(..open.len())?.eq_ignore_ascii_case(&open) {
        return None;
    }
    let rest = &trimmed[open.len()..];
    if !rest.starts_with(|character: char| {
        character == '>' || character == '/' || character.is_ascii_whitespace()
    }) {
        return None;
    }
    let close = format!("</{tag}>");
    let body_start = trimmed.find('>')? + 1;
    let body_end = find_ignore_case(&trimmed[body_start..], &close)? + body_start;
    let text = heading_text(&trimmed[body_start..body_end]);
    (!text.is_empty()).then_some(text)
}

/// Visible text of a markup fragment: tags removed, entities decoded and
/// whitespace collapsed.
fn heading_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find('<') {
        let start = cursor + offset;
        text.push_str(&html[cursor..start]);
        let name_start = start + 1;
        let opens_tag = html[name_start..].starts_with(|character: char| {
            character.is_ascii_alphabetic()
                || character == '/'
                || character == '!'
                || character == '?'
        });
        if !opens_tag {
            // A bare `<` in running text is content, not a tag.
            text.push('<');
            cursor = name_start;
            continue;
        }
        let Some(end) = html[start..].find('>') else {
            break;
        };
        cursor = start + end + 1;
    }
    text.push_str(&html[cursor..]);
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// How far past an `&` the decoder looks for the `;` that closes an entity.
/// Entity names are ASCII, so a window that stops a little early is harmless —
/// but it must never cut a character in half: `value.len()` counts bytes, and a
/// bare `&` in CJK text puts byte 34 inside a three-byte character.
const ENTITY_SCAN_BYTES: usize = 34;

/// First [`ENTITY_SCAN_BYTES`] bytes of `value`, floored to a character
/// boundary so the caller can slice with it.
fn entity_scan_window(value: &str) -> &str {
    let mut end = value.len().min(ENTITY_SCAN_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn decode_entities(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(semicolon) = entity_scan_window(tail).find(';') else {
            output.push('&');
            rest = &rest[index + 1..];
            continue;
        };
        match decode_entity(&tail[1..semicolon]) {
            Some(decoded) => {
                output.push_str(&decoded);
                rest = &tail[semicolon + 1..];
            }
            None => {
                output.push('&');
                rest = &rest[index + 1..];
            }
        }
    }
    output.push_str(rest);
    output
}

fn decode_entity(entity: &str) -> Option<String> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return char::from_u32(u32::from_str_radix(hex, 16).ok()?).map(String::from);
    }
    if let Some(decimal) = entity.strip_prefix('#') {
        return char::from_u32(decimal.parse().ok()?).map(String::from);
    }
    Some(
        match entity {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" => "\u{00A0}",
            "mdash" => "\u{2014}",
            "ndash" => "\u{2013}",
            "hellip" => "\u{2026}",
            "lsquo" => "\u{2018}",
            "rsquo" => "\u{2019}",
            "ldquo" => "\u{201C}",
            "rdquo" => "\u{201D}",
            _ => return None,
        }
        .to_string(),
    )
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn kindle_format(bytes: &[u8], extension: Option<&str>) -> BookFormat {
    if mobi_version(bytes).is_some_and(|version| version >= 8) || extension == Some("azw3") {
        BookFormat::Azw3
    } else if extension == Some("azw") {
        BookFormat::Azw
    } else {
        BookFormat::Mobi
    }
}

/// Shared with the KFX importer: `ebook-rs` exposes the same `NavPoint` tree
/// for both PalmDB MOBI/AZW and KFX containers.
pub(crate) fn flatten_nav(points: &[ebook_rs::NavPoint]) -> Vec<&ebook_rs::NavPoint> {
    fn visit<'a>(points: &'a [ebook_rs::NavPoint], output: &mut Vec<&'a ebook_rs::NavPoint>) {
        for point in points {
            output.push(point);
            visit(&point.subitems, output);
        }
    }
    let mut output = Vec::new();
    visit(points, &mut output);
    output
}

pub(crate) fn kindle_toc_node(
    point: &ebook_rs::NavPoint,
    href_to_unit: &HashMap<String, String>,
    book_id: &str,
) -> Option<TocNode> {
    let unit_id = href_to_unit
        .get(&href_key(&point.href))
        .or_else(|| href_to_unit.get(&href_key(&point.full_path)))?
        .clone();
    let mut node = TocNode::new(
        deterministic_id(
            "toc",
            format!("{book_id}\0{}\0{}", point.id, point.href).as_bytes(),
        ),
        safe_title(Some(&point.label), "Untitled"),
        TocTarget::unit(unit_id),
    );
    node.children = point
        .subitems
        .iter()
        .filter_map(|child| kindle_toc_node(child, href_to_unit, book_id))
        .collect();
    Some(node)
}

pub(crate) fn href_key(href: &str) -> String {
    href.split(['?', '#'])
        .next()
        .unwrap_or(href)
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays out a PalmDB container: 78-byte header, record offset table, then
    /// the records back to back.
    fn build_palmdb(records: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = vec![0_u8; 78];
        bytes[0..8].copy_from_slice(b"TestBook");
        bytes[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        let mut offset = 78 + records.len() * 8;
        for record in records {
            bytes.extend_from_slice(&(offset as u32).to_be_bytes());
            bytes.extend_from_slice(&[0, 0, 0, 0]);
            offset += record.len();
        }
        for record in records {
            bytes.extend_from_slice(record);
        }
        bytes
    }

    fn split_records(bytes: &[u8]) -> Vec<Vec<u8>> {
        let count = usize::from(u16::from_be_bytes([bytes[76], bytes[77]]));
        let starts = (0..count)
            .map(|index| read_u32(bytes, 78 + index * 8).unwrap() as usize)
            .collect::<Vec<_>>();
        (0..count)
            .map(|index| {
                let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
                bytes[starts[index]..end].to_vec()
            })
            .collect()
    }

    /// A HUFF/CDIC container whose single text record decodes to `ABCDAB`.
    ///
    /// The trailing records stand in for the resource region: they must keep
    /// their exact indices, because `first_image_index` and the `recindex=`
    /// rewriting name them by number.
    fn huffcdic_container() -> Vec<u8> {
        let text = b"ABCDAB";
        let mut record0 = vec![0_u8; 280];
        record0[0..2].copy_from_slice(&kindle_huff::COMPRESSION_HUFFCDIC.to_be_bytes());
        record0[4..8].copy_from_slice(&(text.len() as u32).to_be_bytes());
        record0[8..10].copy_from_slice(&1_u16.to_be_bytes());
        record0[10..12].copy_from_slice(&4_096_u16.to_be_bytes());
        record0[16..20].copy_from_slice(b"MOBI");
        record0[20..24].copy_from_slice(&264_u32.to_be_bytes());
        record0[112..116].copy_from_slice(&2_u32.to_be_bytes());
        record0[116..120].copy_from_slice(&2_u32.to_be_bytes());
        build_palmdb(&[
            record0,
            // `tiny_huff` maps 0xFF to phrase 0 and 0xFE to phrase 1.
            vec![0xFF, 0xFE, 0xFF],
            kindle_huff::tiny_huff(),
            kindle_huff::tiny_cdic(&[(b"AB", true), (b"CD", true)]),
            b"\x89PNG\r\n\x1a\nresource".to_vec(),
            b"last record".to_vec(),
        ])
    }

    #[test]
    fn huffcdic_text_is_repacked_without_moving_record_indices() {
        let original = huffcdic_container();
        let prepared = prepare_kindle_source(&original).expect("the container is repacked");
        let rewritten = match prepared {
            Cow::Owned(bytes) => bytes,
            Cow::Borrowed(_) => panic!("HUFF/CDIC text must be rewritten"),
        };

        let records = split_records(&rewritten);
        assert_eq!(records.len(), 6, "the record count must not change");
        assert_eq!(
            u16::from_be_bytes([records[0][0], records[0][1]]),
            1,
            "the repacked container must declare uncompressed text records"
        );
        assert_eq!(
            records[0][4..8],
            [0_u8, 0, 0, 6],
            "the decoded text length is preserved"
        );
        assert_eq!(
            records[1].as_slice(),
            b"ABCDAB".as_slice(),
            "the text record must hold the decoded text"
        );

        let original_records = split_records(&original);
        for index in 2..6 {
            assert_eq!(
                records[index], original_records[index],
                "record {index} must stay at its original index unchanged"
            );
        }
    }

    #[test]
    fn a_palmdoc_container_is_handed_over_untouched() {
        let mut record0 = vec![0_u8; 280];
        record0[0..2].copy_from_slice(&2_u16.to_be_bytes());
        let original = build_palmdb(&[record0, vec![0x01, 0x02], b"resource".to_vec()]);
        match prepare_kindle_source(&original).expect("PalmDOC needs no rewrite") {
            Cow::Borrowed(bytes) => assert_eq!(bytes, original.as_slice()),
            Cow::Owned(_) => panic!("PalmDOC text must not be rewritten"),
        }
    }

    /// Record 0 of a PalmDOC container whose `extra_record_flags` says the text
    /// records carry both a trailing data region and the multibyte overlap.
    fn record0_with_trailing_regions(compression: u16, text_record_count: u16) -> Vec<u8> {
        let mut record0 = vec![0_u8; 280];
        record0[0..2].copy_from_slice(&compression.to_be_bytes());
        record0[8..10].copy_from_slice(&text_record_count.to_be_bytes());
        record0[10..12].copy_from_slice(&4_096_u16.to_be_bytes());
        record0[16..20].copy_from_slice(b"MOBI");
        record0[20..24].copy_from_slice(&264_u32.to_be_bytes());
        record0[EXTRA_RECORD_FLAGS_FIELD..EXTRA_RECORD_FLAGS_FIELD + 4]
            .copy_from_slice(&3_u32.to_be_bytes());
        record0
    }

    /// Appends what `extra_record_flags == 3` describes: two multibyte overlap
    /// bytes whose last byte holds `1` (so the stripper removes 2) and a
    /// five-byte region whose trailing size varint reads 5.
    fn with_trailing_region(mut body: Vec<u8>) -> Vec<u8> {
        body.extend_from_slice(&[0xAA, 0x01]);
        body.extend_from_slice(&[0xFF, 0x00, 0x00, 0x00, 0x05]);
        body
    }

    #[test]
    fn palmdoc_text_records_lose_their_trailing_data_regions() {
        let original = build_palmdb(&[
            record0_with_trailing_regions(1, 2),
            with_trailing_region("作者简介".as_bytes().to_vec()),
            with_trailing_region("前言".as_bytes().to_vec()),
            b"\x89PNG\r\n\x1a\nresource".to_vec(),
        ]);
        let rewritten = match prepare_kindle_source(&original).expect("the container is repacked") {
            Cow::Owned(bytes) => bytes,
            Cow::Borrowed(_) => panic!("text records carrying trailing data must be rewritten"),
        };

        let records = split_records(&rewritten);
        assert_eq!(records.len(), 4, "the record count must not change");
        assert_eq!(
            records[1].as_slice(),
            "作者简介".as_bytes(),
            "the trailing region is not text and must be gone"
        );
        assert_eq!(records[2].as_slice(), "前言".as_bytes());
        assert_eq!(
            records[3].as_slice(),
            b"\x89PNG\r\n\x1a\nresource",
            "records after the text must stay at their index unchanged"
        );
        let joined = [records[1].as_slice(), records[2].as_slice()].concat();
        assert_eq!(
            std::str::from_utf8(&joined).expect("the joined text must be valid UTF-8"),
            "作者简介前言",
            "ebook-rs must no longer fall back to WINDOWS-1252"
        );
    }

    #[test]
    fn trimming_trailing_regions_keeps_the_text_records_compressed() {
        let original = build_palmdb(&[
            record0_with_trailing_regions(2, 1),
            with_trailing_region(vec![0x41, 0x42]),
            b"resource".to_vec(),
        ]);
        let rewritten = match prepare_kindle_source(&original).expect("the container is repacked") {
            Cow::Owned(bytes) => bytes,
            Cow::Borrowed(_) => panic!("trailing regions must be trimmed"),
        };

        let records = split_records(&rewritten);
        assert_eq!(
            u16::from_be_bytes([records[0][0], records[0][1]]),
            2,
            "ebook-rs still owns the PalmDOC decoding"
        );
        assert_eq!(records[1].as_slice(), &[0x41, 0x42]);
        assert_eq!(records[2].as_slice(), b"resource");
    }

    #[test]
    fn a_palmdoc_container_without_trailing_regions_is_handed_over_untouched() {
        let mut record0 = record0_with_trailing_regions(2, 1);
        record0[EXTRA_RECORD_FLAGS_FIELD..EXTRA_RECORD_FLAGS_FIELD + 4]
            .copy_from_slice(&0_u32.to_be_bytes());
        let original = build_palmdb(&[record0, vec![0x41, 0x42], b"resource".to_vec()]);
        match prepare_kindle_source(&original).expect("PalmDOC needs no rewrite") {
            Cow::Borrowed(bytes) => assert_eq!(bytes, original.as_slice()),
            Cow::Owned(_) => panic!("a container with no trailing regions must not be rewritten"),
        }
    }

    fn header(version: u32) -> Vec<u8> {
        let mut bytes = vec![0; 256];
        bytes[60..68].copy_from_slice(b"BOOKMOBI");
        bytes[76..78].copy_from_slice(&2_u16.to_be_bytes());
        bytes[78..82].copy_from_slice(&100_u32.to_be_bytes());
        bytes[86..90].copy_from_slice(&232_u32.to_be_bytes());
        bytes[100..102].copy_from_slice(&1_u16.to_be_bytes());
        bytes[104..108].copy_from_slice(&16_u32.to_be_bytes());
        bytes[108..110].copy_from_slice(&1_u16.to_be_bytes());
        bytes[110..112].copy_from_slice(&4_096_u16.to_be_bytes());
        bytes[116..120].copy_from_slice(b"MOBI");
        bytes[120..124].copy_from_slice(&116_u32.to_be_bytes());
        bytes[136..140].copy_from_slice(&version.to_be_bytes());
        bytes
    }

    #[test]
    fn detects_kf8_from_the_mobi_header_not_only_the_extension() {
        let source = ImportSource {
            file_name: Some("renamed.mobi".into()),
            bytes: Arc::new(header(8)),
        };
        assert_eq!(KindleImporter.probe(&source).format, BookFormat::Azw3);
    }

    #[test]
    fn rejects_declared_kindle_text_expansion_before_parser_allocation() {
        let mut bytes = header(8);
        bytes[104..108].copy_from_slice(&1_024_u32.to_be_bytes());
        let limits = ImportLimits {
            max_total_text_bytes: 1_023,
            ..ImportLimits::default()
        };
        let error = validate_mobi_header(&bytes, &limits)
            .expect_err("declared decompressed text above the limit must be rejected");
        assert!(
            error
                .to_string()
                .contains("decompressed text exceeds the total text safety limit")
        );
    }

    #[test]
    fn rejects_possible_palmdoc_expansion_even_when_declared_length_is_small() {
        let mut bytes = header(8);
        bytes[100..102].copy_from_slice(&2_u16.to_be_bytes());
        let limits = ImportLimits {
            max_total_text_bytes: 119,
            ..ImportLimits::default()
        };
        let error = validate_mobi_header(&bytes, &limits)
            .expect_err("PalmDOC worst-case expansion above the limit must be rejected");
        assert!(
            error
                .to_string()
                .contains("text records can exceed the total text safety limit")
        );
    }

    #[test]
    fn rejects_a_kindle_extension_without_palmdb_magic() {
        let source = ImportSource {
            file_name: Some("fake.azw3".into()),
            bytes: Arc::new(b"not a book".to_vec()),
        };
        assert_eq!(
            KindleImporter.probe(&source).confidence,
            ProbeConfidence::Extension
        );
        assert!(
            KindleImporter
                .import(&source, &ImportLimits::default())
                .is_err()
        );
    }

    /// Record 0 of a container whose MOBI header is followed by `tags`.
    fn record0_with_exth(tags: &[(u32, &[u8])]) -> Vec<u8> {
        const HEADER_LEN: usize = 232;
        let mut exth = Vec::new();
        exth.extend_from_slice(b"EXTH");
        let payload_len = tags.iter().map(|(_, value)| 8 + value.len()).sum::<usize>();
        exth.extend_from_slice(&((12 + payload_len) as u32).to_be_bytes());
        exth.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        for (tag, value) in tags {
            exth.extend_from_slice(&tag.to_be_bytes());
            exth.extend_from_slice(&((8 + value.len()) as u32).to_be_bytes());
            exth.extend_from_slice(value);
        }

        let mut record0 = vec![0_u8; 16 + HEADER_LEN + exth.len()];
        record0[0..2].copy_from_slice(&1_u16.to_be_bytes());
        record0[4..8].copy_from_slice(&64_u32.to_be_bytes());
        record0[8..10].copy_from_slice(&2_u16.to_be_bytes());
        record0[10..12].copy_from_slice(&4_096_u16.to_be_bytes());
        record0[16..20].copy_from_slice(b"MOBI");
        record0[20..24].copy_from_slice(&(HEADER_LEN as u32).to_be_bytes());
        // First resource record: record 3.
        record0[108..112].copy_from_slice(&3_u32.to_be_bytes());
        record0[16 + HEADER_LEN..].copy_from_slice(&exth);
        record0
    }

    #[test]
    fn resources_are_numbered_from_the_first_resource_record() {
        let record0 = record0_with_exth(&[(EXTH_COVER_OFFSET_TAG, &2_u32.to_be_bytes())]);
        let container = build_palmdb(&[
            record0,
            b"text record one".to_vec(),
            b"text record two".to_vec(),
            b"\x89PNG\r\n\x1a\nfirst".to_vec(),
            b"\xFF\xD8\xFFsecond".to_vec(),
            b"\xFF\xD8\xFFcover".to_vec(),
            b"not an image".to_vec(),
        ]);
        let palm = PalmDb::parse(&container)
            .expect("the container parses")
            .expect("the record table is readable");
        let limits = ImportLimits::default();
        let mut budget =
            AssetBudget::with_original(&limits, container.len(), "test container").unwrap();
        let resources = collect_kindle_resources(&palm, &mut budget).expect("resources are read");

        assert_eq!(resources.len(), 3, "records 3, 4 and 5 hold images");
        assert_eq!(resources[0].number, 1);
        assert_eq!(resources[0].extension, "png");
        assert!(!resources[0].is_cover);
        assert_eq!(resources[1].number, 2);
        assert_eq!(resources[1].extension, "jpg");
        assert_eq!(resources[2].number, 3);
        assert!(resources[2].is_cover, "EXTH 201 offset 2 names record 5");
    }

    #[test]
    fn resources_fall_back_to_the_record_after_the_text_records() {
        let mut record0 = vec![0_u8; 300];
        record0[0..2].copy_from_slice(&1_u16.to_be_bytes());
        record0[8..10].copy_from_slice(&2_u16.to_be_bytes());
        record0[16..20].copy_from_slice(b"MOBI");
        record0[20..24].copy_from_slice(&264_u32.to_be_bytes());
        record0[108..112].copy_from_slice(&u32::MAX.to_be_bytes());
        let container = build_palmdb(&[
            record0,
            b"text one".to_vec(),
            b"text two".to_vec(),
            b"\xFF\xD8\xFFpicture".to_vec(),
        ]);
        let palm = PalmDb::parse(&container)
            .expect("the container parses")
            .expect("the record table is readable");
        let limits = ImportLimits::default();
        let mut budget =
            AssetBudget::with_original(&limits, container.len(), "test container").unwrap();
        let resources = collect_kindle_resources(&palm, &mut budget).expect("resources are read");

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].number, 1);
    }

    #[test]
    fn kindle_embed_references_use_the_base32_resource_number() {
        let mut paths = HashMap::new();
        paths.insert(1_usize, "kindle-resource/0001.jpg".to_string());
        paths.insert(10_usize, "kindle-resource/0010.jpg".to_string());
        paths.insert(57_usize, "kindle-resource/0057.jpg".to_string());
        let html = concat!(
            r#"<img src="kindle:embed:0001?mime=image/jpg"/>"#,
            r#"<img src="kindle:embed:000A?mime=image/jpg"/>"#,
            r#"<img src="kindle:embed:001P?mime=image/jpg"/>"#,
            r#"<img src="kindle:embed:000B?mime=image/jpg"/>"#,
        );
        assert_eq!(
            rewrite_kindle_media_references(html, &paths),
            concat!(
                r#"<img src="kindle-resource/0001.jpg"/>"#,
                r#"<img src="kindle-resource/0010.jpg"/>"#,
                r#"<img src="kindle-resource/0057.jpg"/>"#,
                // 000B is not a retained resource: the reference is left alone
                // so the shared rewrite pass still drops the attribute.
                r#"<img src="kindle:embed:000B?mime=image/jpg"/>"#,
            )
        );
    }

    #[test]
    fn legacy_ebook_rs_image_paths_map_back_to_the_resource_number() {
        let mut paths = HashMap::new();
        paths.insert(10_usize, "kindle-resource/0010.jpg".to_string());
        assert_eq!(
            rewrite_kindle_media_references(
                r#"<img src="images/img_0010.jpg?mime=image/jpg"/>"#,
                &paths
            ),
            r#"<img src="kindle-resource/0010.jpg"/>"#
        );
        assert_eq!(
            rewrite_kindle_media_references(r#"<img src="images/img_0099.jpg"/>"#, &paths),
            r#"<img src="images/img_0099.jpg"/>"#
        );
    }

    fn section(href: &str, html: &str) -> ebook_rs::Section {
        ebook_rs::Section {
            index: 0,
            idref: href.to_string(),
            href: href.to_string(),
            full_path: href.to_string(),
            raw_html: html.to_string(),
            processed_html: html.to_string(),
            plain_text: String::new(),
            plain_text_lower: String::new(),
            char_count: 0,
            viewport_width: None,
            viewport_height: None,
        }
    }

    #[test]
    fn chapters_split_only_on_level_one_headings_that_carry_text() {
        let sections = vec![
            section(
                "section_0.html",
                "<html><body><p>front matter</p></body></html>",
            ),
            // A converted document puts its `_Toc…` bookmark in an empty `<h1>`.
            section("section_1.html", r#"<h1><a name="_Toc1"></a></h1>"#),
            section(
                "section_2.html",
                r#"<h1>Rust Programming Language</h1><h2>What is Rust?</h2>"#,
            ),
            section("section_3.html", "<h2>Installation</h2>"),
            section("section_4.html", "<h1>Loops</h1><p>body</p>"),
        ];
        let chapters = kindle_chapters(&sections);

        assert_eq!(chapters.len(), 3, "an empty h1 must not start a chapter");
        assert_eq!(chapters[0].title, "", "the front matter opens no chapter");
        assert_eq!(chapters[0].href, "section_0.html");
        assert!(chapters[0].html.contains("front matter"));
        assert!(
            chapters[0].html.contains(r#"<a name="_Toc1">"#),
            "the bookmark-only section stays with the front matter"
        );
        assert_eq!(chapters[1].title, "Rust Programming Language");
        assert!(chapters[1].html.contains("What is Rust?"));
        assert!(
            chapters[1].html.contains("Installation"),
            "h2 sections stay inside the chapter that owns them"
        );
        assert_eq!(chapters[2].title, "Loops");
    }

    #[test]
    fn heading_titles_decode_entities_and_ignore_empty_headings() {
        assert_eq!(
            leading_heading(r#"<h1>&quot;if-else&quot;</h1>"#, "h1").as_deref(),
            Some("\"if-else\"")
        );
        assert_eq!(
            leading_heading("<h1>  A\n B </h1>", "h1").as_deref(),
            Some("A B")
        );
        assert_eq!(
            leading_heading(r#"<h1><a name="_Toc"></a></h1>"#, "h1"),
            None
        );
        assert_eq!(leading_heading("<h2>Not a chapter</h2>", "h1"), None);
    }

    #[test]
    fn orphan_stylesheet_text_is_dropped() {
        let html = "<p>tail</p> \r\n\t\t\t@page Section1 { size:612pt 792pt; margin:72pt }\r\n\t\t\tdiv.Section1 { page:Section1 }\r\n\t\t";
        assert_eq!(strip_orphan_css(html), "<p>tail</p>");

        let prose = "<p>a &lt; b</p><p>done</p>";
        assert_eq!(strip_orphan_css(prose), prose);
    }

    #[test]
    fn at_rule_inside_a_tag_is_body_copy() {
        // A CSS tutorial can put an at-rule in its own paragraph; the paragraph
        // is content, not a stylesheet flow.
        let paragraph = "<p>@media (max-width: 600px) { body { color: red } }</p>";
        assert_eq!(strip_orphan_css(paragraph), paragraph);

        let tutorial = "<h2>Media queries</h2><p>@page Section1 { size: 612pt }</p><p>done</p>";
        assert_eq!(strip_orphan_css(tutorial), tutorial);
    }

    #[test]
    fn orphan_stylesheet_text_outside_tags_is_still_dropped() {
        let html = "<p>tail</p>@page Section1 { size:612pt }<p>head</p>";
        assert_eq!(strip_orphan_css(html), "<p>tail</p><p>head</p>");
    }

    #[test]
    fn an_unclosed_ampersand_before_cjk_text_does_not_split_a_character() {
        // `&` 后面跟够长的中文：按字节长度截断扫描窗口时，第 34 字节正好落在「中」
        // 的中间。它没有配对的 `;`，应当原样保留，而不是让切片 panic 掉整次导入。
        let text = format!("&{}中", "a".repeat(32));
        assert_eq!(decode_entities(&text), text);
    }

    #[test]
    fn chapter_headings_with_a_bare_ampersand_survive() {
        // 真实章节标题的形状：ASCII 书名 + `&` + 中文。扫描窗口的字节边界落在中文
        // 字符内部，导入必须照常拿到标题。
        let heading = "Tom & Jerry 汤姆和杰瑞历险记续篇";
        let html = format!("<h1>{heading}</h1>");
        assert_eq!(leading_heading(&html, "h1").as_deref(), Some(heading));
    }
}
