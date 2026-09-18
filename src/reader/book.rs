//! The opened book, its spine, table of contents and resource responses.

use super::*;

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

    pub(crate) fn from_epub(epub: Arc<Epub>) -> Result<Self> {
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
