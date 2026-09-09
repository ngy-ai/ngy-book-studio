//! Stable exports from the application's canonical document model.
//!
//! Exporters resolve immutable assets through [`AssetResolver`]. They neither
//! know object-store paths nor expose third-party document representations.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::{Cursor, Write},
    path::Path,
};

use anyhow::{Context as _, Result, bail, ensure};
use image::{ImageReader, Limits};
use tempfile::NamedTempFile;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::document::{
    AssetRef, AssetRole, Block, BookDocument, BookSource, Inline, ListItem, TableRow, TocNode,
    TocTarget,
};

const EPUB_MIME: &[u8] = b"application/epub+zip";
const EPUB_STYLE: &str = r#"body { margin: 5%; line-height: 1.65; }
img, video { max-width: 100%; height: auto; }
table { border-collapse: collapse; max-width: 100%; }
th, td { border: 1px solid #888; padding: 0.25em 0.5em; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; }
figure { margin: 1em 0; }
figcaption { color: #555; font-size: 0.9em; }
"#;
const PDF_WIDTH: f32 = 595.0;
const PDF_HEIGHT: f32 = 842.0;
const PDF_MARGIN: f32 = 48.0;
const PDF_LINES_PER_PAGE: usize = 49;
const PDF_LINE_WIDTH: usize = 84;
const MAX_PDF_IMAGE_DIMENSION: u32 = 10_000;
const MAX_PDF_IMAGE_ALLOCATION: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// The immutable source object. This is the only export that promises byte
    /// identity with an imported file.
    Original,
    /// A normalized EPUB 3 generated from the canonical block model.
    Epub,
    /// A self-contained PDF generated without an external Office/PDF program.
    Pdf,
}

impl ExportFormat {
    pub const fn extension(self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Epub => Some("epub"),
            Self::Pdf => Some("pdf"),
        }
    }

    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Original => "application/octet-stream",
            Self::Epub => "application/epub+zip",
            Self::Pdf => "application/pdf",
        }
    }
}

/// Resolves an opaque domain asset ID to immutable bytes.
///
/// Implementations may use SQLite plus [`crate::storage::BlobStore`], memory,
/// or a future remote backend. Paths and object-store keys remain outside the
/// document/export boundary.
pub trait AssetResolver: Send + Sync {
    fn resolve(&self, asset_id: &str) -> Result<Vec<u8>>;
}

impl<F> AssetResolver for F
where
    F: Fn(&str) -> Result<Vec<u8>> + Send + Sync,
{
    fn resolve(&self, asset_id: &str) -> Result<Vec<u8>> {
        self(asset_id)
    }
}

/// Narrow export boundary shared by library, reader, and editor workflows.
///
/// This operation is intentionally synchronous. Callers must run it on the
/// application I/O runtime or another worker, never in a GPUI render/update
/// borrow.
pub trait DocumentExporter: Send + Sync {
    fn export(
        &self,
        document: &BookDocument,
        format: ExportFormat,
        assets: &dyn AssetResolver,
        target: &Path,
    ) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinDocumentExporter;

/// Convenient name for the built-in stable exporter.
pub type DefaultDocumentExporter = BuiltinDocumentExporter;

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

/// Replaces `target` only after a complete same-directory temporary file has
/// been flushed and synchronized. `tempfile` uses `MoveFileExW` with replace
/// semantics on Windows and rename semantics on Unix.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with_syncs(
        target,
        bytes,
        std::fs::File::sync_all,
        sync_parent_directory,
    )
}

fn atomic_write_with_syncs<PreCommitSync, PostCommitSync>(
    target: &Path,
    bytes: &[u8],
    pre_commit_sync: PreCommitSync,
    post_commit_sync: PostCommitSync,
) -> Result<()>
where
    PreCommitSync: FnOnce(&std::fs::File) -> std::io::Result<()>,
    PostCommitSync: FnOnce(&Path) -> std::io::Result<()>,
{
    ensure!(!target.as_os_str().is_empty(), "导出目标路径不能为空");
    ensure!(target.file_name().is_some(), "导出目标必须是文件路径");
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure!(
        parent.is_dir(),
        "导出目标目录不存在或不是目录：{}",
        parent.display()
    );

    let mut staged = NamedTempFile::new_in(parent)
        .with_context(|| format!("无法在 {} 创建导出临时文件", parent.display()))?;
    staged.write_all(bytes).context("无法写入导出临时文件")?;
    staged.flush().context("无法刷新导出临时文件")?;
    pre_commit_sync(staged.as_file()).context("无法同步导出临时文件")?;
    let persisted = staged
        .persist(target)
        .map_err(|error| error.error)
        .with_context(|| format!("无法原子替换导出目标：{}", target.display()))?;

    // `persist` is the commit point. The complete data file was synchronized
    // above while it still had its temporary name, so no data-file sync that
    // can fail is allowed after this point: returning `Err` now would tell the
    // caller that the old target survived even though it has already been
    // replaced. A parent-directory sync only strengthens rename durability and
    // is therefore best-effort after commit.
    drop(persisted);
    if let Err(error) = post_commit_sync(parent) {
        tracing::warn!(
            target = %target.display(),
            directory = %parent.display(),
            %error,
            "导出文件已提交，但无法同步目标目录元数据"
        );
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

// Rust's standard library does not expose a portable directory fsync on
// Windows. The staged data itself is still fully synchronized before the
// atomic replacement; directory-entry durability remains an OS/filesystem
// property on this platform rather than something this helper can promise.
#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

fn export_original(document: &BookDocument, assets: &dyn AssetResolver) -> Result<Vec<u8>> {
    let BookSource::Imported {
        original_asset_id, ..
    } = &document.source
    else {
        bail!("这本图书是应用内新建的，没有可导出的原文件；请选择 EPUB 或 PDF")
    };
    let metadata = document
        .find_asset(original_asset_id)
        .context("图书记录缺少原文件对象")?;
    ensure!(
        metadata.has_role(AssetRole::OriginalSource),
        "原文件对象的用途标记无效"
    );
    resolve_verified(metadata, assets)
}

fn resolve_verified(metadata: &AssetRef, resolver: &dyn AssetResolver) -> Result<Vec<u8>> {
    let bytes = resolver
        .resolve(&metadata.id)
        .with_context(|| format!("无法解析资源 {}", metadata.id))?;
    ensure!(
        bytes.len() as u64 == metadata.byte_len,
        "资源 {} 的长度与数据库记录不一致",
        metadata.id
    );
    let digest = blake3::hash(&bytes).to_hex().to_string();
    ensure!(
        digest.eq_ignore_ascii_case(&metadata.content_hash),
        "资源 {} 的 BLAKE3 摘要与数据库记录不一致",
        metadata.id
    );
    Ok(bytes)
}

struct EpubAsset<'a> {
    metadata: &'a AssetRef,
    manifest_id: String,
    href: String,
    bytes: Vec<u8>,
}

fn export_epub(document: &BookDocument, resolver: &dyn AssetResolver) -> Result<Vec<u8>> {
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

fn write_zip_entry(
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

fn render_package(document: &BookDocument, assets: &[EpubAsset<'_>]) -> String {
    let language = document.language.as_deref().unwrap_or("und");
    let mut output = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id" xml:lang="{}">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:moye:{}</dc:identifier>
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

fn render_navigation(
    document: &BookDocument,
    unit_hrefs: &HashMap<&str, String>,
) -> Result<String> {
    let language = document.language.as_deref().unwrap_or("und");
    let mut output = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="{}" xml:lang="{}">
<head><meta charset="utf-8"/><title>{}</title></head>
<body><nav epub:type="toc" id="toc"><h1>目录</h1><ol>"#,
        escape_xml(language),
        escape_xml(language),
        escape_xml(&document.title),
    );
    if document.toc.is_empty() {
        for unit in &document.units {
            let href = unit_hrefs
                .get(unit.id.as_str())
                .context("EPUB 目录引用了不存在的内容单元")?;
            output.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_xml(href),
                escape_xml(&unit.title)
            ));
        }
    } else {
        render_toc_nodes(&document.toc, unit_hrefs, &mut output)?;
    }
    output.push_str("</ol></nav></body></html>\n");
    Ok(output)
}

fn render_toc_nodes(
    nodes: &[TocNode],
    unit_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for node in nodes {
        let unit_id = node.target.unit_id();
        let mut href = unit_hrefs
            .get(unit_id)
            .with_context(|| format!("EPUB 目录引用了不存在的内容单元 {unit_id}"))?
            .clone();
        if let TocTarget::Block { block_id, .. } = &node.target {
            href.push('#');
            href.push_str(&anchor_for_id(block_id));
        }
        output.push_str(&format!(
            "<li><a href=\"{}\">{}</a>",
            escape_xml(&href),
            escape_xml(&node.label)
        ));
        if !node.children.is_empty() {
            output.push_str("<ol>");
            render_toc_nodes(&node.children, unit_hrefs, output)?;
            output.push_str("</ol>");
        }
        output.push_str("</li>");
    }
    Ok(())
}

fn render_epub_unit(
    book: &BookDocument,
    unit: &crate::document::ContentUnit,
    asset_hrefs: &HashMap<&str, String>,
) -> Result<String> {
    let language = book.language.as_deref().unwrap_or("und");
    let mut body = String::new();
    if !unit_body_starts_with_title(unit) {
        body.push_str(&format!("<h1>{}</h1>", escape_xml(&unit.title)));
    }
    render_blocks(&unit.document.blocks, asset_hrefs, &mut body)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" lang="{0}" xml:lang="{0}">
<head><meta charset="utf-8"/><title>{1}</title><link rel="stylesheet" type="text/css" href="../styles/book.css"/></head>
<body><article>{2}</article></body></html>
"#,
        escape_xml(language),
        escape_xml(&unit.title),
        body,
    ))
}

fn unit_body_starts_with_title(unit: &crate::document::ContentUnit) -> bool {
    let title = unit.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return false;
    }

    let Some(Block::Heading { content, .. }) = unit.document.blocks.first() else {
        return false;
    };
    content
        .iter()
        .map(Inline::plain_text)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        == title
}

fn render_blocks(
    blocks: &[Block],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for block in blocks {
        let anchor = anchor_for_id(block.id());
        match block {
            Block::Paragraph { content, .. } => {
                output.push_str(&format!("<p id=\"{anchor}\">"));
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</p>");
            }
            Block::Heading { level, content, .. } => {
                output.push_str(&format!("<h{level} id=\"{anchor}\">"));
                render_inlines(content, asset_hrefs, output)?;
                output.push_str(&format!("</h{level}>"));
            }
            Block::BlockQuote { blocks, .. } => {
                output.push_str(&format!("<blockquote id=\"{anchor}\">"));
                render_blocks(blocks, asset_hrefs, output)?;
                output.push_str("</blockquote>");
            }
            Block::BulletList { items, .. } => {
                output.push_str(&format!("<ul id=\"{anchor}\">"));
                render_list_items(items, asset_hrefs, output)?;
                output.push_str("</ul>");
            }
            Block::OrderedList { start, items, .. } => {
                output.push_str(&format!("<ol id=\"{anchor}\" start=\"{start}\">"));
                render_list_items(items, asset_hrefs, output)?;
                output.push_str("</ol>");
            }
            Block::CodeBlock { language, code, .. } => {
                let language = language
                    .as_deref()
                    .map(|language| format!(" class=\"language-{}\"", escape_xml(language)))
                    .unwrap_or_default();
                output.push_str(&format!(
                    "<pre id=\"{anchor}\"><code{language}>{}</code></pre>",
                    escape_xml(code)
                ));
            }
            Block::ThematicBreak { .. } => {
                output.push_str(&format!("<hr id=\"{anchor}\"/>"));
            }
            Block::Table { header, rows, .. } => {
                output.push_str(&format!("<table id=\"{anchor}\">"));
                if let Some(header) = header {
                    output.push_str("<thead>");
                    render_table_row(header, true, asset_hrefs, output)?;
                    output.push_str("</thead>");
                }
                output.push_str("<tbody>");
                for row in rows {
                    render_table_row(row, false, asset_hrefs, output)?;
                }
                output.push_str("</tbody></table>");
            }
            Block::Image {
                asset_id,
                alt,
                title,
                caption,
                ..
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!(
                    "<figure id=\"{anchor}\"><img src=\"{}\" alt=\"{}\"",
                    escape_xml(href),
                    escape_xml(alt)
                ));
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str("/>");
                render_caption(caption, asset_hrefs, output)?;
                output.push_str("</figure>");
            }
            Block::Audio {
                asset_id,
                title,
                caption,
                ..
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!("<figure id=\"{anchor}\"><audio controls=\"controls\" preload=\"metadata\" src=\"{}\"", escape_xml(href)));
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str(">当前阅读器不支持音频播放。</audio>");
                render_caption(caption, asset_hrefs, output)?;
                output.push_str("</figure>");
            }
            Block::Video {
                asset_id,
                poster_asset_id,
                title,
                caption,
                ..
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!("<figure id=\"{anchor}\"><video controls=\"controls\" preload=\"metadata\" src=\"{}\"", escape_xml(href)));
                if let Some(poster) = poster_asset_id {
                    output.push_str(&format!(
                        " poster=\"{}\"",
                        escape_xml(required_asset_href(asset_hrefs, poster)?)
                    ));
                }
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str(">当前阅读器不支持视频播放。</video>");
                render_caption(caption, asset_hrefs, output)?;
                output.push_str("</figure>");
            }
            Block::RawHtml { source, .. } => {
                output.push_str(&format!("<section id=\"{anchor}\">"));
                output.push_str(&sanitize_raw_html(source));
                output.push_str("</section>");
            }
        }
    }
    Ok(())
}

fn render_list_items(
    items: &[ListItem],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for item in items {
        output.push_str("<li>");
        if let Some(checked) = item.checked {
            output.push_str(if checked { "[x] " } else { "[ ] " });
        }
        render_blocks(&item.blocks, asset_hrefs, output)?;
        output.push_str("</li>");
    }
    Ok(())
}

fn render_table_row(
    row: &TableRow,
    header: bool,
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    let tag = if header { "th" } else { "td" };
    output.push_str("<tr>");
    for cell in &row.cells {
        output.push_str(&format!("<{tag}>"));
        render_inlines(&cell.content, asset_hrefs, output)?;
        output.push_str(&format!("</{tag}>"));
    }
    output.push_str("</tr>");
    Ok(())
}

fn render_caption(
    caption: &[Inline],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    if !caption.is_empty() {
        output.push_str("<figcaption>");
        render_inlines(caption, asset_hrefs, output)?;
        output.push_str("</figcaption>");
    }
    Ok(())
}

fn render_inlines(
    inlines: &[Inline],
    asset_hrefs: &HashMap<&str, String>,
    output: &mut String,
) -> Result<()> {
    for inline in inlines {
        match inline {
            Inline::Text { value } => output.push_str(&escape_xml(value)),
            Inline::Emphasis { content } => {
                output.push_str("<em>");
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</em>");
            }
            Inline::Strong { content } => {
                output.push_str("<strong>");
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</strong>");
            }
            Inline::Strikethrough { content } => {
                output.push_str("<del>");
                render_inlines(content, asset_hrefs, output)?;
                output.push_str("</del>");
            }
            Inline::Code { value } => {
                output.push_str(&format!("<code>{}</code>", escape_xml(value)));
            }
            Inline::Link {
                href,
                title,
                content,
            } => {
                if safe_link_href(href) {
                    output.push_str(&format!("<a href=\"{}\"", escape_xml(href)));
                    if let Some(title) = title {
                        output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                    }
                    output.push('>');
                    render_inlines(content, asset_hrefs, output)?;
                    output.push_str("</a>");
                } else {
                    render_inlines(content, asset_hrefs, output)?;
                }
            }
            Inline::HardBreak => output.push_str("<br/>"),
            Inline::SoftBreak => output.push('\n'),
            Inline::Image {
                asset_id,
                alt,
                title,
            } => {
                let href = required_asset_href(asset_hrefs, asset_id)?;
                output.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\"",
                    escape_xml(href),
                    escape_xml(alt)
                ));
                if let Some(title) = title {
                    output.push_str(&format!(" title=\"{}\"", escape_xml(title)));
                }
                output.push_str("/>");
            }
            Inline::RawHtml { source, .. } => output.push_str(&sanitize_raw_html(source)),
        }
    }
    Ok(())
}

fn sanitize_raw_html(source: &str) -> String {
    fn safe_relative_url(url: &str) -> Option<Cow<'_, str>> {
        safe_link_href(url).then_some(Cow::Borrowed(url))
    }

    let mut builder = ammonia::Builder::default();
    builder
        .url_schemes(HashSet::from(["data"]))
        .url_relative(ammonia::UrlRelative::Custom(Box::new(safe_relative_url)));
    xhtmlify_void_elements(&builder.clean(source).to_string())
}

fn xhtmlify_void_elements(html: &str) -> String {
    const VOID_ELEMENTS: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];

    let bytes = html.as_bytes();
    let mut output = String::with_capacity(html.len());
    let mut copied_until = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'<'
            || bytes.get(cursor + 1).is_none_or(|byte| {
                matches!(*byte, b'/' | b'!' | b'?') || !byte.is_ascii_alphabetic()
            })
        {
            cursor += 1;
            continue;
        }
        let name_start = cursor + 1;
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b':'))
        {
            name_end += 1;
        }
        let name = &html[name_start..name_end];
        if !VOID_ELEMENTS
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            cursor = name_end;
            continue;
        }

        let mut end = name_end;
        let mut quote = None;
        while end < bytes.len() {
            match (bytes[end], quote) {
                (b'\'' | b'"', None) => quote = Some(bytes[end]),
                (byte, Some(active)) if byte == active => quote = None,
                (b'>', None) => break,
                _ => {}
            }
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
        let before_end = html[name_end..end].trim_end();
        if !before_end.ends_with('/') {
            output.push_str(&html[copied_until..end]);
            output.push('/');
            copied_until = end;
        }
        cursor = end + 1;
    }
    output.push_str(&html[copied_until..]);
    output
}

fn safe_link_href(href: &str) -> bool {
    let trimmed = href.trim();
    !trimmed.is_empty()
        && !trimmed.starts_with('/')
        && !trimmed.starts_with("\\\\")
        && !trimmed.split('/').any(|segment| segment == "..")
        && (!trimmed.contains(':') || trimmed.starts_with('#'))
}

fn required_asset_href<'a>(
    asset_hrefs: &'a HashMap<&str, String>,
    asset_id: &str,
) -> Result<&'a str> {
    asset_hrefs
        .get(asset_id)
        .map(String::as_str)
        .with_context(|| format!("EPUB 正文引用了无法导出的资源 {asset_id}"))
}

fn anchor_for_id(id: &str) -> String {
    format!("block-{}", blake3::hash(id.as_bytes()).to_hex())
}

fn escape_xml(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ if is_xml_character(character) => output.push(character),
            _ => output.push('\u{fffd}'),
        }
    }
    output
}

fn is_xml_character(character: char) -> bool {
    matches!(character, '\u{9}' | '\u{a}' | '\u{d}')
        || matches!(character as u32, 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}

fn base_media_type(media_type: &str) -> &str {
    media_type.split(';').next().unwrap_or(media_type).trim()
}

fn extension_for_media_type(media_type: &str) -> &'static str {
    match base_media_type(media_type) {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "text/css" => "css",
        "font/woff" | "application/font-woff" => "woff",
        "font/woff2" => "woff2",
        "font/ttf" | "application/x-font-ttf" => "ttf",
        "font/otf" | "application/vnd.ms-opentype" => "otf",
        _ => "bin",
    }
}

enum PreparedPdfPage {
    Text(Vec<String>),
    Image(PdfImage),
}

struct PdfImage {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

fn export_pdf(document: &BookDocument, resolver: &dyn AssetResolver) -> Result<Vec<u8>> {
    let mut pages = Vec::new();
    let mut title_text = document.title.clone();
    if !document.authors.is_empty() {
        title_text.push_str("\n\n");
        title_text.push_str(&document.authors.join(", "));
    }
    if let Some(description) = document.description.as_deref() {
        title_text.push_str("\n\n");
        title_text.push_str(description);
    }
    append_text_pages(&mut pages, &title_text);

    let mut emitted_images = HashSet::new();
    if let Some(cover_id) = document.cover_asset_id.as_deref() {
        append_pdf_image(
            document,
            resolver,
            cover_id,
            &mut emitted_images,
            &mut pages,
        )?;
    }
    for unit in &document.units {
        let mut text = if unit_body_starts_with_title(unit) {
            String::new()
        } else {
            unit.title.clone()
        };
        let body = unit.plain_text();
        if !body.trim().is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&body);
        }
        append_text_pages(&mut pages, &text);

        let mut unit_images = Vec::new();
        collect_block_image_ids(&unit.document.blocks, &mut unit_images);
        for asset_id in unit_images {
            append_pdf_image(
                document,
                resolver,
                asset_id,
                &mut emitted_images,
                &mut pages,
            )?;
        }
    }
    build_pdf(document, &pages)
}

fn append_pdf_image(
    document: &BookDocument,
    resolver: &dyn AssetResolver,
    asset_id: &str,
    emitted: &mut HashSet<String>,
    pages: &mut Vec<PreparedPdfPage>,
) -> Result<()> {
    if !emitted.insert(asset_id.to_string()) {
        return Ok(());
    }
    let metadata = document
        .find_asset(asset_id)
        .with_context(|| format!("PDF 引用了不存在的图片资源 {asset_id}"))?;
    let bytes = resolve_verified(metadata, resolver)?;
    match decode_pdf_image(&bytes) {
        Ok(image) => pages.push(PreparedPdfPage::Image(image)),
        Err(error) => tracing::warn!(
            asset_id,
            media_type = %metadata.media_type,
            %error,
            "PDF 导出跳过了无法解码的图片，替代文本仍保留在正文中"
        ),
    }
    Ok(())
}

fn decode_pdf_image(bytes: &[u8]) -> Result<PdfImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("无法识别图片格式")?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PDF_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_PDF_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_PDF_IMAGE_ALLOCATION);
    reader.limits(limits);
    let rgba = reader.decode().context("无法解码图片")?.to_rgba8();
    let (width, height) = rgba.dimensions();
    ensure!(width > 0 && height > 0, "图片尺寸不能为空");
    let raw = rgba.into_raw();
    let mut rgb = Vec::with_capacity(raw.len() / 4 * 3);
    for pixel in raw.chunks_exact(4) {
        let alpha = u16::from(pixel[3]);
        for channel in &pixel[..3] {
            let composited = (u16::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255;
            rgb.push(composited as u8);
        }
    }
    Ok(PdfImage { width, height, rgb })
}

fn collect_block_image_ids<'a>(blocks: &'a [Block], output: &mut Vec<&'a str>) {
    for block in blocks {
        match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                collect_inline_image_ids(content, output)
            }
            Block::BlockQuote { blocks, .. } => collect_block_image_ids(blocks, output),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                for item in items {
                    collect_block_image_ids(&item.blocks, output);
                }
            }
            Block::Table { header, rows, .. } => {
                for row in header.iter().chain(rows.iter()) {
                    for cell in &row.cells {
                        collect_inline_image_ids(&cell.content, output);
                    }
                }
            }
            Block::Image { asset_id, .. } => output.push(asset_id),
            Block::Video {
                poster_asset_id: Some(asset_id),
                ..
            } => output.push(asset_id),
            Block::Video { .. }
            | Block::Audio { .. }
            | Block::CodeBlock { .. }
            | Block::ThematicBreak { .. }
            | Block::RawHtml { .. } => {}
        }
    }
}

fn collect_inline_image_ids<'a>(inlines: &'a [Inline], output: &mut Vec<&'a str>) {
    for inline in inlines {
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => collect_inline_image_ids(content, output),
            Inline::Image { asset_id, .. } => output.push(asset_id),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

fn append_text_pages(pages: &mut Vec<PreparedPdfPage>, text: &str) {
    let mut lines = Vec::new();
    for source_line in text.lines() {
        if source_line.trim().is_empty() {
            lines.push(String::new());
        } else {
            lines.extend(wrap_visual_line(source_line, PDF_LINE_WIDTH));
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    for page in lines.chunks(PDF_LINES_PER_PAGE) {
        pages.push(PreparedPdfPage::Text(page.to_vec()));
    }
}

fn wrap_visual_line(line: &str, max_width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut width = 0;
    for character in line.chars() {
        let character_width = if character.is_ascii() { 1 } else { 2 };
        if width > 0 && width + character_width > max_width {
            result.push(std::mem::take(&mut current));
            width = 0;
        }
        current.push(character);
        width += character_width;
    }
    if !current.is_empty() || result.is_empty() {
        result.push(current);
    }
    result
}

fn build_pdf(document: &BookDocument, pages: &[PreparedPdfPage]) -> Result<Vec<u8>> {
    ensure!(!pages.is_empty(), "PDF 至少需要一页");
    let mut pdf = PdfBuilder::default();
    let catalog_id = pdf.reserve();
    let pages_id = pdf.reserve();
    let font_id = pdf.reserve();
    let cid_font_id = pdf.reserve();
    let font_descriptor_id = pdf.reserve();
    let info_id = pdf.reserve();

    pdf.set(
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    )?;
    pdf.set(
        font_id,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H /DescendantFonts [{cid_font_id} 0 R] >>"
        )
        .into_bytes(),
    )?;
    pdf.set(
        cid_font_id,
        format!(
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> /FontDescriptor {font_descriptor_id} 0 R /DW 1000 >>"
        )
        .into_bytes(),
    )?;
    pdf.set(
        font_descriptor_id,
        b"<< /Type /FontDescriptor /FontName /STSong-Light /Flags 6 /FontBBox [-25 -254 1000 880] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 >>".to_vec(),
    )?;
    let author = document.authors.join(", ");
    pdf.set(
        info_id,
        format!(
            "<< /Title <{}> /Author <{}> /Producer <{}> >>",
            pdf_utf16_hex(&document.title),
            pdf_utf16_hex(&author),
            pdf_utf16_hex("墨页")
        )
        .into_bytes(),
    )?;

    let mut page_ids = Vec::with_capacity(pages.len());
    for page in pages {
        let page_id = pdf.reserve();
        page_ids.push(page_id);
        match page {
            PreparedPdfPage::Text(lines) => {
                let content_id = pdf.add(pdf_stream(&render_pdf_text(lines), ""));
                pdf.set(
                    page_id,
                    format!(
                        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PDF_WIDTH} {PDF_HEIGHT}] /Resources << /Font << /F1 {font_id} 0 R >> >> /Contents {content_id} 0 R >>"
                    )
                    .into_bytes(),
                )?;
            }
            PreparedPdfPage::Image(image) => {
                let image_id = pdf.add(pdf_stream(
                    &image.rgb,
                    &format!(
                        "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8",
                        image.width, image.height
                    ),
                ));
                let content = render_pdf_image_placement(image.width, image.height);
                let content_id = pdf.add(pdf_stream(content.as_bytes(), ""));
                pdf.set(
                    page_id,
                    format!(
                        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PDF_WIDTH} {PDF_HEIGHT}] /Resources << /XObject << /Im1 {image_id} 0 R >> >> /Contents {content_id} 0 R >>"
                    )
                    .into_bytes(),
                )?;
            }
        }
    }
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.set(
        pages_id,
        format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} >>",
            page_ids.len()
        )
        .into_bytes(),
    )?;
    let document_hash = document.document_hash();
    let document_id = &document_hash[..32];
    pdf.finish(catalog_id, info_id, document_id)
}

fn render_pdf_text(lines: &[String]) -> Vec<u8> {
    let mut content = format!(
        "BT\n/F1 11 Tf\n15 TL\n{PDF_MARGIN} {} Td\n",
        PDF_HEIGHT - PDF_MARGIN - 11.0
    );
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            content.push_str("T*\n");
        }
        if !line.is_empty() {
            content.push('<');
            content.push_str(&pdf_ucs2_hex(line));
            content.push_str("> Tj\n");
        }
    }
    content.push_str("ET\n");
    content.into_bytes()
}

fn render_pdf_image_placement(width: u32, height: u32) -> String {
    let available_width = PDF_WIDTH - 2.0 * PDF_MARGIN;
    let available_height = PDF_HEIGHT - 2.0 * PDF_MARGIN;
    let scale = (available_width / width as f32).min(available_height / height as f32);
    let draw_width = width as f32 * scale;
    let draw_height = height as f32 * scale;
    let x = (PDF_WIDTH - draw_width) / 2.0;
    let y = (PDF_HEIGHT - draw_height) / 2.0;
    format!("q\n{draw_width:.3} 0 0 {draw_height:.3} {x:.3} {y:.3} cm\n/Im1 Do\nQ\n")
}

fn pdf_ucs2_hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 4);
    for character in value.chars() {
        let scalar = character as u32;
        let value = if scalar <= 0xffff {
            scalar as u16
        } else {
            0xfffd
        };
        output.push_str(&format!("{value:04X}"));
    }
    output
}

fn pdf_utf16_hex(value: &str) -> String {
    let mut output = String::from("FEFF");
    for value in value.encode_utf16() {
        output.push_str(&format!("{value:04X}"));
    }
    output
}

fn pdf_stream(bytes: &[u8], dictionary: &str) -> Vec<u8> {
    let separator = if dictionary.is_empty() { "" } else { " " };
    let mut output = format!(
        "<<{separator}{dictionary} /Length {} >>\nstream\n",
        bytes.len()
    )
    .into_bytes();
    output.extend_from_slice(bytes);
    output.extend_from_slice(b"\nendstream");
    output
}

#[derive(Default)]
struct PdfBuilder {
    objects: Vec<Option<Vec<u8>>>,
}

impl PdfBuilder {
    fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }

    fn add(&mut self, object: Vec<u8>) -> usize {
        let id = self.reserve();
        self.objects[id - 1] = Some(object);
        id
    }

    fn set(&mut self, id: usize, object: Vec<u8>) -> Result<()> {
        let slot = self
            .objects
            .get_mut(id.checked_sub(1).context("PDF 对象编号不能为 0")?)
            .context("PDF 对象编号越界")?;
        ensure!(slot.is_none(), "PDF 对象 {id} 被重复写入");
        *slot = Some(object);
        Ok(())
    }

    fn finish(self, root_id: usize, info_id: usize, document_id: &str) -> Result<Vec<u8>> {
        let mut output = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let object_count = self.objects.len();
        let mut offsets = Vec::with_capacity(object_count);
        for (index, object) in self.objects.into_iter().enumerate() {
            let object = object.with_context(|| format!("PDF 对象 {} 未初始化", index + 1))?;
            offsets.push(output.len());
            output.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            output.extend_from_slice(&object);
            output.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = output.len();
        output.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        output.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            ensure!(offset <= 9_999_999_999, "PDF 文件过大，交叉引用偏移溢出");
            output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        output.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root_id} 0 R /Info {info_id} 0 R /ID [<{document_id}><{document_id}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
                object_count + 1
            )
            .as_bytes(),
        );
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use image::{DynamicImage, ImageFormat};

    use super::*;
    use crate::document::{
        AssetRef, BlockDocument, BookFormat, ContentUnit, ContentUnitKind, TableCell, TocNode,
        TocTarget,
    };

    #[derive(Default)]
    struct MemoryAssets(HashMap<String, Vec<u8>>);

    impl AssetResolver for MemoryAssets {
        fn resolve(&self, asset_id: &str) -> Result<Vec<u8>> {
            self.0
                .get(asset_id)
                .cloned()
                .with_context(|| format!("missing test asset {asset_id}"))
        }
    }

    fn sample() -> (BookDocument, MemoryAssets, Vec<u8>) {
        let original = b"byte-exact original document\0\xff".to_vec();
        let original_ref = AssetRef::from_bytes(
            AssetRole::OriginalSource,
            "application/octet-stream",
            Some("original.docx".to_string()),
            &original,
        );
        let mut image_bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(2, 1)
            .write_to(&mut image_bytes, ImageFormat::Png)
            .expect("encode PNG fixture");
        let image_bytes = image_bytes.into_inner();
        let mut image_ref = AssetRef::from_bytes(
            AssetRole::Cover,
            "image/png",
            Some("cover.png".to_string()),
            &image_bytes,
        );
        image_ref.add_role(AssetRole::ContentImage);

        let paragraph = Block::Paragraph {
            id: "paragraph-1".to_string(),
            content: vec![
                Inline::text("中文测试 & normalized export"),
                Inline::Image {
                    asset_id: image_ref.id.clone(),
                    alt: "插图".to_string(),
                    title: None,
                },
            ],
        };
        let raw = Block::RawHtml {
            id: "raw-1".to_string(),
            source: "<script>alert(1)</script><em>保留文字</em>".to_string(),
            plain_text: "保留文字".to_string(),
        };
        let unit = ContentUnit::new(
            "unit-1",
            ContentUnitKind::Chapter,
            "第一章",
            "<h1>第一章</h1>",
            BlockDocument::new(vec![paragraph, raw]),
        );
        let mut document = BookDocument::new(
            "book-1",
            "导出测试",
            BookSource::imported(
                BookFormat::Docx,
                original_ref.id.clone(),
                Some("original.docx".to_string()),
            ),
        );
        document.authors = vec!["墨页".to_string()];
        document.language = Some("zh-CN".to_string());
        document.cover_asset_id = Some(image_ref.id.clone());
        document.units = vec![unit];
        document.toc = vec![TocNode::new("toc-1", "第一章", TocTarget::unit("unit-1"))];
        document.assets = vec![original_ref.clone(), image_ref.clone()];

        let assets = MemoryAssets(HashMap::from([
            (original_ref.id, original.clone()),
            (image_ref.id, image_bytes),
        ]));
        (document, assets, original)
    }

    #[test]
    fn original_export_is_byte_exact_and_atomically_replaces_target() {
        let (document, assets, original) = sample();
        let directory = tempfile::tempdir().expect("temporary export directory");
        let target = directory.path().join("original.docx");
        fs::write(&target, b"older complete file").expect("seed target");

        BuiltinDocumentExporter
            .export(&document, ExportFormat::Original, &assets, &target)
            .expect("export original");

        assert_eq!(fs::read(target).expect("read export"), original);
    }

    #[test]
    fn failed_asset_integrity_check_leaves_existing_target_unchanged() {
        let (document, mut assets, _) = sample();
        let original_id = match &document.source {
            BookSource::Imported {
                original_asset_id, ..
            } => original_asset_id,
            BookSource::Created => unreachable!(),
        };
        assets.0.insert(original_id.clone(), b"tampered".to_vec());
        let directory = tempfile::tempdir().expect("temporary export directory");
        let target = directory.path().join("original.docx");
        fs::write(&target, b"keep me").expect("seed target");

        let error = BuiltinDocumentExporter
            .export(&document, ExportFormat::Original, &assets, &target)
            .expect_err("integrity mismatch must fail");

        assert!(error.to_string().contains("长度") || error.to_string().contains("BLAKE3"));
        assert_eq!(fs::read(target).expect("read old target"), b"keep me");
    }

    #[test]
    fn normalized_epub_is_deterministic_valid_and_script_free() {
        let (document, assets, _) = sample();
        let first = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Epub, &assets)
            .expect("export EPUB");
        let second = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Epub, &assets)
            .expect("repeat export EPUB");
        assert_eq!(first, second);

        let mut archive = zip::ZipArchive::new(Cursor::new(&first)).expect("valid ZIP");
        let first_entry = archive.by_index(0).expect("first ZIP entry");
        assert_eq!(first_entry.name(), "mimetype");
        assert_eq!(first_entry.compression(), CompressionMethod::Stored);
        drop(first_entry);
        let mut chapter = String::new();
        std::io::Read::read_to_string(
            &mut archive
                .by_name("EPUB/text/unit-0001.xhtml")
                .expect("chapter entry"),
            &mut chapter,
        )
        .expect("read chapter");
        assert!(chapter.contains("中文测试 &amp; normalized export"));
        assert!(chapter.contains("../assets/asset-0001.png"));
        assert!(!chapter.to_ascii_lowercase().contains("<script"));
        assert!(rbook::Epub::read(Cursor::new(first)).is_ok());
    }

    #[test]
    fn normalized_epub_does_not_duplicate_an_existing_unit_title() {
        let (mut document, assets, _) = sample();
        assert!(!unit_body_starts_with_title(&document.units[0]));
        let title = document.units[0].title.clone();
        document.units[0]
            .document
            .blocks
            .insert(0, Block::heading("chapter-title", 1, title));
        assert!(unit_body_starts_with_title(&document.units[0]));

        let bytes = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Epub, &assets)
            .expect("export EPUB");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("valid ZIP");
        let mut chapter = String::new();
        std::io::Read::read_to_string(
            &mut archive
                .by_name("EPUB/text/unit-0001.xhtml")
                .expect("chapter entry"),
            &mut chapter,
        )
        .expect("read chapter");

        assert_eq!(chapter.matches("<h1").count(), 1, "{chapter}");
    }

    #[test]
    fn only_a_heading_block_can_supply_the_unit_title() {
        let (mut document, _, _) = sample();
        let title = document.units[0].title.clone();
        let non_heading_blocks = vec![
            Block::paragraph("paragraph-title", &title),
            Block::BlockQuote {
                id: "quote-title".to_string(),
                blocks: vec![Block::heading("nested-heading", 1, &title)],
            },
            Block::BulletList {
                id: "bullet-title".to_string(),
                items: vec![ListItem::new(vec![Block::paragraph("bullet-text", &title)])],
            },
            Block::OrderedList {
                id: "ordered-title".to_string(),
                start: 1,
                items: vec![ListItem::new(vec![Block::paragraph(
                    "ordered-text",
                    &title,
                )])],
            },
            Block::CodeBlock {
                id: "code-title".to_string(),
                language: None,
                code: title.clone(),
            },
            Block::Table {
                id: "table-title".to_string(),
                header: Some(TableRow::new(vec![TableCell::text(&title)])),
                rows: Vec::new(),
            },
            Block::Image {
                id: "image-title".to_string(),
                asset_id: "image-asset".to_string(),
                alt: title.clone(),
                title: None,
                caption: Vec::new(),
            },
            Block::Audio {
                id: "audio-title".to_string(),
                asset_id: "audio-asset".to_string(),
                title: Some(title.clone()),
                caption: Vec::new(),
            },
            Block::Video {
                id: "video-title".to_string(),
                asset_id: "video-asset".to_string(),
                poster_asset_id: None,
                title: Some(title.clone()),
                caption: Vec::new(),
            },
            Block::RawHtml {
                id: "raw-title".to_string(),
                source: format!("<h1>{title}</h1>"),
                plain_text: title.clone(),
            },
        ];

        for block in non_heading_blocks {
            document.units[0].document.blocks = vec![block.clone()];
            assert!(
                !unit_body_starts_with_title(&document.units[0]),
                "non-heading block must not suppress the generated title: {block:?}"
            );
        }
    }

    #[test]
    fn normalized_epub_keeps_title_when_code_only_has_the_same_text() {
        let (mut document, assets, _) = sample();
        let title = document.units[0].title.clone();
        document.units[0].document.blocks.insert(
            0,
            Block::CodeBlock {
                id: "code-title".to_string(),
                language: None,
                code: title.clone(),
            },
        );
        assert!(!unit_body_starts_with_title(&document.units[0]));

        let bytes = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Epub, &assets)
            .expect("export EPUB");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("valid ZIP");
        let mut chapter = String::new();
        std::io::Read::read_to_string(
            &mut archive
                .by_name("EPUB/text/unit-0001.xhtml")
                .expect("chapter entry"),
            &mut chapter,
        )
        .expect("read chapter");

        assert_eq!(chapter.matches("<h1").count(), 1, "{chapter}");
        assert!(
            chapter.contains(&format!("<code>{title}</code>")),
            "{chapter}"
        );
    }

    #[test]
    fn pdf_deduplicates_heading_title_but_not_code_with_the_same_text() {
        let (mut document, assets, _) = sample();
        let title = document.units[0].title.clone();
        let encoded_title = pdf_ucs2_hex(&title);

        document.units[0]
            .document
            .blocks
            .insert(0, Block::heading("chapter-title", 1, &title));
        let heading_pdf = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Pdf, &assets)
            .expect("export PDF with heading title");
        assert_eq!(
            String::from_utf8_lossy(&heading_pdf)
                .matches(&encoded_title)
                .count(),
            1,
            "semantic heading should suppress the generated duplicate"
        );

        document.units[0].document.blocks[0] = Block::CodeBlock {
            id: "code-title".to_string(),
            language: None,
            code: title,
        };
        let code_pdf = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Pdf, &assets)
            .expect("export PDF with code matching title");
        assert_eq!(
            String::from_utf8_lossy(&code_pdf)
                .matches(&encoded_title)
                .count(),
            2,
            "code text must not suppress the generated unit title"
        );
    }

    #[test]
    fn pdf_is_deterministic_parseable_and_contains_text_and_image_pages() {
        let (document, assets, _) = sample();
        let first = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Pdf, &assets)
            .expect("export PDF");
        let second = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Pdf, &assets)
            .expect("repeat export PDF");
        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.7"));
        let parsed = lopdf::Document::load_mem(&first).expect("parse generated PDF");
        assert!(parsed.get_pages().len() >= 3);
        let ascii = String::from_utf8_lossy(&first);
        assert!(ascii.contains("4E2D65876D4B8BD5"));
        assert!(ascii.contains("/Subtype /Image"));
    }

    #[test]
    fn created_book_has_no_original_export() {
        let mut document = BookDocument::created("book-created", "新书");
        document.units.push(ContentUnit::empty(
            "unit-1",
            ContentUnitKind::Chapter,
            "第一章",
        ));
        let error = BuiltinDocumentExporter
            .export_bytes(&document, ExportFormat::Original, &MemoryAssets::default())
            .expect_err("created book has no original");
        assert!(error.to_string().contains("没有可导出的原文件"));
    }

    #[test]
    fn atomic_write_never_requires_the_target_to_be_absent() {
        let directory = tempfile::tempdir().expect("temporary export directory");
        let target = directory.path().join("book.pdf");
        atomic_write(&target, b"first").expect("first atomic write");
        atomic_write(&target, b"second").expect("replace atomically");
        assert_eq!(fs::read(target).expect("read target"), b"second");
    }

    #[test]
    fn pre_commit_sync_failure_leaves_existing_target_unchanged() {
        let directory = tempfile::tempdir().expect("temporary export directory");
        let target = directory.path().join("book.pdf");
        fs::write(&target, b"old complete file").expect("seed target");

        let error = atomic_write_with_syncs(
            &target,
            b"new complete file",
            |_| Err(std::io::Error::other("injected staged-file sync failure")),
            |_| Ok(()),
        )
        .expect_err("pre-commit sync failure must abort the replacement");

        assert!(error.to_string().contains("临时文件"));
        assert_eq!(
            fs::read(&target).expect("read unchanged target"),
            b"old complete file"
        );
    }

    #[test]
    fn post_commit_directory_sync_failure_does_not_report_export_failure() {
        let directory = tempfile::tempdir().expect("temporary export directory");
        let target = directory.path().join("book.pdf");
        fs::write(&target, b"old complete file").expect("seed target");

        atomic_write_with_syncs(
            &target,
            b"new complete file",
            std::fs::File::sync_all,
            |_| Err(std::io::Error::other("injected directory sync failure")),
        )
        .expect("the replacement is already committed");

        assert_eq!(
            fs::read(&target).expect("read committed target"),
            b"new complete file"
        );
    }

    #[test]
    fn visual_line_wrapping_is_utf8_safe() {
        let wrapped = wrap_visual_line(&"中".repeat(50), PDF_LINE_WIDTH);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped.concat(), "中".repeat(50));
    }

    #[test]
    fn raw_html_is_inert_and_serialized_as_xhtml() {
        let clean = sanitize_raw_html(
            r#"<script src="https://example.invalid/a.js">bad()</script><img src="//example.invalid/a.png"><br><a href="chapter.xhtml">next</a>"#,
        );
        assert!(!clean.contains("script"));
        assert!(!clean.contains("example.invalid"));
        assert!(clean.contains("<img />") || clean.contains("<img/>"));
        assert!(clean.contains("<br />") || clean.contains("<br/>"));
        assert!(clean.contains("href=\"chapter.xhtml\""));
    }

    #[test]
    fn xml_escape_replaces_disallowed_control_characters() {
        assert_eq!(escape_xml("a\u{1}&b"), "a\u{fffd}&amp;b");
    }

    #[test]
    fn resolver_closure_is_supported() {
        let bytes = Arc::new(b"asset".to_vec());
        let captured = Arc::clone(&bytes);
        let resolver = move |_: &str| Ok(captured.as_ref().clone());
        assert_eq!(resolver.resolve("ignored").expect("resolve"), *bytes);
    }
}
