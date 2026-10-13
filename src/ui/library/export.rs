//! Export targets and the payloads handed to the reader windows.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryExportKind {
    Epub,
    Pdf,
    Original,
}

pub(crate) enum OpenReaderPayload {
    Pdf {
        record: BookRecord,
        book_incarnation: u64,
        bytes: Arc<Vec<u8>>,
        pages: Vec<PdfReaderPage>,
        initial_page: u32,
        document_revision: u64,
    },
    /// A DjVu scan: reading needs the published page images, which the reader
    /// prepares asynchronously after the document validation succeeds here.
    PageImages {
        record: BookRecord,
        initial_page: u32,
    },
    Reflowable {
        record: BookRecord,
        book_incarnation: u64,
        opened: OpenedBook,
        initial_spine: usize,
        /// 上次阅读留下的章内位置；只从书架打开这本书时才有，用于首个章节
        /// 加载后的滚动恢复。
        initial_anchor: Option<TextAnchor>,
        initial_citation: Option<reader::ReflowableCitationTarget>,
        progress_locators: Vec<DocumentLocator>,
        annotation_revisions: (u64, Vec<u64>),
    },
}

pub(crate) fn reflowable_progress_locators(
    record: &BookRecord,
    document: &BookDocument,
    opened: &OpenedBook,
) -> Result<Vec<DocumentLocator>> {
    ensure!(document.id == record.id, "阅读文档与图书记录不匹配");
    ensure!(
        document.units.len() == opened.spine.len(),
        "阅读投影与统一文档内容单元数量不一致"
    );
    Ok(document
        .units
        .iter()
        .map(|unit| DocumentLocator::unit(&record.id, &unit.id))
        .collect())
}

pub(crate) struct PdfReaderWindowRequest {
    pub(crate) record: BookRecord,
    pub(crate) book_incarnation: u64,
    pub(crate) bytes: Arc<Vec<u8>>,
    pub(crate) pages: Vec<PdfReaderPage>,
    pub(crate) initial_page: u32,
    pub(crate) document_revision: u64,
    pub(crate) library: LibraryStore,
    pub(crate) services: Arc<AppServices>,
    pub(crate) library_view: Entity<EpubReaderApp>,
    pub(crate) persist_progress: bool,
    pub(crate) preview_label: String,
    /// Set when this window is the book's single reading window.
    pub(crate) singleton_key: Option<String>,
}

impl LibraryExportKind {
    pub(super) fn format(self) -> ExportFormat {
        match self {
            Self::Epub => ExportFormat::Epub,
            Self::Pdf => ExportFormat::Pdf,
            Self::Original => ExportFormat::Original,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Epub => "EPUB",
            Self::Pdf => "PDF",
            Self::Original => "原件",
        }
    }
}

pub(crate) fn export_extension(kind: LibraryExportKind, original_extension: Option<&str>) -> &str {
    match kind {
        LibraryExportKind::Epub => "epub",
        LibraryExportKind::Pdf => "pdf",
        LibraryExportKind::Original => original_extension.unwrap_or("bin"),
    }
}

pub(crate) fn suggested_library_export_filename(
    book: &BookRecord,
    kind: LibraryExportKind,
    original_extension: Option<&str>,
) -> String {
    let epub = suggested_epub_filename(&book.title);
    let stem = epub.strip_suffix(".epub").unwrap_or("book");
    format!("{stem}.{}", export_extension(kind, original_extension))
}

pub(crate) fn canonical_original_extension(document: &BookDocument) -> Result<&'static str> {
    let CanonicalBookSource::Imported { format, .. } = &document.source else {
        anyhow::bail!("这本图书是应用内新建的，没有可导出的原文件；请选择 EPUB 或 PDF")
    };
    Ok(match format {
        BookFormat::Epub => "epub",
        BookFormat::Pdf => "pdf",
        BookFormat::Doc => "doc",
        BookFormat::Docx => "docx",
        BookFormat::Pptx => "pptx",
        BookFormat::Xlsx => "xlsx",
        BookFormat::Mobi => "mobi",
        BookFormat::Azw => "azw",
        BookFormat::Azw3 => "azw3",
        BookFormat::Kfx => "kfx",
        BookFormat::Djvu => "djvu",
    })
}
