//! The registry that dispatches an import to a format importer.

use super::*;

#[derive(Clone, Default)]
pub struct FormatRegistry {
    pub(crate) importers: Vec<Arc<dyn DocumentImporter>>,
}

impl std::fmt::Debug for FormatRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FormatRegistry")
            .field("importer_count", &self.importers.len())
            .finish()
    }
}

impl FormatRegistry {
    pub fn with_builtin_importers() -> Self {
        let mut registry = Self::default();
        registry.register(EpubImporter);
        registry.register(PdfImporter);
        registry.register(OfficeImporter);
        registry.register(KindleImporter);
        registry.register(KfxImporter);
        registry.register(DjvuImporter);
        registry
    }

    pub fn register(&mut self, importer: impl DocumentImporter + 'static) {
        self.importers.push(Arc::new(importer));
    }

    pub fn capabilities(&self) -> Vec<ImporterCapabilities> {
        self.importers
            .iter()
            .map(|importer| importer.capabilities())
            .collect()
    }

    pub fn probe(&self, source: &ImportSource) -> Vec<ProbeResult> {
        let mut results = self
            .importers
            .iter()
            .map(|importer| importer.probe(source))
            .filter(|result| result.confidence != ProbeConfidence::NoMatch)
            .collect::<Vec<_>>();
        results.sort_by_key(|result| Reverse(result.confidence));
        results
    }

    pub fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook> {
        tracing::info!(
            target: "ngy_import",
            file_name = ?source.file_name,
            extension = ?source.extension(),
            bytes = source.bytes.len(),
            "导入：读取源文件完成，开始探测格式"
        );
        if source.bytes.len() as u64 > limits.max_source_bytes {
            bail!("import source exceeds the configured size limit");
        }
        AssetBudget::with_original(limits, source.bytes.len(), "import source")?;
        let candidates = self
            .importers
            .iter()
            .map(|importer| (importer, importer.probe(source)))
            .filter(|(_, probe)| probe.confidence != ProbeConfidence::NoMatch)
            .collect::<Vec<_>>();
        for (importer, probe) in &candidates {
            tracing::debug!(
                target: "ngy_import",
                importer = importer.capabilities().importer,
                format = ?probe.format,
                confidence = ?probe.confidence,
                detail = ?probe.detail,
                "导入：候选解析器"
            );
        }
        let best = candidates
            .iter()
            .map(|(_, probe)| probe.confidence)
            .max()
            .context("unsupported book format")?;
        let best_candidates = candidates
            .iter()
            .filter(|(_, probe)| probe.confidence == best)
            .collect::<Vec<_>>();
        if best_candidates.len() != 1 {
            let formats = best_candidates
                .iter()
                .map(|(_, probe)| format!("{:?}", probe.format))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("book format is ambiguous between: {formats}");
        }
        let (importer, probe) = best_candidates[0];
        tracing::info!(
            target: "ngy_import",
            importer = importer.capabilities().importer,
            format = ?probe.format,
            confidence = ?probe.confidence,
            detail = ?probe.detail,
            "导入：已选定解析器"
        );
        let imported = importer.import(source, limits).inspect_err(|error| {
            tracing::error!(
                target: "ngy_import",
                importer = importer.capabilities().importer,
                error = %format!("{error:#}"),
                "导入：解析器返回错误"
            );
        })?;
        imported.validate(limits)?;
        tracing::info!(
            target: "ngy_import",
            book_id = %imported.document.id,
            title = %imported.document.title,
            units = imported.document.units.len(),
            assets = imported.document.assets.len(),
            "导入：解析与安全校验完成"
        );
        Ok(imported)
    }

    pub fn import_path(&self, path: &Path, limits: &ImportLimits) -> Result<ImportedBook> {
        tracing::info!(target: "ngy_import", path = %path.display(), "导入：开始读取文件");
        let source = ImportSource::from_path(path, limits)?;
        self.import(&source, limits)
            .with_context(|| format!("failed to import {}", path.display()))
    }
}
