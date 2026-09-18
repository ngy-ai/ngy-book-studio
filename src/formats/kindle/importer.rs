//! The document importer entry point for the Kindle formats.

use super::*;

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
