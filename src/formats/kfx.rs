//! Amazon KFX (Kindle Format 10) import.
//!
//! Only DRM-free containers are supported. `ebook-rs`'s `KfxBook` parses the
//! `CONT` container and carves readable paragraph fragments out of it into
//! synthetic sections; it is a heuristic text extractor rather than a complete
//! KFX/Ion reader, so it never exposes resources and may fall back to built-in
//! metadata placeholders. This importer therefore:
//!
//! * rejects encrypted containers before parsing, with the same boundary as
//!   the MOBI/AZW path,
//! * keeps only the byte-exact original plus the carved text (no cover and no
//!   content assets can be recovered), and
//! * refuses containers whose carved text is not plausible prose instead of
//!   publishing a garbage book.
//!
//! Importer-local sanity plus [`super::ImportLimits`] bound the work; the
//! third-party IR itself never leaves this module.

use std::collections::HashMap;

use anyhow::{Context as _, Result, bail, ensure};
use ebook_rs::KfxBook;

use crate::{
    document::{
        BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind, SourceLocator, TocNode,
        TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, original_asset, safe_title,
    },
};

use super::kindle::{flatten_nav, href_key, kindle_toc_node};

#[derive(Clone, Copy, Debug, Default)]
pub struct KfxImporter;

const KFX_FORMATS: &[BookFormat] = &[BookFormat::Kfx];
const KFX_MEDIA_TYPE: &str = "application/x-kfx";

/// `ebook-rs` substitutes these when the container carries no metadata; they
/// must not become the imported book's title/author.
const KFX_PLACEHOLDER_TITLE: &str = "Amazon KFX Publication";
const KFX_PLACEHOLDER_AUTHOR: &str = "Unknown Author";

/// Minimum non-whitespace characters the carved text must contain. Below this
/// the container yielded no book text at all.
const MIN_KFX_VISIBLE_CHARS: usize = 64;
/// Maximum share of replacement/control/private-use characters. A much larger
/// share means the container bytes were not actually decoded into text.
const MAX_KFX_UNREADABLE_RATIO: f64 = 0.05;
/// Minimum share of characters that belong to word-like runs (three or more
/// consecutive letters/digits). Prose stays far above this; symbol soup
/// carved out of a binary payload stays far below it.
const MIN_KFX_WORD_LIKE_RATIO: f64 = 0.40;

const KFX_DRM_MARKERS: [&[u8]; 4] = [b"$DRM", b"DRM_V1", b"DRM_V2", b"kfx_drm"];

impl DocumentImporter for KfxImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "ebook-rs",
            formats: KFX_FORMATS,
            parser_version: "0.16.4",
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        if KfxBook::is_kfx(source.bytes.as_slice()) {
            ProbeResult {
                format: BookFormat::Kfx,
                confidence: ProbeConfidence::Magic,
                detail: Some("Amazon KFX CONT container".to_string()),
            }
        } else if source.extension().as_deref() == Some("kfx") {
            ProbeResult {
                format: BookFormat::Kfx,
                confidence: ProbeConfidence::Extension,
                detail: Some(".kfx extension without a CONT container signature".to_string()),
            }
        } else {
            ProbeResult::no_match(BookFormat::Kfx)
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        ensure!(
            KfxBook::is_kfx(source.bytes.as_slice()),
            "source is not an Amazon KFX CONT container"
        );
        AssetBudget::with_original(limits, source.bytes.len(), "KFX original source")?;
        if contains_kfx_drm_marker(source.bytes.as_slice()) {
            bail!("加密的 Amazon KFX 文件不受支持；请先移除 DRM 后再导入");
        }

        let parsed = KfxBook::from_bytes(source.bytes.as_slice()).map_err(|error| {
            let message = error.to_string();
            if message.contains("DRM") {
                anyhow::anyhow!("加密的 Amazon KFX 文件不受支持；请先移除 DRM 后再导入")
            } else {
                anyhow::anyhow!(message).context("ebook-rs 无法解析该 KFX 文件")
            }
        })?;
        ensure!(!parsed.sections.is_empty(), "KFX 文件不包含任何可读章节");
        ensure!(
            parsed.sections.len() <= limits.max_units,
            "KFX 文档章节数超过 {} 上限",
            limits.max_units
        );
        validate_kfx_text(&parsed.sections)?;

        let original = original_asset(source, BookFormat::Kfx, KFX_MEDIA_TYPE);
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        let metadata = &parsed.metadata;
        let title = if is_kfx_placeholder_title(&metadata.title) {
            safe_title(None, source.stem().as_str())
        } else {
            safe_title(Some(&metadata.title), source.stem().as_str())
        };

        let toc_titles = flatten_nav(&parsed.toc)
            .into_iter()
            .map(|point| (href_key(&point.href), point.label.clone()))
            .collect::<HashMap<_, _>>();
        let mut units = Vec::with_capacity(parsed.sections.len());
        let mut href_to_unit = HashMap::new();
        let mut total_text_bytes = 0_usize;
        for (index, section) in parsed.sections.iter().enumerate() {
            if section.raw_html.len() > limits.max_unit_text_bytes {
                bail!("KFX 第 {} 章超过文本安全上限", index + 1);
            }
            let unit_id = deterministic_id(
                "unit",
                format!("{book_id}\0kfx\0{}\0{}", index + 1, section.href).as_bytes(),
            );
            let section_title = toc_titles
                .get(&href_key(&section.href))
                .cloned()
                .unwrap_or_else(|| format!("Chapter {}", index + 1));
            let normalized = crate::markup::parse_source_for_unit(&section.raw_html, &unit_id)
                .with_context(|| format!("failed to normalize KFX section {}", index + 1))?;
            if normalized.canonical_source.len() > limits.max_unit_text_bytes {
                bail!("KFX 第 {} 章超过文本安全上限", index + 1);
            }
            total_text_bytes = total_text_bytes
                .checked_add(normalized.canonical_source.len())
                .context("total imported KFX text size overflowed")?;
            if total_text_bytes > limits.max_total_text_bytes {
                bail!("KFX 文档超过文本总大小安全上限");
            }
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
            .toc
            .iter()
            .filter_map(|point| kindle_toc_node(point, &href_to_unit, &book_id))
            .collect::<Vec<_>>();
        let toc = if toc.is_empty() {
            units
                .iter()
                .enumerate()
                .map(|(index, unit)| {
                    TocNode::new(
                        deterministic_id("toc", format!("{book_id}\0kfx\0{index}").as_bytes()),
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
                BookFormat::Kfx,
                original.metadata.id.clone(),
                source.file_name.clone(),
            ),
        );
        document.authors = metadata
            .creators
            .iter()
            .map(|author| author.trim().to_string())
            .filter(|author| {
                !author.is_empty() && !author.eq_ignore_ascii_case(KFX_PLACEHOLDER_AUTHOR)
            })
            .collect();
        document.language = metadata
            .languages
            .first()
            .map(|language| language.trim().to_string())
            .filter(|language| !language.is_empty());
        document.description = metadata
            .description
            .clone()
            .filter(|description| !description.trim().is_empty());
        document.units = units;
        document.toc = toc;
        document.assets = vec![original.metadata.clone()];
        Ok(ImportedBook {
            document,
            assets: vec![original],
        })
    }
}

fn is_kfx_placeholder_title(title: &str) -> bool {
    let trimmed = title.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case(KFX_PLACEHOLDER_TITLE)
}

/// Allocation-free substring scan: `KfxContainer::parse` performs the same
/// check with a lossy UTF-8 copy, but repeating it here keeps the rejection
/// message under this importer's control and avoids trusting upstream text.
fn contains_kfx_drm_marker(bytes: &[u8]) -> bool {
    KFX_DRM_MARKERS
        .iter()
        .any(|marker| bytes.windows(marker.len()).any(|window| window == *marker))
}

#[derive(Default)]
struct KfxTextQuality {
    visible: usize,
    unreadable: usize,
    word_like: usize,
}

/// Measures how much of the carved text is plausible prose. `plain_text` is
/// produced by the pinned parser from `raw_html` and is used here purely as a
/// quality signal; it never enters the canonical model.
fn kfx_text_quality(sections: &[ebook_rs::Section]) -> KfxTextQuality {
    let mut quality = KfxTextQuality::default();
    let mut word_run = 0_usize;
    for section in sections {
        for character in section.plain_text.chars() {
            if character.is_whitespace() {
                word_run = 0;
                continue;
            }
            quality.visible += 1;
            if is_unreadable_kfx_character(character) {
                quality.unreadable += 1;
            }
            if character.is_alphanumeric() {
                word_run += 1;
                if word_run >= 3 {
                    quality.word_like += 1;
                }
            } else {
                word_run = 0;
            }
        }
    }
    quality
}

fn is_unreadable_kfx_character(character: char) -> bool {
    character == '\u{FFFD}'
        || character.is_control()
        || matches!(
            u32::from(character),
            0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD
        )
}

fn validate_kfx_text(sections: &[ebook_rs::Section]) -> Result<()> {
    let quality = kfx_text_quality(sections);
    ensure!(
        quality.visible >= MIN_KFX_VISIBLE_CHARS,
        "KFX 未解析出可用正文（可能为加密或不受支持的 KFX 版本）"
    );
    let unreadable_ratio = quality.unreadable as f64 / quality.visible as f64;
    ensure!(
        unreadable_ratio <= MAX_KFX_UNREADABLE_RATIO,
        "KFX 正文中有 {:.1}% 的字节无法解码，已拒绝导入以避免生成乱码图书",
        unreadable_ratio * 100.0
    );
    let word_like_ratio = quality.word_like as f64 / quality.visible as f64;
    ensure!(
        word_like_ratio >= MIN_KFX_WORD_LIKE_RATIO,
        "KFX 未解析出可读的成词正文（{:.1}%），已拒绝导入以避免生成乱码图书",
        word_like_ratio * 100.0
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ebook_rs::kfx::{KfxContainer, KfxIndexEntry};

    use super::*;

    fn source(file_name: &str, bytes: Vec<u8>) -> ImportSource {
        ImportSource {
            file_name: Some(file_name.to_string()),
            bytes: Arc::new(bytes),
        }
    }

    fn container(payload: &[u8]) -> Vec<u8> {
        // `build` writes a well-formed CONT header plus the SHA-1 trailer, so
        // the fixture exercises the real container parser.
        KfxContainer::build(
            &[KfxIndexEntry {
                entity_id: 1,
                type_id: 0,
                offset: 0,
                length: payload.len() as u64,
            }],
            payload,
        )
    }

    /// 66 non-whitespace characters: long enough to clear the visible floor so
    /// the word-like guard is the check under test.
    const SYMBOL_SOUP: &str =
        "@#$%&()*+,/:;<=>?[]^_{|}~@#$%&()*+,/:;<=>?[]^_{|}~@#$%&()*+,/:;<=>?[]^_{|}~";

    fn section(plain_text: &str) -> ebook_rs::Section {
        ebook_rs::Section {
            index: 0,
            idref: "sec_0".to_string(),
            href: "sec_0.xhtml".to_string(),
            full_path: "OEBPS/sec_0.xhtml".to_string(),
            raw_html: String::new(),
            processed_html: String::new(),
            plain_text: plain_text.to_string(),
            plain_text_lower: plain_text.to_lowercase(),
            char_count: plain_text.chars().count(),
            viewport_width: None,
            viewport_height: None,
        }
    }

    fn prose_payload() -> Vec<u8> {
        b"title: KFX Fixture\nauthor: Ada Lovelace\n\nAlice was beginning to get very \
          tired of sitting by her sister on the bank, and of having nothing to do: once or \
          twice she had peeped into the book her sister was reading, but it had no pictures \
          or conversations in it, and what is the use of a book, thought Alice, without \
          pictures or conversations?\n\nSo she was considering in her own mind whether the \
          pleasure of making a daisy-chain would be worth the trouble of getting up.\n"
            .to_vec()
    }

    #[test]
    fn probes_the_cont_magic_instead_of_the_extension() {
        let source = source("renamed.bin", container(&prose_payload()));
        assert_eq!(
            KfxImporter.probe(&source).confidence,
            ProbeConfidence::Magic
        );
        assert_eq!(KfxImporter.probe(&source).format, BookFormat::Kfx);
    }

    #[test]
    fn probes_a_kfx_extension_without_the_container_magic() {
        let source = source("fake.kfx", b"not a container".to_vec());
        assert_eq!(
            KfxImporter.probe(&source).confidence,
            ProbeConfidence::Extension
        );
        assert!(
            KfxImporter
                .import(&source, &ImportLimits::default())
                .is_err()
        );
    }

    #[test]
    fn imports_carved_prose_and_falls_back_to_the_file_stem() {
        // The payload carries `title:`/`author:` pairs, which the pinned parser
        // reads; the stem stays the fallback for files without metadata.
        let source = source("moby-dick.kfx", container(&prose_payload()));
        let imported = KfxImporter
            .import(&source, &ImportLimits::default())
            .expect("import DRM-free KFX fixture");

        assert!(matches!(
            &imported.document.source,
            BookSource::Imported {
                format: BookFormat::Kfx,
                original_file_name,
                ..
            } if original_file_name.as_deref() == Some("moby-dick.kfx")
        ));
        assert_eq!(imported.document.title, "KFX Fixture");
        assert_eq!(imported.document.authors, vec!["Ada Lovelace".to_string()]);
        assert!(!imported.document.units.is_empty());
        assert_eq!(imported.document.units.len(), imported.document.toc.len());
        assert!(matches!(
            imported.document.units[0].source_locator,
            Some(SourceLocator::KindleSection { index: 1, .. })
        ));
        let original = imported
            .original_asset()
            .expect("the byte-exact original is retained");
        assert_eq!(original.metadata.media_type, KFX_MEDIA_TYPE);
        imported
            .validate(&ImportLimits::default())
            .expect("imported fixture satisfies the shared import contract");
    }

    #[test]
    fn keeps_the_file_stem_when_the_container_carries_no_metadata() {
        let payload = b"Alice was beginning to get very tired of sitting by her sister, and \
                        of having nothing to do; the bank was warm and the afternoon long, so \
                        she kept reading until the light began to fail behind the hedgerow."
            .repeat(2);
        let source = source("untitled.kfx", container(&payload));
        let imported = KfxImporter
            .import(&source, &ImportLimits::default())
            .expect("import container without metadata");
        assert_eq!(imported.document.title, "untitled");
        assert!(imported.document.authors.is_empty());
    }

    #[test]
    fn rejects_containers_that_carry_drm_markers() {
        let mut payload = prose_payload();
        payload.extend_from_slice(b"\n$DRM\n");
        let source = source("protected.kfx", container(&payload));
        let error = KfxImporter
            .import(&source, &ImportLimits::default())
            .expect_err("DRM-protected KFX must be rejected");
        assert!(error.to_string().contains("加密"), "{error}");
    }

    #[test]
    fn rejects_carved_symbol_soup_instead_of_importing_garbage() {
        let payload = b"@#$%&()*+,/:;<=>?[]^_{|}~".repeat(64);
        let source = source("garbage.kfx", container(&payload));
        let error = KfxImporter
            .import(&source, &ImportLimits::default())
            .expect_err("symbol soup must not become a book");
        assert!(error.to_string().contains("乱码"), "{error}");
    }

    #[test]
    fn rejects_containers_with_unreadable_bytes() {
        // Control bytes cannot be carved into fragments, so the parser falls
        // back to injecting the whole container as one paragraph; the resulting
        // replacement-character share must fail the quality guard.
        let payload = (0u8..=0x1f).collect::<Vec<_>>();
        let source = source("binary.kfx", container(&payload));
        let error = KfxImporter
            .import(&source, &ImportLimits::default())
            .expect_err("undecodable bytes must be rejected");
        assert!(
            error.to_string().contains("乱码") || error.to_string().contains("可用正文"),
            "{error}"
        );
    }

    #[test]
    fn quality_metrics_separate_prose_from_symbol_soup() {
        let prose_text =
            "plain readable prose with enough letters to count as a real paragraph for the guard";
        let prose = kfx_text_quality(&[section(prose_text)]);
        assert_eq!(prose.unreadable, 0);
        assert_eq!(
            prose.visible,
            prose_text
                .chars()
                .filter(|character| !character.is_whitespace())
                .count()
        );
        assert!(prose.word_like as f64 / prose.visible as f64 > MIN_KFX_WORD_LIKE_RATIO);
        validate_kfx_text(&[section(prose_text)]).expect("prose passes the quality guard");

        let soup = kfx_text_quality(&[section(SYMBOL_SOUP)]);
        assert_eq!(soup.word_like, 0);
        let error = validate_kfx_text(&[section(SYMBOL_SOUP)])
            .expect_err("symbol soup fails the quality guard");
        assert!(error.to_string().contains("乱码"), "{error}");

        // `\u{FFFD}` is not alphanumeric, so word runs reset and the word-like
        // ratio stays healthy; only the unreadable-byte share rejects this.
        let mut mangled_text = "readable ".repeat(12);
        mangled_text.push_str(&"\u{FFFD}".repeat(12));
        let mangled = kfx_text_quality(&[section(&mangled_text)]);
        assert_eq!(mangled.unreadable, 12);
        let error = validate_kfx_text(&[section(&mangled_text)])
            .expect_err("undecodable bytes fail the quality guard");
        assert!(error.to_string().contains("乱码"), "{error}");

        assert!(
            validate_kfx_text(&[section("short")]).is_err(),
            "below the visible-character floor the container has no usable text"
        );
    }

    #[test]
    fn unreadable_characters_cover_replacement_control_and_private_use() {
        assert!(is_unreadable_kfx_character('\u{FFFD}'));
        assert!(is_unreadable_kfx_character('\u{0007}'));
        assert!(is_unreadable_kfx_character('\u{E001}'));
        assert!(!is_unreadable_kfx_character('A'));
        assert!(!is_unreadable_kfx_character('中'));
    }
}
