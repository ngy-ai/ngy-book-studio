use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as _, Result, bail};

use crate::{
    document::{
        AssetRole, BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind,
        SourceLocator, TocNode, TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, asset_from_bytes,
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
        if !is_mobi_container(source.bytes.as_slice()) {
            bail!("source is not a MOBI/AZW PalmDB container");
        }
        let mut asset_budget =
            AssetBudget::with_original(limits, source.bytes.len(), "Kindle original source")?;
        validate_mobi_header(source.bytes.as_slice(), limits)?;
        let format = kindle_format(source.bytes.as_slice(), source.extension().as_deref());
        let parsed = ebook_rs::MobiBook::parse(source.bytes.as_slice()).map_err(|error| {
            anyhow::anyhow!(error).context(
                "ebook-rs rejected the Kindle file; encrypted Kindle books are not supported",
            )
        })?;
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

        let mut assets = vec![original.clone()];
        let mut asset_ids_by_href = HashMap::new();
        let mut cover_asset_id = None;
        if let Some((cover_bytes, cover_mime)) = parsed.cover_image()
            && !cover_bytes.is_empty()
        {
            asset_budget.add_usize(cover_bytes.len(), "Kindle cover image")?;
            let cover = asset_from_bytes(
                &format!("{book_id}\0kindle-cover"),
                vec![AssetRole::Cover, AssetRole::ContentImage],
                cover_mime,
                None,
                Arc::new(cover_bytes),
            );
            cover_asset_id = Some(cover.metadata.id.clone());
            assets.push(cover);
        }
        let existing_hashes = assets
            .iter()
            .map(|asset| asset.metadata.content_hash.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut existing_hashes = existing_hashes;
        for item in parsed.manifest().values() {
            let role = if item.media_type.starts_with("image/") {
                Some(AssetRole::ContentImage)
            } else if item.media_type.starts_with("audio/") {
                Some(AssetRole::Audio)
            } else if item.media_type.starts_with("video/") {
                Some(AssetRole::Video)
            } else {
                None
            };
            let Some(role) = role else {
                continue;
            };
            let Ok((bytes, detected_mime)) = parsed.get_resource_bytes(&item.full_path) else {
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            asset_budget
                .check_single_usize(bytes.len(), &format!("Kindle resource {}", item.full_path))?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            if !existing_hashes.insert(hash.clone()) {
                // `cover_image` and the manifest may expose the same bytes.
                // Even when the blob is deduplicated, every manifest href must
                // still resolve to the retained asset so section markup can be
                // rewritten to its stable asset ID.
                if let Some(existing) = assets
                    .iter()
                    .find(|asset| asset.metadata.content_hash == hash)
                {
                    for href in [&item.href, &item.full_path] {
                        if let Some(key) = normalized_archive_href(href) {
                            asset_ids_by_href.insert(key, existing.metadata.id.clone());
                        }
                    }
                }
                continue;
            }
            asset_budget.add_usize(bytes.len(), &format!("Kindle resource {}", item.full_path))?;
            let asset = asset_from_bytes(
                &format!("{book_id}\0kindle-resource\0{}", item.full_path),
                vec![role],
                if item.media_type.is_empty() {
                    detected_mime
                } else {
                    &item.media_type
                },
                item.full_path.rsplit('/').next().map(str::to_owned),
                Arc::new(bytes),
            );
            for href in [&item.href, &item.full_path] {
                if let Some(key) = normalized_archive_href(href) {
                    asset_ids_by_href.insert(key, asset.metadata.id.clone());
                }
            }
            assets.push(asset);
        }

        let toc_titles = flatten_nav(parsed.toc())
            .into_iter()
            .map(|point| (href_key(&point.href), point.label.clone()))
            .collect::<HashMap<_, _>>();
        let mut units = Vec::with_capacity(parsed.sections().len());
        let mut href_to_unit = HashMap::new();
        for (index, section) in parsed.sections().iter().enumerate() {
            if section.raw_html.len() > limits.max_unit_text_bytes {
                bail!("Kindle section {} exceeds the text safety limit", index + 1);
            }
            let unit_id = deterministic_id(
                "unit",
                format!("{book_id}\0kindle\0{}\0{}", index + 1, section.href).as_bytes(),
            );
            let section_title = toc_titles
                .get(&href_key(&section.href))
                .cloned()
                .unwrap_or_else(|| format!("Chapter {}", index + 1));
            let rewritten = rewrite_imported_html_assets(
                &section.raw_html,
                &section.full_path,
                &asset_ids_by_href,
            )?;
            let normalized = crate::markup::parse_source_for_unit(&rewritten, &unit_id)
                .with_context(|| format!("failed to normalize Kindle section {}", index + 1))?;
            units.push(
                ContentUnit::new(
                    unit_id.clone(),
                    ContentUnitKind::Chapter,
                    section_title,
                    normalized.canonical_source,
                    normalized.document,
                )
                .with_source_locator(SourceLocator::kindle_section(
                    u32::try_from(index + 1).unwrap_or(u32::MAX),
                    Some(section.href.clone()),
                )),
            );
            href_to_unit.insert(href_key(&section.href), unit_id.clone());
            href_to_unit.insert(href_key(&section.full_path), unit_id);
        }

        let toc = parsed
            .toc()
            .iter()
            .filter_map(|point| kindle_toc_node(point, &href_to_unit, &book_id))
            .collect::<Vec<_>>();
        let toc = if toc.is_empty() {
            units
                .iter()
                .enumerate()
                .map(|(index, unit)| {
                    TocNode::new(
                        deterministic_id("toc", format!("{book_id}\0{index}").as_bytes()),
                        unit.title.clone(),
                        TocTarget::unit(unit.id.clone()),
                    )
                })
                .collect()
        } else {
            toc
        };

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
        Ok(ImportedBook { document, assets })
    }
}

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
    bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
        || bytes.starts_with(b"BM")
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
}
