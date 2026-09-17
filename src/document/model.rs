//! The book-level model: formats, sources, units and the document root.

use super::*;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookFormat {
    Epub,
    Pdf,
    Doc,
    Docx,
    Pptx,
    Xlsx,
    Mobi,
    Azw,
    Azw3,
    /// Amazon Kindle Format 10 container (`CONT`). Structured, reflowable
    /// content; DRM-protected files are rejected at import time.
    Kfx,
    /// DjVu scanned-document container. Pages are rendered as images by the
    /// visual pipeline and the optional hidden text layer becomes the page
    /// text.
    Djvu,
}

/// Describes where a book came from. The original imported file is referenced
/// by an opaque asset ID; storage-specific keys belong outside this model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BookSource {
    Created,
    Imported {
        format: BookFormat,
        original_asset_id: String,
        original_file_name: Option<String>,
    },
}

impl BookSource {
    pub const fn created() -> Self {
        Self::Created
    }

    pub fn imported(
        format: BookFormat,
        original_asset_id: impl Into<String>,
        original_file_name: Option<String>,
    ) -> Self {
        Self::Imported {
            format,
            original_asset_id: original_asset_id.into(),
            original_file_name,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentUnitKind {
    Chapter,
    Section,
    Page,
    Slide,
    Worksheet,
}

/// Identifies the native source fragment from which a content unit was
/// produced. Numeric page/section/slide indexes are deliberately one-based so
/// they can be shown directly in citations and import diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceLocator {
    Created,
    Epub {
        href: String,
    },
    PdfPage {
        page: u32,
    },
    /// One-based page in a temporary PDF exported by Microsoft Office for an
    /// enhanced visual preview. This is deliberately distinct from
    /// [`SourceLocator::PdfPage`], which always identifies a page in an
    /// imported PDF source.
    OfficeRenderedPage {
        page: u32,
    },
    OfficeSection {
        index: u32,
    },
    Slide {
        index: u32,
    },
    Worksheet {
        name: String,
        range: Option<String>,
    },
    KindleSection {
        index: u32,
        href: Option<String>,
    },
    /// One-based page within an imported DjVu document. The page image is
    /// produced by the visual pipeline; the locator identifies the exact
    /// source page for citations and reading progress.
    DjvuPage {
        page: u32,
    },
}

impl SourceLocator {
    pub const fn created() -> Self {
        Self::Created
    }

    pub fn epub(href: impl Into<String>) -> Self {
        Self::Epub { href: href.into() }
    }

    pub const fn pdf_page(page: u32) -> Self {
        Self::PdfPage { page }
    }

    pub const fn office_rendered_page(page: u32) -> Self {
        Self::OfficeRenderedPage { page }
    }

    pub const fn office_section(index: u32) -> Self {
        Self::OfficeSection { index }
    }

    pub const fn slide(index: u32) -> Self {
        Self::Slide { index }
    }

    pub fn worksheet(name: impl Into<String>, range: Option<String>) -> Self {
        Self::Worksheet {
            name: name.into(),
            range,
        }
    }

    pub fn kindle_section(index: u32, href: Option<String>) -> Self {
        Self::KindleSection { index, href }
    }

    pub const fn djvu_page(page: u32) -> Self {
        Self::DjvuPage { page }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("source_locator", &mut validator);
        validator.finish()
    }

    pub(crate) fn validate_into(&self, path: &str, validator: &mut Validator) {
        match self {
            Self::Created => {}
            Self::Epub { href } => {
                validate_source_location_text(validator, &format!("{path}.href"), href, "EPUB href")
            }
            Self::PdfPage { page } => {
                validate_one_based(validator, &format!("{path}.page"), *page, "PDF page")
            }
            Self::OfficeRenderedPage { page } => validate_one_based(
                validator,
                &format!("{path}.page"),
                *page,
                "Office rendered page",
            ),
            Self::OfficeSection { index } => validate_one_based(
                validator,
                &format!("{path}.index"),
                *index,
                "Office section index",
            ),
            Self::Slide { index } => {
                validate_one_based(validator, &format!("{path}.index"), *index, "slide index")
            }
            Self::Worksheet { name, range } => {
                validate_required_text(validator, &format!("{path}.name"), name);
                if let Some(range) = range {
                    validate_source_location_text(
                        validator,
                        &format!("{path}.range"),
                        range,
                        "worksheet range",
                    );
                }
            }
            Self::KindleSection { index, href } => {
                validate_one_based(
                    validator,
                    &format!("{path}.index"),
                    *index,
                    "Kindle section index",
                );
                if let Some(href) = href {
                    validate_source_location_text(
                        validator,
                        &format!("{path}.href"),
                        href,
                        "Kindle href",
                    );
                }
            }
            Self::DjvuPage { page } => {
                validate_one_based(validator, &format!("{path}.page"), *page, "DjVu page")
            }
        }
    }
}

/// One editable/readable unit in linear reading order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentUnit {
    pub id: String,
    pub revision: Revision,
    pub kind: ContentUnitKind,
    pub title: String,
    pub source_locator: Option<SourceLocator>,
    /// The current HTML projection. Importers may preserve the
    /// original spelling until the first semantic block edit.
    pub source: String,
    /// The semantic editing and indexing representation for `source`.
    pub document: BlockDocument,
}

impl ContentUnit {
    pub fn new(
        id: impl Into<String>,
        kind: ContentUnitKind,
        title: impl Into<String>,
        source: impl Into<String>,
        document: BlockDocument,
    ) -> Self {
        Self {
            id: id.into(),
            revision: Revision::INITIAL,
            kind,
            title: title.into(),
            source_locator: None,
            source: source.into(),
            document,
        }
    }

    pub fn empty(id: impl Into<String>, kind: ContentUnitKind, title: impl Into<String>) -> Self {
        Self::new(id, kind, title, String::new(), BlockDocument::default())
    }

    pub fn plain_text(&self) -> String {
        self.document.plain_text()
    }

    pub fn content_hash(&self) -> String {
        self.document.content_hash()
    }

    pub fn find_block(&self, block_id: &str) -> Option<&Block> {
        self.document.find_block(block_id)
    }

    pub fn with_source_locator(mut self, source_locator: SourceLocator) -> Self {
        self.source_locator = Some(source_locator);
        self
    }
}

/// The format-neutral, ordered representation of a book.
///
/// `units` defines linear reading order. `toc` is a separate hierarchy and may
/// omit a unit or contain more than one entry targeting the same unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookDocument {
    pub schema_version: u32,
    pub id: String,
    pub revision: Revision,
    pub title: String,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    pub source: BookSource,
    pub cover_asset_id: Option<String>,
    pub units: Vec<ContentUnit>,
    pub toc: Vec<TocNode>,
    pub assets: Vec<AssetRef>,
}

impl BookDocument {
    pub fn new(id: impl Into<String>, title: impl Into<String>, source: BookSource) -> Self {
        Self {
            schema_version: DOCUMENT_SCHEMA_VERSION,
            id: id.into(),
            revision: Revision::INITIAL,
            title: title.into(),
            authors: Vec::new(),
            language: None,
            description: None,
            source,
            cover_asset_id: None,
            units: Vec::new(),
            toc: Vec::new(),
            assets: Vec::new(),
        }
    }

    pub fn created(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self::new(id, title, BookSource::Created)
    }

    pub fn find_unit(&self, unit_id: &str) -> Option<&ContentUnit> {
        self.units.iter().find(|unit| unit.id == unit_id)
    }

    pub fn find_asset(&self, asset_id: &str) -> Option<&AssetRef> {
        self.assets.iter().find(|asset| asset.id == asset_id)
    }

    /// Text used as the format-neutral basis for indexing and embeddings.
    pub fn plain_text(&self) -> String {
        let mut sections = Vec::new();
        push_non_blank(&mut sections, &self.title);
        if !self.authors.is_empty() {
            push_non_blank(&mut sections, &self.authors.join(", "));
        }
        if let Some(description) = self.description.as_deref() {
            push_non_blank(&mut sections, description);
        }
        for unit in &self.units {
            push_non_blank(&mut sections, &unit.title);
            push_non_blank(&mut sections, &unit.plain_text());
        }
        sections.join("\n\n")
    }

    /// A deterministic hash of the serialized document, including IDs and
    /// revisions. `BlockDocument::content_hash` should be used for a chapter
    /// selection that must remain stable across unrelated book metadata edits.
    pub fn document_hash(&self) -> String {
        stable_json_hash(self)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        if self.schema_version != DOCUMENT_SCHEMA_VERSION {
            validator.issue(
                "schema_version",
                format!(
                    "unsupported schema version {}; expected {DOCUMENT_SCHEMA_VERSION}",
                    self.schema_version
                ),
            );
        }
        validate_id(&mut validator, "id", &self.id);
        validate_required_text(&mut validator, "title", &self.title);
        for (index, author) in self.authors.iter().enumerate() {
            validate_required_text(&mut validator, &format!("authors[{index}]"), author);
        }
        if let Some(language) = self.language.as_deref() {
            validate_required_text(&mut validator, "language", language);
        }

        let mut asset_ids = HashSet::new();
        let mut assets = HashMap::new();
        for (index, asset) in self.assets.iter().enumerate() {
            let path = format!("assets[{index}]");
            asset.validate_into(&path, &mut validator);
            if !asset.id.is_empty() && !asset_ids.insert(asset.id.as_str()) {
                validator.issue(format!("{path}.id"), "duplicate asset ID");
            }
            assets.entry(asset.id.as_str()).or_insert(asset);
        }

        match &self.source {
            BookSource::Created => {}
            BookSource::Imported {
                original_asset_id,
                original_file_name,
                ..
            } => {
                validate_id(
                    &mut validator,
                    "source.original_asset_id",
                    original_asset_id,
                );
                validate_optional_file_name(
                    &mut validator,
                    "source.original_file_name",
                    original_file_name.as_deref(),
                );
                validate_asset_use(
                    &mut validator,
                    "source.original_asset_id",
                    original_asset_id,
                    &assets,
                    &[AssetRole::OriginalSource],
                );
            }
        }

        if let Some(cover_asset_id) = self.cover_asset_id.as_deref() {
            validate_id(&mut validator, "cover_asset_id", cover_asset_id);
            validate_asset_use(
                &mut validator,
                "cover_asset_id",
                cover_asset_id,
                &assets,
                &[AssetRole::Cover],
            );
        }

        let mut unit_ids = HashSet::new();
        let mut units = HashMap::new();
        for (index, unit) in self.units.iter().enumerate() {
            let path = format!("units[{index}]");
            validate_id(&mut validator, &format!("{path}.id"), &unit.id);
            if !unit.id.is_empty() && !unit_ids.insert(unit.id.as_str()) {
                validator.issue(format!("{path}.id"), "duplicate content-unit ID");
            }
            if let Some(source_locator) = &unit.source_locator {
                source_locator.validate_into(&format!("{path}.source_locator"), &mut validator);
            }
            unit.document
                .validate_into(&format!("{path}.document"), &mut validator);
            validate_block_asset_uses(
                &unit.document.blocks,
                &format!("{path}.document.blocks"),
                0,
                &assets,
                &mut validator,
            );
            units.entry(unit.id.as_str()).or_insert(unit);
        }

        let mut toc_ids = HashSet::new();
        validate_toc_nodes(&self.toc, "toc", 0, &units, &mut toc_ids, &mut validator);
        validator.finish()
    }

    pub fn validate_locator(&self, locator: &DocumentLocator) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        locator.validate_into("locator", &mut validator);
        if locator.book_id != self.id {
            validator.issue("locator.book_id", "locator belongs to a different book");
        }

        match self.find_unit(&locator.unit_id) {
            None => validator.issue("locator.unit_id", "content unit does not exist"),
            Some(unit) => {
                let valid_document = unit.document.validate().is_ok();
                if !valid_document {
                    validator.issue(
                        "locator.unit_id",
                        "content unit has an invalid block document",
                    );
                }
                if valid_document && let Some(block_id) = locator.block_id.as_deref() {
                    match unit.find_block(block_id) {
                        Some(block) => {
                            if let Some(range) = locator.text_range {
                                validate_text_range(&block.plain_text(), range, &mut validator);
                            }
                        }
                        None => validator
                            .issue("locator.block_id", "block does not exist in content unit"),
                    }
                }
            }
        }
        validator.finish()
    }
}
