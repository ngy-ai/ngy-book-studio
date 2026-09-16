//! DjVu import.
//!
//! DjVu is a scanned-page container: the page bitmaps are never stored in the
//! canonical model. The `ngy-djvu-png` visual renderer publishes one PNG per
//! page from the retained original, while the optional hidden text layer
//! (TXTz/TXTa) becomes the page text so full-text search, AI retrieval and
//! citations still work. Image-only pages import with empty text and stay
//! readable through the page-image window; no OCR is attempted.

use anyhow::{Context as _, Result, bail, ensure};
use djvu_rs::{
    DEFAULT_MAX_RENDER_PIXELS, DjVuBookmark, Document, ParseOptions, ResourceLimits, TextLayer,
};

use crate::{
    document::{
        BookDocument, BookFormat, BookSource, ContentUnit, ContentUnitKind, SourceLocator, TocNode,
        TocTarget, deterministic_id,
    },
    formats::{
        AssetBudget, DocumentImporter, ImportLimits, ImportSource, ImportedBook,
        ImporterCapabilities, ProbeConfidence, ProbeResult, original_asset, safe_title,
        text_block_document,
    },
};

#[derive(Clone, Copy, Debug, Default)]
pub struct DjvuImporter;

/// DjVu originals feed the same visual pipeline as PDFs, so they share its
/// source and page-count ceilings.
pub(crate) const MAX_DJVU_SOURCE_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_DJVU_PAGES: usize = 4_096;
/// Parse-time ceilings. They are deliberately generous for real scans while
/// still rejecting declared page/pixel bombs before any decoding happens.
const MAX_DJVU_PAGE_PIXELS: u64 = 100_000_000;
const MAX_DJVU_TOTAL_PIXELS: u64 = 20_000_000_000;
const MAX_DJVU_DECODED_BYTES: u64 = 1 << 30;
const MAX_DJVU_COMPONENTS: u64 = 20_000;

const DJVU_MEDIA_TYPE: &str = "image/vnd.djvu";

impl DocumentImporter for DjvuImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "djvu-rs",
            formats: &[BookFormat::Djvu],
            parser_version: "0.32.1",
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        if djvu_form_type(source.bytes.as_slice()).is_some() {
            ProbeResult {
                format: BookFormat::Djvu,
                confidence: ProbeConfidence::Magic,
                detail: Some("IFF FORM DjVu container".to_string()),
            }
        } else if source.extension().as_deref() == Some("djvu") {
            ProbeResult {
                format: BookFormat::Djvu,
                confidence: ProbeConfidence::Extension,
                detail: Some(".djvu extension without an IFF FORM header".to_string()),
            }
        } else {
            ProbeResult::no_match(BookFormat::Djvu)
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        let form = djvu_form_type(source.bytes.as_slice())
            .context("source does not have a DjVu FORM header")?;
        ensure!(
            source.bytes.len() <= MAX_DJVU_SOURCE_BYTES,
            "DjVu 原文件超过视觉管线的 {MAX_DJVU_SOURCE_BYTES} 字节上限"
        );
        AssetBudget::with_original(limits, source.bytes.len(), "DjVu original source")?;

        // `djvu-rs` takes ownership of the bytes (it moves them into a shared
        // backing for lazy page materialisation), so one copy of the source is
        // unavoidable. It happens only after the size ceilings above, keeping
        // the peak at twice the bounded source size.
        let owned = source.bytes.as_slice().to_vec();
        let parsed = Document::from_bytes_with_options(
            owned,
            &ParseOptions {
                limits: Some(djvu_resource_limits()),
            },
        )
        .map_err(|error| {
            let message = error.to_string();
            if form == b"DJVM" {
                anyhow::anyhow!(message).context(
                    "无法解析该 DjVu 文件；间接多文件 DjVu（DJVM 索引加分离页面文件）不受支持，请使用打包式 .djvu",
                )
            } else {
                anyhow::anyhow!(message).context("djvu-rs 无法解析该 DjVu 文件")
            }
        })?;

        let page_count = parsed.page_count();
        ensure!(page_count > 0, "DjVu 文件不包含任何页面");
        ensure!(
            page_count <= MAX_DJVU_PAGES,
            "DjVu 超过视觉管线的 {MAX_DJVU_PAGES} 页上限"
        );
        ensure!(
            page_count <= limits.max_units,
            "DjVu 文档页数超过 {} 上限",
            limits.max_units
        );

        let original = original_asset(source, BookFormat::Djvu, DJVU_MEDIA_TYPE);
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        let title = safe_title(None, source.stem().as_str());

        let mut units = Vec::with_capacity(page_count);
        let mut page_unit_ids = Vec::with_capacity(page_count);
        let mut page_toc = Vec::with_capacity(page_count);
        let mut total_text_bytes = 0_usize;
        for index in 0..page_count {
            let page = parsed
                .page(index)
                .with_context(|| format!("无法读取 DjVu 第 {} 页", index + 1))?;
            let layer = page
                .text_layer()
                .with_context(|| format!("无法读取 DjVu 第 {} 页文字层", index + 1))?;
            let text = paragraphs_from_text_layer(layer.as_ref()).join("\n\n");
            if text.len() > limits.max_unit_text_bytes {
                bail!("DjVu 第 {} 页文字超过文本安全上限", index + 1);
            }
            let page_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let unit_id = deterministic_id(
                "unit",
                format!("{book_id}\0djvu-page\0{page_number}").as_bytes(),
            );
            let page_title = format!("第 {page_number} 页");
            let page_document = text_block_document(&unit_id, &text);
            let html = crate::markup::serialize_source(&page_document)
                .with_context(|| format!("failed to normalize DjVu page {page_number} as HTML"))?;
            if html.len() > limits.max_unit_text_bytes {
                bail!("DjVu 第 {page_number} 页文字超过文本安全上限");
            }
            total_text_bytes = total_text_bytes
                .checked_add(html.len())
                .context("total imported DjVu text size overflowed")?;
            if total_text_bytes > limits.max_total_text_bytes {
                bail!("DjVu 文档超过文本总大小安全上限（第 {page_number} 页）");
            }
            units.push(
                ContentUnit::new(
                    unit_id.clone(),
                    ContentUnitKind::Page,
                    page_title.clone(),
                    html,
                    page_document,
                )
                .with_source_locator(SourceLocator::djvu_page(page_number)),
            );
            page_toc.push(TocNode::new(
                deterministic_id("toc", format!("{book_id}\0djvu\0{page_number}").as_bytes()),
                page_title,
                TocTarget::unit(unit_id.clone()),
            ));
            page_unit_ids.push(unit_id);
        }

        let toc = parsed
            .bookmarks()
            .map(|bookmarks| djvu_toc_nodes(&bookmarks, &book_id, &page_unit_ids, "root"))
            .unwrap_or_default();
        let toc = if toc.is_empty() { page_toc } else { toc };

        let mut document = BookDocument::new(
            book_id,
            title,
            BookSource::imported(
                BookFormat::Djvu,
                original.metadata.id.clone(),
                source.file_name.clone(),
            ),
        );
        document.units = units;
        document.toc = toc;
        document.assets = vec![original.metadata.clone()];
        Ok(ImportedBook {
            document,
            assets: vec![original],
        })
    }
}

fn djvu_resource_limits() -> ResourceLimits {
    ResourceLimits {
        max_file_bytes: Some(MAX_DJVU_SOURCE_BYTES as u64),
        // Kept at the pipeline ceiling rather than `limits.max_units` so the
        // per-configuration limit is reported by this importer's own
        // localized check below.
        max_pages: Some(MAX_DJVU_PAGES as u64),
        max_components: Some(MAX_DJVU_COMPONENTS),
        max_page_pixels: Some(MAX_DJVU_PAGE_PIXELS),
        max_total_pixels: Some(MAX_DJVU_TOTAL_PIXELS),
        max_decoded_bytes: Some(MAX_DJVU_DECODED_BYTES),
        max_render_pixels: Some(DEFAULT_MAX_RENDER_PIXELS),
    }
}

/// Returns the four-byte IFF form type of a DjVu container.
///
/// DjVu files are either bare IFF (`FORM` at offset 0) or carry the optional
/// `AT&T` prefix, after which `FORM` and its big-endian length follow.
pub(crate) fn djvu_form_type(bytes: &[u8]) -> Option<&[u8]> {
    let type_offset = if bytes.starts_with(b"AT&TFORM") {
        12
    } else if bytes.starts_with(b"FORM") {
        8
    } else {
        return None;
    };
    let form = bytes.get(type_offset..type_offset + 4)?;
    matches!(form, b"DJVU" | b"DJVM" | b"DJVI" | b"THUM").then_some(form)
}

/// Converts the hidden text layer into reflowable paragraphs. Pages without a
/// text layer yield no paragraphs, which is the normal case for pure scans.
fn paragraphs_from_text_layer(layer: Option<&TextLayer>) -> Vec<String> {
    let Some(layer) = layer else {
        return Vec::new();
    };
    layer
        .reflowable_text()
        .into_iter()
        .map(|paragraph| paragraph.text.trim().to_string())
        .filter(|paragraph| !paragraph.is_empty())
        .collect()
}

/// Maps NAVM bookmarks onto page content units.
///
/// A bundled DjVu bookmark URL targets its page with a trailing `#<page>`
/// fragment; anything else (component file names, indirect page ids) cannot be
/// resolved to a canonical page, so the node is skipped while its resolvable
/// children are kept.
fn djvu_toc_nodes(
    bookmarks: &[DjVuBookmark],
    book_id: &str,
    page_unit_ids: &[String],
    path: &str,
) -> Vec<TocNode> {
    let page_count = u32::try_from(page_unit_ids.len()).unwrap_or(u32::MAX);
    let mut nodes = Vec::new();
    for (index, bookmark) in bookmarks.iter().enumerate() {
        let children = djvu_toc_nodes(
            &bookmark.children,
            book_id,
            page_unit_ids,
            &format!("{path}.{index}"),
        );
        let Some(page) = djvu_bookmark_page(&bookmark.url, page_count) else {
            nodes.extend(children);
            continue;
        };
        let Some(unit_id) = page_unit_ids.get((page - 1) as usize) else {
            nodes.extend(children);
            continue;
        };
        let mut node = TocNode::new(
            deterministic_id(
                "toc",
                format!("{book_id}\0djvu\0{path}\0{index}\0{}", bookmark.title).as_bytes(),
            ),
            safe_title(Some(&bookmark.title), "未命名章节"),
            TocTarget::unit(unit_id.clone()),
        );
        node.children = children;
        nodes.push(node);
    }
    nodes
}

fn djvu_bookmark_page(url: &str, page_count: u32) -> Option<u32> {
    let (_, fragment) = url.rsplit_once('#')?;
    let fragment = fragment.trim();
    if fragment.is_empty() || !fragment.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let page = fragment.parse::<u32>().ok()?;
    (page >= 1 && page <= page_count).then_some(page)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use djvu_rs::{Bitmap, jb2_encode::BUNDLE_DEFAULT_DPI, text::TextZone};

    use super::*;

    fn djvm_fixture(page_count: u32) -> Vec<u8> {
        let pages = (0..page_count)
            .map(|index| {
                let mut page = Bitmap::new(64, 48);
                for y in 4..44 {
                    page.set_black(index % 60, y);
                    page.set_black(y % 60, 8);
                }
                page
            })
            .collect::<Vec<_>>();
        djvu_rs::jb2_encode::encode_djvm_bundle_jb2(
            &pages,
            page_count as usize + 1,
            BUNDLE_DEFAULT_DPI,
        )
    }

    fn import_source(file_name: &str, bytes: Vec<u8>) -> ImportSource {
        ImportSource {
            file_name: Some(file_name.to_string()),
            bytes: Arc::new(bytes),
        }
    }

    #[test]
    fn probes_the_iff_form_instead_of_the_extension() {
        let prefixed = import_source("renamed.bin", djvm_fixture(1));
        assert_eq!(DjvuImporter.probe(&prefixed).format, BookFormat::Djvu);
        assert_eq!(
            DjvuImporter.probe(&prefixed).confidence,
            ProbeConfidence::Magic
        );

        let bare = import_source("bare.bin", [b"FORM".as_slice(), &[0; 4], b"DJVU"].concat());
        assert_eq!(DjvuImporter.probe(&bare).confidence, ProbeConfidence::Magic);
    }

    #[test]
    fn probes_a_djvu_extension_without_the_form_header() {
        let fake = import_source("fake.djvu", b"not a djvu".to_vec());
        assert_eq!(
            DjvuImporter.probe(&fake).confidence,
            ProbeConfidence::Extension
        );
        assert!(
            DjvuImporter
                .import(&fake, &ImportLimits::default())
                .is_err()
        );
    }

    #[test]
    fn imports_one_page_unit_per_djvu_page_and_keeps_the_original() {
        let scan = import_source("scan.djvu", djvm_fixture(2));
        let imported = DjvuImporter
            .import(&scan, &ImportLimits::default())
            .expect("import two-page DjVu fixture");

        assert!(matches!(
            &imported.document.source,
            BookSource::Imported {
                format: BookFormat::Djvu,
                original_file_name,
                ..
            } if original_file_name.as_deref() == Some("scan.djvu")
        ));
        assert_eq!(imported.document.title, "scan");
        assert_eq!(imported.document.units.len(), 2);
        assert_eq!(imported.document.toc.len(), 2);
        for (index, unit) in imported.document.units.iter().enumerate() {
            assert_eq!(unit.kind, ContentUnitKind::Page);
            assert_eq!(unit.title, format!("第 {} 页", index + 1));
            assert_eq!(
                unit.source_locator,
                Some(SourceLocator::djvu_page((index + 1) as u32))
            );
            // The fixture is image-only, so the page text stays empty rather
            // than inventing content.
            assert!(unit.plain_text().is_empty());
        }
        let original = imported
            .original_asset()
            .expect("the byte-exact original is retained");
        assert_eq!(original.metadata.media_type, DJVU_MEDIA_TYPE);
        assert_eq!(original.metadata.byte_len, scan.bytes.len() as u64);
        imported
            .validate(&ImportLimits::default())
            .expect("imported fixture satisfies the shared import contract");
    }

    #[test]
    fn rejects_documents_above_the_page_limits() {
        let two_pages = import_source("two-pages.djvu", djvm_fixture(2));
        let limits = ImportLimits {
            max_units: 1,
            ..ImportLimits::default()
        };
        let error = DjvuImporter
            .import(&two_pages, &limits)
            .expect_err("page count above the unit limit must be rejected");
        assert!(error.to_string().contains("上限"), "{error}");
    }

    #[test]
    fn text_layer_paragraphs_become_page_text() {
        let layer = TextLayer {
            text: "First paragraph on the page\u{001d}Second paragraph after a break".to_string(),
            zones: vec![TextZone {
                kind: djvu_rs::TextZoneKind::Page,
                rect: djvu_rs::text::Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                text: String::new(),
                children: Vec::new(),
            }],
        };
        assert_eq!(
            paragraphs_from_text_layer(Some(&layer)),
            vec![
                "First paragraph on the page".to_string(),
                "Second paragraph after a break".to_string(),
            ]
        );
        assert!(paragraphs_from_text_layer(None).is_empty());
    }

    #[test]
    fn bookmarks_map_only_resolvable_page_fragments() {
        assert_eq!(djvu_bookmark_page("#2", 3), Some(2));
        assert_eq!(djvu_bookmark_page("book.djvu#1", 3), Some(1));
        assert_eq!(djvu_bookmark_page("#page2", 3), None);
        assert_eq!(djvu_bookmark_page("#4", 3), None);
        assert_eq!(djvu_bookmark_page("chapter1.xhtml", 3), None);
        assert_eq!(djvu_bookmark_page("#0", 3), None);

        let bookmarks = vec![
            DjVuBookmark {
                title: "第一章".to_string(),
                url: "#2".to_string(),
                children: vec![DjVuBookmark {
                    title: "未知目标".to_string(),
                    url: "part2.djvu".to_string(),
                    children: Vec::new(),
                }],
            },
            DjVuBookmark {
                title: "无目标".to_string(),
                url: "#9".to_string(),
                children: Vec::new(),
            },
        ];
        let units = vec!["unit-1".to_string(), "unit-2".to_string()];
        let nodes = djvu_toc_nodes(&bookmarks, "book-a", &units, "root");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].label, "第一章");
        assert_eq!(nodes[0].target, TocTarget::unit("unit-2".to_string()));
        assert!(nodes[0].children.is_empty());
    }
}
