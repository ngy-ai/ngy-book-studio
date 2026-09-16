use std::{collections::HashMap, io::Cursor, ops::Range, sync::Arc};

use anyhow::{Context as _, Result, bail, ensure};
use percent_encoding::percent_decode_str;
use rbook::{Epub, epub::rewrite::EpubRewriteOptions};

use crate::{
    document::{AssetRole, BookDocument},
    media::{MediaBackend, MediaMetadata, MediaResponse, MediaService},
};

/// Reading text size in pixels: what a chapter renders at before the reader
/// changes it, the range Ctrl + wheel covers, and one wheel notch's step. The
/// chapter runtime clamps the same range in `READER_INITIALIZATION_SCRIPT`, and
/// `the_chapter_runtime_shares_the_host_text_size_range` fails if they drift.
pub const READER_FONT_SIZE_DEFAULT: u8 = 18;
pub const READER_FONT_SIZE_MIN: u8 = 14;
pub const READER_FONT_SIZE_MAX: u8 = 30;
pub const READER_FONT_SIZE_STEP: u8 = 2;

const READER_CSS: &str = r#"
  /* ngy-reader-style */
  :root {
    color-scheme: light;
    --ngy-font-size: 18px;
    --ngy-paper: #fbfaf7;
    --ngy-text: #302d29;
    --ngy-muted: #716b63;
    --ngy-accent: #c35f3f;
  }
  html { background: var(--ngy-paper) !important; }
  body {
    box-sizing: border-box !important;
    max-width: 820px !important;
    min-height: 100vh !important;
    margin: 0 auto !important;
    padding: 48px 72px 96px !important;
    background: var(--ngy-paper) !important;
    color: var(--ngy-text) !important;
    font-family: "Noto Serif CJK SC", "Source Han Serif SC", "Microsoft YaHei", Georgia, serif !important;
    font-size: var(--ngy-font-size) !important;
    font-weight: normal !important;
    line-height: 1.85 !important;
    overflow-wrap: anywhere;
  }
  p { margin: 0 0 1.15em !important; }
  h1, h2, h3, h4, h5, h6 {
    color: var(--ngy-text) !important;
    line-height: 1.35 !important;
    margin-top: 1.7em !important;
  }
  h1:first-child, h2:first-child, h3:first-child { margin-top: 0 !important; }
  img, svg, video { max-width: 100% !important; height: auto !important; }
  table { max-width: 100% !important; border-collapse: collapse; }
  a { color: var(--ngy-accent) !important; text-decoration-thickness: 1px; }
  blockquote {
    margin-left: 0 !important;
    padding-left: 1.2em !important;
    border-left: 3px solid #dfd4ca !important;
    color: var(--ngy-muted) !important;
  }
  @media (max-width: 720px) {
    body { padding: 32px 34px 72px !important; }
  }
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpineItem {
    pub href: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocItem {
    pub label: String,
    pub href: Option<String>,
    pub depth: usize,
    pub spine_index: Option<usize>,
}

#[derive(Clone)]
pub struct OpenedBook {
    pub epub: Arc<Epub>,
    pub title: String,
    pub spine: Vec<SpineItem>,
    pub toc: Vec<TocItem>,
}

impl OpenedBook {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        crate::epub_limits::validate_epub_archive(path)?;
        let epub = Arc::new(
            Epub::open(path).with_context(|| format!("无法打开 EPUB：{}", path.display()))?,
        );
        Self::from_epub(epub)
    }

    /// Opens a book from the EPUB bytes stored in the database.
    pub fn open_bytes(bytes: Vec<u8>) -> Result<Self> {
        crate::epub_limits::validate_epub_bytes(&bytes)?;
        let epub = Arc::new(Epub::read(Cursor::new(bytes)).context("无法解析 EPUB 数据")?);
        Self::from_epub(epub)
    }

    fn from_epub(epub: Arc<Epub>) -> Result<Self> {
        let title = epub
            .metadata()
            .title()
            .map(|title| title.value().trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "未命名图书".to_string());

        let spine = epub
            .spine()
            .iter()
            .filter(|entry| entry.is_linear())
            .filter_map(|entry| entry.manifest_entry())
            .map(|entry| {
                let href = entry.href().as_str().to_string();
                SpineItem {
                    title: title_from_href(&href),
                    href,
                }
            })
            .collect::<Vec<_>>();

        if spine.is_empty() {
            bail!("EPUB 不包含可阅读章节");
        }

        let mut toc = Vec::new();
        if let Some(root) = epub.toc().contents() {
            for entry in root.flatten() {
                let href = entry.href().map(|href| href.as_str().to_string());
                let spine_index = href.as_deref().and_then(|href| {
                    let path = href.split(['?', '#']).next().unwrap_or(href);
                    spine.iter().position(|spine_item| {
                        spine_item.href.split(['?', '#']).next() == Some(path)
                    })
                });
                toc.push(TocItem {
                    label: non_empty_label(entry.label(), href.as_deref()),
                    href,
                    depth: entry.depth().saturating_sub(1),
                    spine_index,
                });
            }
        }

        if toc.is_empty() {
            toc = spine
                .iter()
                .enumerate()
                .map(|(index, item)| TocItem {
                    label: item.title.clone(),
                    href: Some(item.href.clone()),
                    depth: 0,
                    spine_index: Some(index),
                })
                .collect();
        }

        Ok(Self {
            epub,
            title,
            spine,
            toc,
        })
    }

    pub fn url_for_href(href: &str) -> String {
        let href = if href.starts_with('/') {
            href.to_string()
        } else {
            format!("/{href}")
        };
        format!("epubreader://book{href}")
    }

    /// Return the URL that should be passed directly to the platform WebView.
    ///
    /// Wry maps custom protocols to an HTTP origin on Windows when the WebView
    /// is first built, but `WebView::load_url` does not apply that mapping to
    /// later navigations. Use the same mapped origin here so chapter changes
    /// continue to reach the custom protocol handler.
    pub fn navigation_url_for_href(href: &str) -> String {
        let url = Self::url_for_href(href);
        #[cfg(target_os = "windows")]
        {
            url.replacen("epubreader://", "http://epubreader.", 1)
        }
        #[cfg(not(target_os = "windows"))]
        {
            url
        }
    }

    pub fn spine_index_for_url(&self, url: &str) -> Option<usize> {
        let loaded_path = reader_url_path(url)?;
        self.spine.iter().position(|item| {
            let href_path = item.href.split(['?', '#']).next().unwrap_or(&item.href);
            if href_path.starts_with('/') {
                href_path == loaded_path
            } else {
                loaded_path
                    .strip_prefix('/')
                    .is_some_and(|path| path == href_path)
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceResponse {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub mime: String,
    pub accept_ranges: Option<&'static str>,
    pub content_length: u64,
    pub content_range: Option<String>,
}

pub fn load_resource(epub: &Epub, request_path: &str) -> Result<ResourceResponse> {
    load_resource_with_range(epub, request_path, None)
}

/// Loads one manifest-declared EPUB resource and applies HTTP single-range
/// semantics to binary resources. This fallback is scoped to one already-open
/// EPUB; the application protocol prefers persisted book assets so large media
/// ranges can be read directly from the object store.
pub fn load_resource_with_range(
    epub: &Epub,
    request_path: &str,
    range_header: Option<&str>,
) -> Result<ResourceResponse> {
    let safe_path = safe_resource_path(request_path)?;
    let entry = epub
        .manifest()
        .by_href(&safe_path)
        .with_context(|| format!("EPUB 清单中没有资源：{safe_path}"))?;
    let mime = resource_mime(entry.media_type(), &safe_path);

    let bytes = if is_html_mime(&mime) {
        let rewrite = EpubRewriteOptions::new().inject_css(READER_CSS);
        let mut html = entry
            .read_str_with(&rewrite)
            .with_context(|| format!("无法读取章节资源：{safe_path}"))?;
        // rbook's ContentRewriter injects CSS on </head> but not on
        // <head/> (self-closing). Calibre-generated EPUBs often use
        // <head/>, which causes the CSS injection to be skipped and
        // leaves the WebView rendering body text with no styling.
        ensure_reader_css(&mut html);
        // The chapter is handed to the WebView as `application/xhtml+xml`, so
        // every character reference must be one XML resolves on its own. Third
        // party EPUBs and our own generated projection both reach this point
        // with `&nbsp;` and friends otherwise.
        let html = crate::markup::xml_safe_entities(&html);
        tracing::debug!(
            target: "ngy_reader",
            path = %safe_path,
            mime = %mime,
            html_bytes = html.html.len(),
            rewritten = html.rewritten_count,
            entities = ?html.rewritten,
            "阅读器：准备章节正文"
        );
        html.html.into_owned().into_bytes()
    } else {
        entry
            .read_bytes()
            .with_context(|| format!("无法读取 EPUB 资源：{safe_path}"))?
    };
    let mime = if is_html_mime(&mime) {
        format!("{mime}; charset=utf-8")
    } else {
        mime
    };

    if is_html_mime(&mime) {
        return Ok(ResourceResponse {
            status: 200,
            content_length: bytes.len() as u64,
            bytes,
            mime,
            accept_ranges: None,
            content_range: None,
        });
    }

    let response = MediaService::new(ManifestResourceBackend {
        path: safe_path.clone(),
        media_type: mime,
        bytes: Arc::new(bytes),
    })
    .serve("epub", request_path, range_header)?;
    tracing::debug!(
        target: "ngy_reader",
        path = %safe_path,
        mime = %response.media_type,
        status = response.status,
        bytes = response.content_length,
        range = ?response.content_range,
        "阅读器：准备资源响应"
    );
    Ok(response.into())
}

/// rbook's `ContentRewriter` injects CSS on `</head>` but not on `<head/>`
/// (self-closing). EPUBs generated by calibre and similar tools often use
/// `<head/>`, which skips the CSS injection entirely. This function ensures
/// the reader stylesheet is always present in the HTML served to the WebView.
fn ensure_reader_css(html: &mut String) {
    if html.contains("ngy-reader-style") {
        return; // Already injected by rbook
    }
    let style_tag = format!("<style>/*<![CDATA[*/{}/*]]>*/</style>", READER_CSS);
    // Replace <head/> or <head /> with <head><style>...</style></head>
    if let Some(pos) = html.find("<head/>") {
        let end = pos + "<head/>".len();
        html.replace_range(pos..end, &format!("<head>{style_tag}</head>"));
        return;
    }
    if let Some(pos) = html.find("<head />") {
        let end = pos + "<head />".len();
        html.replace_range(pos..end, &format!("<head>{style_tag}</head>"));
        return;
    }
    // Fallback: inject before </head> or after <body>
    if let Some(pos) = html.find("</head>") {
        html.insert_str(pos, &style_tag);
        return;
    }
    if let Some(pos) = html.find("<body") {
        let end = html[pos..]
            .find('>')
            .map(|i| pos + i + 1)
            .unwrap_or(pos + 5);
        html.insert_str(end, &style_tag);
    }
}

impl From<MediaResponse> for ResourceResponse {
    fn from(response: MediaResponse) -> Self {
        Self {
            status: response.status,
            bytes: response.body,
            mime: response.media_type,
            accept_ranges: Some(response.accept_ranges),
            content_length: response.content_length,
            content_range: response.content_range,
        }
    }
}

#[derive(Clone)]
struct ManifestResourceBackend {
    path: String,
    media_type: String,
    bytes: Arc<Vec<u8>>,
}

impl MediaBackend for ManifestResourceBackend {
    fn metadata(&self, book_id: &str, asset_id: &str) -> Result<MediaMetadata> {
        self.authorize(book_id, asset_id)?;
        Ok(MediaMetadata {
            media_type: self.media_type.clone(),
            byte_len: self.bytes.len() as u64,
        })
    }

    fn read(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
        self.authorize(book_id, asset_id)?;
        Ok(self.bytes.as_ref().clone())
    }

    fn read_range(&self, book_id: &str, asset_id: &str, range: Range<u64>) -> Result<Vec<u8>> {
        self.authorize(book_id, asset_id)?;
        let start = usize::try_from(range.start).context("EPUB range start is too large")?;
        let end = usize::try_from(range.end).context("EPUB range end is too large")?;
        Ok(self
            .bytes
            .get(start..end)
            .context("EPUB range is outside the manifest resource")?
            .to_vec())
    }
}

impl ManifestResourceBackend {
    fn authorize(&self, book_id: &str, asset_id: &str) -> Result<()> {
        ensure!(book_id == "epub", "resource belongs to another EPUB");
        ensure!(
            safe_resource_path(asset_id)? == self.path,
            "resource is not the authorized manifest entry"
        );
        Ok(())
    }
}

/// Immutable authorization produced from one persisted document revision and
/// its EPUB reader projection. A path is present only when its manifest bytes
/// exactly match an asset owned by that document.
#[derive(Clone, Debug, Default)]
pub struct ReaderResourceAuthorizations {
    book_id: String,
    assets_by_path: Arc<HashMap<String, AuthorizedReaderAsset>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedReaderAsset {
    pub asset_id: String,
    pub media_type: String,
    pub byte_len: u64,
    pub content_hash: String,
}

impl ReaderResourceAuthorizations {
    pub fn for_document(epub: &Epub, document: &BookDocument) -> Result<Self> {
        document.validate()?;
        let mut candidates = HashMap::<(&str, u64), Vec<&crate::document::AssetRef>>::new();
        for asset in document.assets.iter().filter(|asset| {
            !asset.roles.contains(&AssetRole::OriginalSource)
                && normalize_mime(&asset.media_type).is_some()
        }) {
            candidates
                .entry((asset.content_hash.as_str(), asset.byte_len))
                .or_default()
                .push(asset);
        }

        let mut assets_by_path = HashMap::new();
        for entry in epub.manifest().iter() {
            let href = entry.href().as_str();
            let request_path = if href.starts_with('/') {
                href.to_string()
            } else {
                format!("/{href}")
            };
            let Ok(path_key) = canonical_resource_path(&request_path) else {
                continue;
            };
            let safe_path = safe_resource_path(&request_path)?;
            let media_type = resource_mime(entry.media_type(), &safe_path);
            if is_html_mime(&media_type) {
                continue;
            }
            let bytes = match entry.read_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(href, %error, "cannot authorize unreadable EPUB resource");
                    continue;
                }
            };
            let content_hash = blake3::hash(&bytes).to_hex().to_string();
            let Some(matches) = candidates.get(&(content_hash.as_str(), bytes.len() as u64)) else {
                continue;
            };
            let Some(asset) = matches
                .iter()
                .copied()
                .find(|asset| normalize_mime(&asset.media_type).as_deref() == Some(&media_type))
            else {
                continue;
            };
            assets_by_path.insert(
                path_key,
                AuthorizedReaderAsset {
                    asset_id: asset.id.clone(),
                    media_type,
                    byte_len: asset.byte_len,
                    content_hash,
                },
            );
        }

        Ok(Self {
            book_id: document.id.clone(),
            assets_by_path: Arc::new(assets_by_path),
        })
    }

    pub fn book_id(&self) -> &str {
        &self.book_id
    }

    pub fn asset_for_path(&self, request_path: &str) -> Result<Option<AuthorizedReaderAsset>> {
        let path = canonical_resource_path(request_path)?;
        Ok(self.assets_by_path.get(&path).cloned())
    }
}

fn resource_mime(manifest_mime: &str, safe_path: &str) -> String {
    normalize_mime(manifest_mime)
        .or_else(|| {
            mime_guess::from_path(safe_path)
                .first_raw()
                .and_then(normalize_mime)
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn reader_url_path(url: &str) -> Option<&str> {
    const INTERNAL_ORIGINS: [&str; 3] = [
        "epubreader://book",
        "http://epubreader.book",
        "https://epubreader.book",
    ];
    INTERNAL_ORIGINS
        .iter()
        .find_map(|origin| url.strip_prefix(origin))
        .filter(|path| path.starts_with('/'))
        .map(|path| path.split(['?', '#']).next().unwrap_or(path))
}

/// Applies one reading text size to a chapter document.
///
/// The size is one custom property on the root element, so a chapter never has
/// to be re-served when the reader changes it: the served stylesheet carries the
/// default and this override wins over it. The value is clamped here because the
/// host is the last place that can bound a size that came back from the page or
/// from an old settings row.
pub fn reader_appearance_script(font_size: u8) -> String {
    let font_size = reader_font_size(font_size);
    format!(
        "(() => {{ document.documentElement.style \
         .setProperty('--ngy-font-size', '{font_size}px'); }})()"
    )
}

/// The one range a reading text size is allowed to take.
pub fn reader_font_size(font_size: u8) -> u8 {
    font_size.clamp(READER_FONT_SIZE_MIN, READER_FONT_SIZE_MAX)
}

fn safe_resource_path(request_path: &str) -> Result<String> {
    let encoded_path = request_path
        .split(['?', '#'])
        .next()
        .unwrap_or(request_path);
    let decoded = percent_decode_str(encoded_path)
        .decode_utf8()
        .context("EPUB 资源路径不是有效的 UTF-8")?;
    if decoded.contains('\0') || decoded.contains('\\') || decoded.contains(':') {
        bail!("EPUB 资源路径不安全");
    }
    if decoded
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        bail!("EPUB 资源路径越过了图书边界");
    }
    if !encoded_path.starts_with('/') || encoded_path == "/" {
        bail!("EPUB 资源路径无效");
    }
    Ok(encoded_path.to_string())
}

fn canonical_resource_path(request_path: &str) -> Result<String> {
    let safe_path = safe_resource_path(request_path)?;
    percent_decode_str(&safe_path)
        .decode_utf8()
        .context("EPUB 资源路径不是有效的 UTF-8")
        .map(|path| path.into_owned())
}

fn is_html_mime(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    matches!(mime, "application/xhtml+xml" | "text/html")
}

fn normalize_mime(mime: &str) -> Option<String> {
    let mime = mime.split(';').next()?.trim().to_ascii_lowercase();
    let (kind, subtype) = mime.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    };
    (valid(kind) && valid(subtype)).then_some(mime)
}

fn title_from_href(href: &str) -> String {
    let path = href.split(['?', '#']).next().unwrap_or(href);
    let decoded = percent_decode_str(path).decode_utf8_lossy();
    decoded
        .rsplit('/')
        .next()
        .unwrap_or("章节")
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or("章节")
        .replace(['-', '_'], " ")
}

fn non_empty_label(label: &str, href: Option<&str>) -> String {
    let label = label.trim();
    if label.is_empty() {
        href.map(title_from_href)
            .unwrap_or_else(|| "未命名分组".to_string())
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_encoded_parent_traversal() {
        assert!(safe_resource_path("/EPUB/%2e%2e/META-INF/container.xml").is_err());
        assert!(safe_resource_path("/EPUB/chapter.xhtml").is_ok());
    }

    #[test]
    fn builds_custom_protocol_url() {
        assert_eq!(
            "epubreader://book/EPUB/chapter.xhtml#part-1",
            OpenedBook::url_for_href("/EPUB/chapter.xhtml#part-1")
        );
    }

    #[test]
    fn builds_platform_navigation_url() {
        let url = OpenedBook::navigation_url_for_href("/EPUB/chapter.xhtml#part-1");
        #[cfg(target_os = "windows")]
        assert_eq!("http://epubreader.book/EPUB/chapter.xhtml#part-1", url);
        #[cfg(not(target_os = "windows"))]
        assert_eq!("epubreader://book/EPUB/chapter.xhtml#part-1", url);
    }

    #[test]
    fn rejects_unsafe_mime_header_values() {
        assert_eq!(
            normalize_mime("Application/XHTML+XML; charset=UTF-8").as_deref(),
            Some("application/xhtml+xml")
        );
        assert_eq!(normalize_mime("text/html\r\nx-injected: true"), None);
    }

    #[test]
    fn recognizes_only_internal_reader_urls() {
        assert_eq!(
            reader_url_path("http://epubreader.book/EPUB/chapter.xhtml#part"),
            Some("/EPUB/chapter.xhtml")
        );
        assert_eq!(
            reader_url_path("epubreader://book/EPUB/chapter.xhtml"),
            Some("/EPUB/chapter.xhtml")
        );
        assert_eq!(reader_url_path("https://epubreader.evil/chapter"), None);
    }

    /// The served stylesheet carries the text size a chapter renders at before
    /// the reader changes it. The runtime clamps the same range in
    /// `ui::reader::READER_INITIALIZATION_SCRIPT`, which pins it too.
    #[test]
    fn the_served_chapter_stylesheet_defaults_to_the_host_text_size() {
        assert!(
            READER_CSS.contains(&format!("--ngy-font-size: {READER_FONT_SIZE_DEFAULT}px")),
            "the served chapter stylesheet must default to the host's text size"
        );
    }

    #[test]
    fn reading_text_size_steps_within_the_host_range() {
        assert_eq!(reader_font_size(0), READER_FONT_SIZE_MIN);
        assert_eq!(reader_font_size(200), READER_FONT_SIZE_MAX);
        assert_eq!(
            reader_font_size(READER_FONT_SIZE_DEFAULT),
            READER_FONT_SIZE_DEFAULT
        );

        // The applied script always carries the clamped size, whoever asks.
        assert!(
            reader_appearance_script(0).contains(&format!("{READER_FONT_SIZE_MIN}px")),
            "a stored size below the range must not reach the chapter"
        );
        assert!(reader_appearance_script(u8::MAX).contains(&format!("{READER_FONT_SIZE_MAX}px")));
    }
}
