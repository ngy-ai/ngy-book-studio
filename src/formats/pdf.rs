use anyhow::{Context as _, Result, bail};

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

const MAX_PAGE_DECOMPRESSED_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_PDF_SOURCE_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_PDF_PAGES: u32 = 4_096;

#[derive(Clone, Copy, Debug, Default)]
pub struct PdfImporter;

impl DocumentImporter for PdfImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "lopdf",
            formats: &[BookFormat::Pdf],
            parser_version: "0.44.0",
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        if source.bytes.starts_with(b"%PDF-") {
            ProbeResult {
                format: BookFormat::Pdf,
                confidence: ProbeConfidence::Magic,
                detail: Some("PDF header is present".to_string()),
            }
        } else if source.extension().as_deref() == Some("pdf") {
            ProbeResult {
                format: BookFormat::Pdf,
                confidence: ProbeConfidence::Extension,
                detail: Some(".pdf extension without a PDF header".to_string()),
            }
        } else {
            ProbeResult::no_match(BookFormat::Pdf)
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        if !source.bytes.starts_with(b"%PDF-") {
            bail!("source does not have a PDF header");
        }
        AssetBudget::with_original(limits, source.bytes.len(), "PDF original source")?;
        validate_pdf_source_size(source.bytes.len())?;
        let pdf = lopdf::Document::load_mem(source.bytes.as_slice())
            .context("source is not a valid PDF")?;
        if pdf.is_encrypted() {
            bail!("encrypted PDF files are not supported");
        }
        let pages = pdf.get_pages();
        validate_pdf_page_count(pages.len(), limits)?;

        let original = original_asset(source, BookFormat::Pdf, "application/pdf");
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        let title = safe_title(None, source.stem().as_str());
        let mut units = Vec::with_capacity(pages.len());
        let mut toc = Vec::with_capacity(pages.len());
        let mut total_text_bytes = 0_usize;
        for (ordinal, page_number) in pages.keys().copied().enumerate() {
            let text = pdf
                .extract_text_with_limit(&[page_number], MAX_PAGE_DECOMPRESSED_BYTES)
                .with_context(|| format!("failed to extract PDF page {page_number}"))?;
            if text.len() > limits.max_unit_text_bytes {
                bail!("PDF page {page_number} exceeds the text safety limit");
            }
            let unit_id = deterministic_id(
                "unit",
                format!("{book_id}\0pdf-page\0{page_number}").as_bytes(),
            );
            let page_title = format!("Page {}", ordinal + 1);
            let page_document = text_block_document(&unit_id, &text);
            let html = crate::markup::serialize_source(&page_document)
                .with_context(|| format!("failed to normalize PDF page {page_number} as HTML"))?;
            if html.len() > limits.max_unit_text_bytes {
                bail!("PDF page {page_number} exceeds the text safety limit");
            }
            total_text_bytes = total_text_bytes
                .checked_add(html.len())
                .context("total imported PDF text size overflowed")?;
            if total_text_bytes > limits.max_total_text_bytes {
                bail!("PDF exceeds the total text safety limit at page {page_number}");
            }
            units.push(
                ContentUnit::new(
                    unit_id.clone(),
                    ContentUnitKind::Page,
                    page_title.clone(),
                    html,
                    page_document,
                )
                .with_source_locator(SourceLocator::pdf_page(page_number)),
            );
            toc.push(TocNode::new(
                deterministic_id("toc", format!("{book_id}\0{page_number}").as_bytes()),
                page_title,
                TocTarget::unit(unit_id),
            ));
        }

        let mut document = BookDocument::new(
            book_id,
            title,
            BookSource::imported(
                BookFormat::Pdf,
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

fn validate_pdf_source_size(byte_len: usize) -> Result<()> {
    if byte_len > MAX_PDF_SOURCE_BYTES {
        bail!("PDF 原文件超过视觉管线的 {MAX_PDF_SOURCE_BYTES} 字节上限");
    }
    Ok(())
}

fn validate_pdf_page_count(page_count: usize, limits: &ImportLimits) -> Result<()> {
    if page_count > limits.max_units {
        bail!("PDF contains too many pages");
    }
    if page_count > MAX_PDF_PAGES as usize {
        bail!("PDF 超过视觉管线的 {MAX_PDF_PAGES} 页上限");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, sync::Arc};

    use lopdf::{Document, Object, Stream, content::Content, dictionary};

    use super::*;

    #[test]
    fn probes_pdf_by_magic_instead_of_extension() {
        let source = ImportSource {
            file_name: Some("renamed.bin".into()),
            bytes: Arc::new(b"%PDF-1.7\ninvalid".to_vec()),
        };
        assert_eq!(
            PdfImporter.probe(&source).confidence,
            ProbeConfidence::Magic
        );
    }

    #[test]
    fn import_limits_match_the_required_visual_pipeline_boundaries() {
        validate_pdf_source_size(MAX_PDF_SOURCE_BYTES).unwrap();
        assert!(validate_pdf_source_size(MAX_PDF_SOURCE_BYTES + 1).is_err());

        let limits = ImportLimits::default();
        validate_pdf_page_count(MAX_PDF_PAGES as usize, &limits).unwrap();
        let error = validate_pdf_page_count(MAX_PDF_PAGES as usize + 1, &limits)
            .expect_err("a PDF that the visual pipeline cannot render must not import");
        assert!(error.to_string().contains("视觉管线"));

        let tighter = ImportLimits {
            max_units: 10,
            ..limits
        };
        assert!(validate_pdf_page_count(11, &tighter).is_err());
    }

    #[test]
    fn import_rejects_aggregate_text_before_accumulating_later_pages() {
        let source = ImportSource {
            file_name: Some("aggregate.pdf".into()),
            bytes: Arc::new(pdf_with_text_pages(&["first-page", "second-page"])),
        };
        let limits = ImportLimits {
            max_unit_text_bytes: "<p>second-page</p>".len(),
            max_total_text_bytes: "<p>first-page</p><p>second-page</p>".len(),
            ..ImportLimits::default()
        };
        let imported = PdfImporter
            .import(&source, &limits)
            .expect("canonical HTML fits the exact unit and aggregate budgets");
        assert_eq!(imported.document.units[0].source, "<p>first-page</p>");
        assert_eq!(imported.document.units[1].source, "<p>second-page</p>");

        let error = PdfImporter
            .import(
                &source,
                &ImportLimits {
                    max_total_text_bytes: limits.max_total_text_bytes - 1,
                    ..limits
                },
            )
            .expect_err("the second page must cross the aggregate text budget");
        assert!(error.to_string().contains("total text safety limit"));
        assert!(error.to_string().contains("page 2"));

        let error = PdfImporter
            .import(
                &source,
                &ImportLimits {
                    max_unit_text_bytes: limits.max_unit_text_bytes - 1,
                    ..limits
                },
            )
            .expect_err("the second page must cross the canonical HTML unit budget");
        assert!(
            error
                .to_string()
                .contains("page 2 exceeds the text safety limit")
        );
    }

    fn pdf_with_text_pages(texts: &[&str]) -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut page_ids = Vec::with_capacity(texts.len());
        for text in texts {
            let content = Content {
                operations: vec![
                    lopdf::content::Operation::new("BT", vec![]),
                    lopdf::content::Operation::new("Tf", vec!["F1".into(), 18.into()]),
                    lopdf::content::Operation::new("Td", vec![72.into(), 720.into()]),
                    lopdf::content::Operation::new("Tj", vec![Object::string_literal(*text)]),
                    lopdf::content::Operation::new("ET", vec![]),
                ],
            };
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                content.encode().expect("encode test PDF content"),
            ));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            });
            page_ids.push(page_id.into());
        }
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids,
                "Count" => texts.len() as i64,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut output = Cursor::new(Vec::new());
        document.save_to(&mut output).expect("write test PDF");
        output.into_inner()
    }
}
