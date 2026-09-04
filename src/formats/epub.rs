use std::{collections::HashMap, io::Cursor, sync::Arc};

use anyhow::{Context as _, Result};
use rbook::Epub;

use crate::{
    document::{
        AssetRole, BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind, SourceKind,
        SourceLocator, TocNode, TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, asset_from_bytes,
        normalized_archive_href, original_asset, rewrite_imported_html_assets, safe_title,
    },
};

#[derive(Clone, Copy, Debug, Default)]
pub struct EpubImporter;

impl DocumentImporter for EpubImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "rbook",
            formats: &[BookFormat::Epub],
            parser_version: "0.7.10",
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        let bytes = source.bytes.as_slice();
        let is_zip = bytes.starts_with(b"PK\x03\x04")
            || bytes.starts_with(b"PK\x05\x06")
            || bytes.starts_with(b"PK\x07\x08");
        if is_zip && zip_contains(bytes, "META-INF/container.xml") {
            ProbeResult {
                format: BookFormat::Epub,
                confidence: ProbeConfidence::Container,
                detail: Some("EPUB container.xml is present".to_string()),
            }
        } else if source.extension().as_deref() == Some("epub") {
            ProbeResult {
                format: BookFormat::Epub,
                confidence: ProbeConfidence::Extension,
                detail: Some(".epub extension without a valid EPUB container".to_string()),
            }
        } else {
            ProbeResult::no_match(BookFormat::Epub)
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        let mut asset_budget =
            AssetBudget::with_original(limits, source.bytes.len(), "EPUB original source")?;
        crate::epub_limits::validate_epub_archive_bytes(source.bytes.as_slice())
            .context("EPUB exceeds archive safety limits")?;
        let epub = Epub::read(Cursor::new(source.bytes.as_ref().clone()))
            .context("source is not a valid EPUB")?;
        let original = original_asset(source, BookFormat::Epub, "application/epub+zip");
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        let metadata = epub.metadata();
        let title = safe_title(
            metadata.title().map(|title| title.value()),
            source.stem().as_str(),
        );
        let authors = metadata
            .creators()
            .map(|creator| creator.value().trim().to_string())
            .filter(|author| !author.is_empty())
            .collect::<Vec<_>>();

        let toc_titles = epub
            .toc()
            .contents()
            .map(|root| {
                let mut map = HashMap::new();
                for entry in root.flatten() {
                    let Some(href) = entry.href() else { continue };
                    let href = href.as_str().to_string();
                    let label = entry.label().trim();
                    if !label.is_empty() {
                        // Keep the first entry for each href — it is the closest
                        // to the root and typically the correct chapter title.
                        map.entry(href_key(&href))
                            .or_insert_with(|| label.to_string());
                    }
                }
                map
            })
            .unwrap_or_default();

        let mut imported_assets = vec![original.clone()];
        let mut asset_ids_by_href = HashMap::new();
        let mut cover_asset_id = None;
        let cover_href = epub
            .manifest()
            .cover_image()
            .map(|entry| href_key(entry.href().as_str()));
        for entry in epub.manifest().iter() {
            let media_type = entry.media_type().trim();
            let role = asset_role(
                media_type,
                cover_href.as_deref() == Some(&href_key(entry.href().as_str())),
            );
            let Some(role) = role else {
                continue;
            };
            let bytes = match entry.read_bytes() {
                Ok(bytes) if !bytes.is_empty() => bytes,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(href = %entry.href().as_str(), %error, "skipping unreadable EPUB asset");
                    continue;
                }
            };
            let href = entry.href().as_str().to_string();
            asset_budget.add_usize(bytes.len(), &format!("EPUB asset {href}"))?;
            let file_name = href
                .split(['?', '#'])
                .next()
                .and_then(|path| path.rsplit('/').next())
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let roles = if role == AssetRole::Cover {
                vec![AssetRole::Cover, AssetRole::ContentImage]
            } else {
                vec![role]
            };
            let asset = asset_from_bytes(
                &format!("epub-resource\0{href}"),
                roles,
                if media_type.is_empty() {
                    "application/octet-stream"
                } else {
                    media_type
                },
                file_name,
                Arc::new(bytes),
            );
            if role == AssetRole::Cover {
                cover_asset_id = Some(asset.metadata.id.clone());
            }
            if let Some(key) = normalized_archive_href(&href) {
                asset_ids_by_href.insert(key, asset.metadata.id.clone());
            }
            imported_assets.push(asset);
        }

        let mut units = Vec::new();
        let mut href_to_unit = HashMap::new();
        for (index, entry) in epub
            .spine()
            .iter()
            .filter(|entry| entry.is_linear())
            .filter_map(|entry| entry.manifest_entry())
            .enumerate()
        {
            if units.len() >= limits.max_units {
                anyhow::bail!("EPUB contains too many visible spine entries");
            }
            let href = entry.href().as_str().to_string();
            let html = entry
                .read_str()
                .with_context(|| format!("failed to read EPUB chapter {href}"))?;
            if html.len() > limits.max_unit_text_bytes {
                anyhow::bail!("EPUB chapter {href} exceeds the text safety limit");
            }
            let unit_id = deterministic_id(
                "unit",
                format!("{}\0{}\0{}", book_id, index, href_key(&href)).as_bytes(),
            );
            let chapter_title = toc_titles
                .get(&href_key(&href))
                .cloned()
                .unwrap_or_else(|| title_from_href(&href, index));
            let rewritten = rewrite_imported_html_assets(&html, &href, &asset_ids_by_href)?;
            let parsed =
                crate::markup::parse_source_for_unit(SourceKind::Html, &rewritten, &unit_id)
                    .with_context(|| format!("failed to normalize EPUB chapter {href}"))?;
            let unit = ContentUnit::new(
                unit_id.clone(),
                ContentUnitKind::Chapter,
                chapter_title,
                SourceKind::Html,
                parsed.canonical_source,
                parsed.document,
            )
            .with_source_locator(SourceLocator::epub(href.clone()));
            href_to_unit.insert(href_key(&href), unit_id);
            units.push(unit);
        }

        let toc = epub
            .toc()
            .contents()
            .map(|root| {
                root.iter()
                    .filter_map(|entry| epub_toc_node(entry, &href_to_unit, &book_id))
                    .collect::<Vec<_>>()
            })
            .filter(|nodes| !nodes.is_empty())
            .unwrap_or_else(|| {
                units
                    .iter()
                    .enumerate()
                    .map(|(index, unit)| {
                        TocNode::new(
                            deterministic_id("toc", format!("{}\0{}", book_id, index).as_bytes()),
                            unit.title.clone(),
                            TocTarget::unit(unit.id.clone()),
                        )
                    })
                    .collect()
            });

        let mut document = BookDocument::new(
            book_id,
            title,
            BookSource::imported(
                BookFormat::Epub,
                original.metadata.id.clone(),
                source.file_name.clone(),
            ),
        );
        document.authors = authors;
        document.units = units;
        document.toc = toc;
        document.cover_asset_id = cover_asset_id;
        document.assets = imported_assets
            .iter()
            .map(|asset| asset.metadata.clone())
            .collect();
        Ok(ImportedBook {
            document,
            assets: imported_assets,
        })
    }
}

fn zip_contains(bytes: &[u8], name: &str) -> bool {
    zip::ZipArchive::new(Cursor::new(bytes)).is_ok_and(|mut archive| archive.by_name(name).is_ok())
}

fn asset_role(media_type: &str, cover: bool) -> Option<AssetRole> {
    if cover {
        Some(AssetRole::Cover)
    } else if media_type.starts_with("image/") {
        Some(AssetRole::ContentImage)
    } else if media_type.starts_with("audio/") {
        Some(AssetRole::Audio)
    } else if media_type.starts_with("video/") {
        Some(AssetRole::Video)
    } else if media_type == "text/css" {
        Some(AssetRole::Stylesheet)
    } else if media_type.starts_with("font/")
        || media_type.contains("font")
        || media_type.contains("opentype")
    {
        Some(AssetRole::Font)
    } else {
        None
    }
}

fn epub_toc_node(
    entry: rbook::epub::toc::EpubTocEntry<'_>,
    href_to_unit: &HashMap<String, String>,
    book_id: &str,
) -> Option<TocNode> {
    let href = entry.href()?.as_str().to_string();
    let unit_id = href_to_unit.get(&href_key(&href))?.clone();
    let label = entry.label().trim();
    let mut node = TocNode::new(
        deterministic_id(
            "toc",
            format!("{}\0{}\0{}", book_id, href, label).as_bytes(),
        ),
        if label.is_empty() { "Untitled" } else { label },
        TocTarget::unit(unit_id),
    );
    node.children = entry
        .iter()
        .filter_map(|child| epub_toc_node(child, href_to_unit, book_id))
        .collect();
    Some(node)
}

fn href_key(href: &str) -> String {
    href.split(['?', '#'])
        .next()
        .unwrap_or(href)
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn title_from_href(href: &str, index: usize) -> String {
    let stem = href
        .split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('/').next())
        .and_then(|name| name.rsplit_once('.').map(|(stem, _)| stem).or(Some(name)))
        .unwrap_or("");
    let decoded = urlencoding::decode(stem)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| stem.to_string())
        .replace(['-', '_'], " ");
    if decoded.trim().is_empty() {
        format!("Chapter {}", index + 1)
    } else {
        decoded.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_trust_an_epub_extension_without_a_container() {
        let source = ImportSource {
            file_name: Some("fake.epub".into()),
            bytes: Arc::new(b"not a zip".to_vec()),
        };
        assert_eq!(
            EpubImporter.probe(&source).confidence,
            ProbeConfidence::Extension
        );
        assert!(
            EpubImporter
                .import(&source, &ImportLimits::default())
                .is_err()
        );
    }
}
