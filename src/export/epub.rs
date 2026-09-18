//! Writing the EPUB container and its package document.

use super::*;

pub(crate) fn export_epub(
    document: &BookDocument,
    resolver: &dyn AssetResolver,
) -> Result<Vec<u8>> {
    ensure!(!document.units.is_empty(), "无法导出没有章节或页面的 EPUB");

    let mut epub_assets = Vec::new();
    let mut asset_hrefs = HashMap::new();
    for metadata in document.assets.iter().filter(|asset| {
        asset
            .roles
            .iter()
            .any(|role| *role != AssetRole::OriginalSource)
    }) {
        let sequence = epub_assets.len() + 1;
        let href = format!(
            "assets/asset-{sequence:04}.{}",
            extension_for_media_type(&metadata.media_type)
        );
        let bytes = resolve_verified(metadata, resolver)?;
        asset_hrefs.insert(metadata.id.as_str(), format!("../{href}"));
        epub_assets.push(EpubAsset {
            metadata,
            manifest_id: format!("asset-{sequence:04}"),
            href,
            bytes,
        });
    }

    let unit_hrefs = document
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            (
                unit.id.as_str(),
                format!("text/unit-{:04}.xhtml", index + 1),
            )
        })
        .collect::<HashMap<_, _>>();

    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);

    write_zip_entry(&mut archive, "mimetype", EPUB_MIME, options)?;
    write_zip_entry(
        &mut archive,
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        options,
    )?;
    write_zip_entry(
        &mut archive,
        "EPUB/styles/book.css",
        EPUB_STYLE.as_bytes(),
        options,
    )?;

    let package = render_package(document, &epub_assets);
    write_zip_entry(
        &mut archive,
        "EPUB/package.opf",
        package.as_bytes(),
        options,
    )?;
    let navigation = render_navigation(document, &unit_hrefs)?;
    write_zip_entry(
        &mut archive,
        "EPUB/nav.xhtml",
        navigation.as_bytes(),
        options,
    )?;

    for (index, unit) in document.units.iter().enumerate() {
        let chapter = render_epub_unit(document, unit, &asset_hrefs)?;
        write_zip_entry(
            &mut archive,
            &format!("EPUB/text/unit-{:04}.xhtml", index + 1),
            chapter.as_bytes(),
            options,
        )?;
    }
    for asset in &epub_assets {
        write_zip_entry(
            &mut archive,
            &format!("EPUB/{}", asset.href),
            &asset.bytes,
            options,
        )?;
    }

    let cursor = archive.finish().context("无法完成 EPUB ZIP 容器")?;
    Ok(cursor.into_inner())
}

pub(crate) fn write_zip_entry(
    archive: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<()> {
    archive
        .start_file(path, options)
        .with_context(|| format!("无法创建 EPUB 条目 {path}"))?;
    archive
        .write_all(bytes)
        .with_context(|| format!("无法写入 EPUB 条目 {path}"))?;
    Ok(())
}

pub(crate) fn render_package(document: &BookDocument, assets: &[EpubAsset<'_>]) -> String {
    let language = document.language.as_deref().unwrap_or("und");
    let mut output = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id" xml:lang="{}">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:ngy:{}</dc:identifier>
    <dc:title>{}</dc:title>
    <dc:language>{}</dc:language>
"#,
        escape_xml(language),
        escape_xml(&document.id),
        escape_xml(&document.title),
        escape_xml(language),
    );
    for author in &document.authors {
        output.push_str(&format!(
            "    <dc:creator>{}</dc:creator>\n",
            escape_xml(author)
        ));
    }
    if let Some(description) = document.description.as_deref() {
        output.push_str(&format!(
            "    <dc:description>{}</dc:description>\n",
            escape_xml(description)
        ));
    }
    output.push_str("    <meta property=\"dcterms:modified\">2000-01-01T00:00:00Z</meta>\n");
    if let Some(cover) = document.cover_asset_id.as_deref()
        && let Some(asset) = assets.iter().find(|asset| asset.metadata.id == cover)
    {
        output.push_str(&format!(
            "    <meta name=\"cover\" content=\"{}\"/>\n",
            asset.manifest_id
        ));
    }
    output.push_str("  </metadata>\n  <manifest>\n");
    output.push_str("    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    output.push_str("    <item id=\"style\" href=\"styles/book.css\" media-type=\"text/css\"/>\n");
    for (index, _) in document.units.iter().enumerate() {
        output.push_str(&format!(
            "    <item id=\"unit-{0:04}\" href=\"text/unit-{0:04}.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
            index + 1
        ));
    }
    for asset in assets {
        let cover_property =
            if document.cover_asset_id.as_deref() == Some(asset.metadata.id.as_str()) {
                " properties=\"cover-image\""
            } else {
                ""
            };
        output.push_str(&format!(
            "    <item id=\"{}\" href=\"{}\" media-type=\"{}\"{cover_property}/>\n",
            asset.manifest_id,
            escape_xml(&asset.href),
            escape_xml(base_media_type(&asset.metadata.media_type)),
        ));
    }
    output.push_str("  </manifest>\n  <spine>\n");
    for (index, _) in document.units.iter().enumerate() {
        output.push_str(&format!("    <itemref idref=\"unit-{:04}\"/>\n", index + 1));
    }
    output.push_str("  </spine>\n</package>\n");
    output
}
