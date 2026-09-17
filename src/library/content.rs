//! Reading a stored book back out, and exporting it.

use super::*;

impl LibraryStore {
    pub fn source_bytes(&self, book_id: &str) -> Result<Vec<u8>> {
        let record = self.book_record(book_id)?;
        self.read_blob(&record.source_object_key)
    }

    pub fn epub_bytes(&self, book_id: &str) -> Result<Vec<u8>> {
        let record = self.book_record(book_id)?;
        ensure!(record.format == "epub", "当前图书不是 EPUB 投影");
        self.read_blob(&record.source_object_key)
    }

    /// Returns an EPUB view projection for every supported format. Native
    /// imports are rendered from the canonical block model without replacing
    /// their immutable original or changing the current database revision.
    pub fn reader_epub_bytes(&self, book_id: &str) -> Result<Vec<u8>> {
        let record = self.book_record(book_id)?;
        if record.format == "epub" {
            return self.read_blob(&record.source_object_key);
        }
        let document = self.document(book_id)?;
        let resolver = |asset_id: &str| self.asset_bytes(book_id, asset_id);
        BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Epub, &resolver)
            .context("无法生成结构化阅读预览")
    }

    pub fn document(&self, book_id: &str) -> Result<BookDocument> {
        let record = self.book_record(book_id)?;
        let conn = db::open_conn(&self.db_path)?;
        let source = db::book_sources::get_revision(&conn, book_id, record.revision)?
            .context("当前图书来源不存在")?;
        let source_history = db::book_sources::list_for_book(&conn, book_id)?;
        let unit_rows = db::content_units::list_for_source(&conn, &source.id)?;
        let asset_rows = db::assets::list_for_source(&conn, &source.id)?;
        let toc_rows = db::toc_entries::list_for_source(&conn, &source.id)?;
        let original_source = source_history
            .iter()
            .find(|candidate| candidate.source_kind == "original");

        let mut assets = Vec::with_capacity(asset_rows.len());
        for asset in &asset_rows {
            let blob = db::blobs::get(&conn, &asset.object_key)?.context("资源对象元数据不存在")?;
            assets.push(DocumentAsset {
                id: asset.id.clone(),
                roles: asset_roles_from_kind(&asset.kind),
                media_type: asset.media_type.clone(),
                original_file_name: (asset.kind == "original_source")
                    .then(|| original_source.and_then(|source| source.source_name.clone()))
                    .flatten(),
                byte_len: asset.byte_len,
                content_hash: blob.hash,
            });
        }
        let original_asset_id = asset_rows
            .iter()
            .find(|asset| asset.kind == "original_source")
            .map(|asset| asset.id.clone());
        let document_source = if original_source.is_none() {
            DocumentSource::Created
        } else {
            let original_source = original_source.expect("checked above");
            DocumentSource::imported(
                parse_format(&original_source.format)?,
                original_asset_id.context("导入图书缺少原件资源")?,
                original_source.source_name.clone(),
            )
        };
        let mut document =
            BookDocument::new(record.id.clone(), record.title.clone(), document_source);
        document.revision = Revision::new(record.revision);
        document.authors = split_authors(&record.author);
        document.language = record.language.clone();
        document.description = record.description.clone();
        document.cover_asset_id = record.cover_asset_id.clone();
        document.assets = assets;
        document.units = unit_rows
            .into_iter()
            .map(document_unit_from_row)
            .collect::<Result<_>>()?;
        document.toc = restore_toc(toc_rows);
        document.validate()?;
        Ok(document)
    }

    pub fn export_epub(&self, book_id: &str, target: &Path) -> Result<u64> {
        self.export_as(book_id, ExportFormat::Epub, target)
    }

    pub fn export_pdf(&self, book_id: &str, target: &Path) -> Result<u64> {
        self.export_as(book_id, ExportFormat::Pdf, target)
    }

    pub fn export_original(&self, book_id: &str, target: &Path) -> Result<u64> {
        self.export_as(book_id, ExportFormat::Original, target)
    }

    pub fn export_as(&self, book_id: &str, format: ExportFormat, target: &Path) -> Result<u64> {
        let document = self.document(book_id)?;
        let resolver = |asset_id: &str| self.asset_bytes(book_id, asset_id);
        let bytes = BuiltinDocumentExporter.export_bytes(&document, format, &resolver)?;
        crate::export::atomic_write(target, &bytes)?;
        Ok(bytes.len() as u64)
    }
}
pub(crate) fn generated_epub(document: &BookDocument) -> Result<Vec<u8>> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut archive = zip::ZipWriter::new(&mut output);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file(
            "mimetype",
            options.compression_method(zip::CompressionMethod::Stored),
        )?;
        archive.write_all(b"application/epub+zip")?;
        archive.start_file("META-INF/container.xml", options)?;
        archive.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#)?;
        archive.start_file("OEBPS/content.opf", options)?;
        let author = document.authors.first().map(String::as_str).unwrap_or("");
        let opf = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">{}</dc:identifier><dc:title>{}</dc:title><dc:creator>{}</dc:creator><dc:language>{}</dc:language><meta property="dcterms:modified">2026-01-01T00:00:00Z</meta></metadata><manifest><item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter1"/></spine></package>"#,
            xml_escape(&document.id),
            xml_escape(&document.title),
            xml_escape(author),
            xml_escape(document.language.as_deref().unwrap_or("zh-CN")),
        );
        archive.write_all(opf.as_bytes())?;
        archive.start_file("OEBPS/chapter1.xhtml", options)?;
        let unit = document.units.first().context("新建图书缺少默认章节")?;
        let html = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>{}</title></head><body><h1>{}</h1><p>开始写作……</p></body></html>"#,
            xml_escape(&unit.title),
            xml_escape(&unit.title)
        );
        archive.write_all(html.as_bytes())?;
        archive.finish()?;
    }
    Ok(output.into_inner())
}
