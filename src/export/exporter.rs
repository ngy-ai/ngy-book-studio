//! The built-in exporter and how it dispatches on the format.

use super::*;

impl BuiltinDocumentExporter {
    /// Produces a complete artifact in memory. This is also useful for tests,
    /// background persistence, and future object-store destinations.
    pub fn export_bytes(
        &self,
        document: &BookDocument,
        format: ExportFormat,
        assets: &dyn AssetResolver,
    ) -> Result<Vec<u8>> {
        document.validate().context("无法导出无效的图书文档")?;
        match format {
            ExportFormat::Original => export_original(document, assets),
            ExportFormat::Epub => export_epub(document, assets),
            ExportFormat::Pdf => export_pdf(document, assets),
        }
    }
}

impl DocumentExporter for BuiltinDocumentExporter {
    fn export(
        &self,
        document: &BookDocument,
        format: ExportFormat,
        assets: &dyn AssetResolver,
        target: &Path,
    ) -> Result<()> {
        let bytes = self.export_bytes(document, format, assets)?;
        atomic_write(target, &bytes)
            .with_context(|| format!("无法写入导出文件：{}", target.display()))
    }
}
