//! The Office importer: probing the archive and building the unit graph.

use super::*;

impl DocumentImporter for OfficeImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "office_oxide",
            formats: OFFICE_FORMATS,
            parser_version: office_oxide::VERSION,
        }
    }

    fn probe(&self, source: &ImportSource) -> ProbeResult {
        if source.bytes.starts_with(b"PK\x03\x04") {
            if zip_contains(source.bytes.as_slice(), "word/document.xml") {
                return matched(BookFormat::Docx, "OOXML Word container");
            }
            if zip_contains(source.bytes.as_slice(), "ppt/presentation.xml") {
                return matched(BookFormat::Pptx, "OOXML PowerPoint container");
            }
            if zip_contains(source.bytes.as_slice(), "xl/workbook.xml") {
                return matched(BookFormat::Xlsx, "OOXML Excel container");
            }
        }
        if source.bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0])
            && source.extension().as_deref() == Some("doc")
        {
            return matched(BookFormat::Doc, "OLE Compound File Word document");
        }
        let format = match source.extension().as_deref() {
            Some("doc") => BookFormat::Doc,
            Some("docx") => BookFormat::Docx,
            Some("pptx") => BookFormat::Pptx,
            Some("xlsx") => BookFormat::Xlsx,
            _ => return ProbeResult::no_match(BookFormat::Docx),
        };
        ProbeResult {
            format,
            confidence: ProbeConfidence::Extension,
            detail: Some("Office extension without a matching container signature".to_string()),
        }
    }

    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        let probe = self.probe(source);
        if probe.confidence < ProbeConfidence::Container {
            bail!("Office file signature does not match its supported format");
        }
        let mut asset_budget =
            AssetBudget::with_original(limits, source.bytes.len(), "Office original source")?;
        if source.bytes.starts_with(b"PK\x03\x04") {
            validate_ooxml_archive(source.bytes.as_slice(), limits)?;
        }
        let (format, office_format, media_type) = match probe.format {
            BookFormat::Doc => (BookFormat::Doc, DocumentFormat::Doc, "application/msword"),
            BookFormat::Docx => (
                BookFormat::Docx,
                DocumentFormat::Docx,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            BookFormat::Pptx => (
                BookFormat::Pptx,
                DocumentFormat::Pptx,
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            ),
            BookFormat::Xlsx => (
                BookFormat::Xlsx,
                DocumentFormat::Xlsx,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            _ => bail!("unsupported Office format"),
        };
        let parsed =
            Document::from_reader(Cursor::new(source.bytes.as_ref().clone()), office_format)
                .context("office_oxide could not parse the document")?;
        let ir = parsed.to_ir();
        let original = original_asset(source, format, media_type);
        let book_id = deterministic_id("book", original.metadata.content_hash.as_bytes());
        // Excel 工作簿的 core properties 常常为空或沿用模板标题，导入时一律以
        // 文件名作为书名，避免把模板里的标题当成用户图书名。
        let title = if format == BookFormat::Xlsx {
            safe_title(None, source.stem().as_str())
        } else {
            safe_title(ir.metadata.title.as_deref(), source.stem().as_str())
        };

        let mut units = Vec::new();
        let mut total_text_bytes = 0;
        let mut imported_assets = vec![original.clone()];
        if ir.sections.is_empty() {
            let html = parsed.to_html();
            push_office_units(
                &book_id,
                format,
                1,
                None,
                &html,
                Vec::new(),
                None,
                limits,
                &mut total_text_bytes,
                &mut units,
            )?;
        } else {
            for (section_index, section) in ir.sections.iter().enumerate() {
                let mut one_section = DocumentIR {
                    metadata: ir.metadata.clone(),
                    sections: vec![section.clone()],
                };
                if matches!(format, BookFormat::Doc | BookFormat::Docx) {
                    // Word section titles repeat the first body heading in the
                    // parser IR. Keep them as fallback metadata; rendering an
                    // extra h2 would duplicate content and create a false split.
                    one_section.sections[0].title = None;
                }
                let (html, worksheet_range) = if format == BookFormat::Xlsx {
                    if let Some(xlsx) = parsed.as_xlsx() {
                        if let Some(worksheet) = xlsx.worksheets.get(section_index) {
                            let used_range = worksheet_used_range(worksheet, xlsx);
                            (
                                worksheet_grid_html(worksheet, xlsx, used_range, limits)?,
                                used_range.map(|range| range.to_a1()),
                            )
                        } else {
                            (one_section.to_html(), None)
                        }
                    } else {
                        (one_section.to_html(), None)
                    }
                } else {
                    (one_section.to_html(), None)
                };
                let image_assets = extract_section_images(
                    &book_id,
                    section_index,
                    section,
                    &mut asset_budget,
                    &mut imported_assets,
                )?;
                push_office_units(
                    &book_id,
                    format,
                    section_index + 1,
                    section.title.as_deref(),
                    &html,
                    image_assets,
                    worksheet_range.as_deref(),
                    limits,
                    &mut total_text_bytes,
                    &mut units,
                )?;
            }
        }
        if units.is_empty() {
            let unit_id = deterministic_id("unit", format!("{book_id}\0empty").as_bytes());
            units.push(
                ContentUnit::empty(unit_id, unit_kind(format), title.clone())
                    .with_source_locator(source_locator(format, 1, None, None)),
            );
        }

        let toc = units
            .iter()
            .enumerate()
            .map(|(index, unit)| {
                TocNode::new(
                    deterministic_id("toc", format!("{book_id}\0{index}").as_bytes()),
                    unit.title.clone(),
                    TocTarget::unit(unit.id.clone()),
                )
            })
            .collect();
        let mut document = BookDocument::new(
            book_id,
            title,
            BookSource::imported(
                format,
                original.metadata.id.clone(),
                source.file_name.clone(),
            ),
        );
        if let Some(author) = ir
            .metadata
            .author
            .as_deref()
            .map(str::trim)
            .filter(|author| !author.is_empty())
        {
            document.authors.push(author.to_string());
        }
        document.description = ir.metadata.description.clone();
        document.units = units;
        document.toc = toc;
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
