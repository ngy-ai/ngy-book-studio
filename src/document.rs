//! Format-neutral book document model.
//!
//! The types in this module deliberately do not expose database rows, native
//! file paths, object-store keys, or WebView-specific state. They form the
//! stable boundary shared by importers, exporters, the editor, search, and AI
//! citations.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::mem;

use html5ever::{parse_document, tendril::TendrilSink as _};
use markup5ever_rcdom::{NodeData, RcDom};
use serde::{Deserialize, Serialize};

pub const DOCUMENT_SCHEMA_VERSION: u32 = 2;
pub const BLOCK_SCHEMA_VERSION: u32 = 1;
/// Visual coordinates are normalized to this inclusive upper bound so a
/// persisted locator does not depend on the renderer's pixel dimensions.
pub const NORMALIZED_COORDINATE_MAX: u16 = 1_000;
// Keep the semantic model limit comfortably below serde_json's default 128
// container limit. A nested list level adds an item object and two arrays in
// addition to the child block object, so a limit close to 64 cannot make a
// valid BookDocument JSON-round-trippable.
pub const MAX_DOCUMENT_DEPTH: usize = 24;

thread_local! {
    static MODEL_SERDE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct ModelSerdeDepthGuard {
    previous: usize,
}

impl Drop for ModelSerdeDepthGuard {
    fn drop(&mut self) {
        MODEL_SERDE_DEPTH.with(|depth| depth.set(self.previous));
    }
}

fn enter_model_serde_depth() -> Option<ModelSerdeDepthGuard> {
    MODEL_SERDE_DEPTH.with(|depth| {
        let previous = depth.get();
        if previous > MAX_DOCUMENT_DEPTH {
            return None;
        }
        depth.set(previous + 1);
        Some(ModelSerdeDepthGuard { previous })
    })
}

/// A persisted revision. Editor-session revisions should use a separate
/// monotonic counter and must not be confused with this value.
#[derive(
    Clone, Copy, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub const INITIAL: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Creates a deterministic, content-derived identifier without requiring a
/// random-number generator. The full BLAKE3 digest is retained.
pub fn deterministic_id(prefix: &str, seed: impl AsRef<[u8]>) -> String {
    let prefix = prefix.trim();
    let seed = seed.as_ref();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(prefix.len() as u64).to_le_bytes());
    hasher.update(prefix.as_bytes());
    hasher.update(&(seed.len() as u64).to_le_bytes());
    hasher.update(seed);
    let digest = hasher.finalize().to_hex();
    if prefix.is_empty() {
        digest.to_string()
    } else {
        format!("{prefix}-{digest}")
    }
}

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

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("source_locator", &mut validator);
        validator.finish()
    }

    fn validate_into(&self, path: &str, validator: &mut Validator) {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDocument {
    pub schema_version: u32,
    pub blocks: Vec<Block>,
}

impl Default for BlockDocument {
    fn default() -> Self {
        Self {
            schema_version: BLOCK_SCHEMA_VERSION,
            blocks: Vec::new(),
        }
    }
}

impl BlockDocument {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            schema_version: BLOCK_SCHEMA_VERSION,
            blocks,
        }
    }

    pub fn plain_text(&self) -> String {
        join_block_text_at_depth(&self.blocks, "\n\n", 0)
    }

    pub fn content_hash(&self) -> String {
        stable_json_hash(self)
    }

    pub fn find_block(&self, block_id: &str) -> Option<&Block> {
        self.blocks
            .iter()
            .find_map(|block| block.find_block(block_id, 0))
    }

    pub fn referenced_asset_ids(&self) -> Vec<&str> {
        let mut result = Vec::new();
        for block in &self.blocks {
            block.collect_asset_ids(&mut result, 0);
        }
        result
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("document", &mut validator);
        validator.finish()
    }

    fn validate_into(&self, path: &str, validator: &mut Validator) {
        if self.schema_version != BLOCK_SCHEMA_VERSION {
            validator.issue(
                format!("{path}.schema_version"),
                format!(
                    "unsupported block schema version {}; expected {BLOCK_SCHEMA_VERSION}",
                    self.schema_version
                ),
            );
        }
        let mut block_ids = HashSet::new();
        validate_blocks(
            &self.blocks,
            &format!("{path}.blocks"),
            0,
            &mut block_ids,
            validator,
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph {
        id: String,
        content: Vec<Inline>,
    },
    Heading {
        id: String,
        level: u8,
        content: Vec<Inline>,
    },
    BlockQuote {
        id: String,
        blocks: Vec<Block>,
    },
    BulletList {
        id: String,
        items: Vec<ListItem>,
    },
    OrderedList {
        id: String,
        start: u64,
        items: Vec<ListItem>,
    },
    CodeBlock {
        id: String,
        language: Option<String>,
        code: String,
    },
    ThematicBreak {
        id: String,
    },
    Table {
        id: String,
        header: Option<TableRow>,
        rows: Vec<TableRow>,
    },
    Image {
        id: String,
        asset_id: String,
        alt: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Audio {
        id: String,
        asset_id: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Video {
        id: String,
        asset_id: String,
        poster_asset_id: Option<String>,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    /// An unsupported but preserved HTML subtree. `plain_text` must be the
    /// inert visible-text projection created by the HTML importer.
    RawHtml {
        id: String,
        source: String,
        plain_text: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Block", tag = "type", rename_all = "snake_case")]
enum BlockSerde {
    Paragraph {
        id: String,
        content: Vec<Inline>,
    },
    Heading {
        id: String,
        level: u8,
        content: Vec<Inline>,
    },
    BlockQuote {
        id: String,
        blocks: Vec<Block>,
    },
    BulletList {
        id: String,
        items: Vec<ListItem>,
    },
    OrderedList {
        id: String,
        start: u64,
        items: Vec<ListItem>,
    },
    CodeBlock {
        id: String,
        language: Option<String>,
        code: String,
    },
    ThematicBreak {
        id: String,
    },
    Table {
        id: String,
        header: Option<TableRow>,
        rows: Vec<TableRow>,
    },
    Image {
        id: String,
        asset_id: String,
        alt: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Audio {
        id: String,
        asset_id: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Video {
        id: String,
        asset_id: String,
        poster_asset_id: Option<String>,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    RawHtml {
        id: String,
        source: String,
        plain_text: String,
    },
}

impl Serialize for Block {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::ser::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        BlockSerde::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::de::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        BlockSerde::deserialize(deserializer)
    }
}

impl Block {
    pub fn paragraph(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Paragraph {
            id: id.into(),
            content: vec![Inline::text(text)],
        }
    }

    pub fn heading(id: impl Into<String>, level: u8, text: impl Into<String>) -> Self {
        Self::Heading {
            id: id.into(),
            level,
            content: vec![Inline::text(text)],
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Paragraph { id, .. }
            | Self::Heading { id, .. }
            | Self::BlockQuote { id, .. }
            | Self::BulletList { id, .. }
            | Self::OrderedList { id, .. }
            | Self::CodeBlock { id, .. }
            | Self::ThematicBreak { id }
            | Self::Table { id, .. }
            | Self::Image { id, .. }
            | Self::Audio { id, .. }
            | Self::Video { id, .. }
            | Self::RawHtml { id, .. } => id,
        }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    fn plain_text_at_depth(&self, depth: usize) -> String {
        if depth > MAX_DOCUMENT_DEPTH {
            return String::new();
        }
        match self {
            Self::Paragraph { content, .. } | Self::Heading { content, .. } => {
                inline_plain_text_at_depth(content, depth + 1)
            }
            Self::BlockQuote { blocks, .. } => join_block_text_at_depth(blocks, "\n\n", depth + 1),
            Self::BulletList { items, .. } | Self::OrderedList { items, .. } => items
                .iter()
                .map(|item| item.plain_text_at_depth(depth + 1))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::CodeBlock { code, .. } => code.clone(),
            Self::ThematicBreak { .. } => String::new(),
            Self::Table { header, rows, .. } => header
                .iter()
                .chain(rows.iter())
                .map(|row| row.plain_text_at_depth(depth + 1))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Image {
                alt,
                title,
                caption,
                ..
            } => {
                let caption = inline_plain_text_at_depth(caption, depth + 1);
                join_visible_text([
                    title.as_deref().unwrap_or_default(),
                    alt.as_str(),
                    caption.as_str(),
                ])
            }
            Self::Audio { title, caption, .. } | Self::Video { title, caption, .. } => {
                let caption = inline_plain_text_at_depth(caption, depth + 1);
                join_visible_text([title.as_deref().unwrap_or_default(), caption.as_str()])
            }
            Self::RawHtml { plain_text, .. } => plain_text.clone(),
        }
    }

    fn find_block(&self, block_id: &str, depth: usize) -> Option<&Block> {
        if depth > MAX_DOCUMENT_DEPTH {
            return None;
        }
        if self.id() == block_id {
            return Some(self);
        }
        match self {
            Self::BlockQuote { blocks, .. } => blocks
                .iter()
                .find_map(|block| block.find_block(block_id, depth + 1)),
            Self::BulletList { items, .. } | Self::OrderedList { items, .. } => items
                .iter()
                .flat_map(|item| item.blocks.iter())
                .find_map(|block| block.find_block(block_id, depth + 1)),
            _ => None,
        }
    }

    fn collect_asset_ids<'a>(&'a self, output: &mut Vec<&'a str>, depth: usize) {
        if depth > MAX_DOCUMENT_DEPTH {
            return;
        }
        match self {
            Self::BlockQuote { blocks, .. } => {
                for block in blocks {
                    block.collect_asset_ids(output, depth + 1);
                }
            }
            Self::BulletList { items, .. } | Self::OrderedList { items, .. } => {
                for item in items {
                    for block in &item.blocks {
                        block.collect_asset_ids(output, depth + 1);
                    }
                }
            }
            Self::Paragraph { content, .. } | Self::Heading { content, .. } => {
                collect_inline_asset_ids(content, output, depth + 1)
            }
            Self::Table { header, rows, .. } => {
                for row in header.iter().chain(rows.iter()) {
                    for cell in &row.cells {
                        collect_inline_asset_ids(&cell.content, output, depth + 1);
                    }
                }
            }
            Self::Image {
                asset_id, caption, ..
            }
            | Self::Audio {
                asset_id, caption, ..
            } => {
                output.push(asset_id);
                collect_inline_asset_ids(caption, output, depth + 1);
            }
            Self::Video {
                asset_id,
                poster_asset_id,
                caption,
                ..
            } => {
                output.push(asset_id);
                if let Some(poster_asset_id) = poster_asset_id.as_deref() {
                    output.push(poster_asset_id);
                }
                collect_inline_asset_ids(caption, output, depth + 1);
            }
            Self::CodeBlock { .. } | Self::ThematicBreak { .. } | Self::RawHtml { .. } => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItem {
    /// `Some` denotes a GFM task-list item; `None` denotes a normal item.
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

impl ListItem {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            checked: None,
            blocks,
        }
    }

    pub fn task(checked: bool, blocks: Vec<Block>) -> Self {
        Self {
            checked: Some(checked),
            blocks,
        }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    fn plain_text_at_depth(&self, depth: usize) -> String {
        join_block_text_at_depth(&self.blocks, "\n", depth)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

impl TableRow {
    pub fn new(cells: Vec<TableCell>) -> Self {
        Self { cells }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    fn plain_text_at_depth(&self, depth: usize) -> String {
        self.cells
            .iter()
            .map(|cell| cell.plain_text_at_depth(depth))
            .collect::<Vec<_>>()
            .join("\t")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCell {
    pub content: Vec<Inline>,
}

impl TableCell {
    pub fn new(content: Vec<Inline>) -> Self {
        Self { content }
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self::new(vec![Inline::text(value)])
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    fn plain_text_at_depth(&self, depth: usize) -> String {
        inline_plain_text_at_depth(&self.content, depth)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text {
        value: String,
    },
    Emphasis {
        content: Vec<Inline>,
    },
    Strong {
        content: Vec<Inline>,
    },
    Strikethrough {
        content: Vec<Inline>,
    },
    Code {
        value: String,
    },
    Link {
        href: String,
        title: Option<String>,
        content: Vec<Inline>,
    },
    HardBreak,
    SoftBreak,
    Image {
        asset_id: String,
        alt: String,
        title: Option<String>,
    },
    RawHtml {
        source: String,
        plain_text: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Inline", tag = "type", rename_all = "snake_case")]
enum InlineSerde {
    Text {
        value: String,
    },
    Emphasis {
        content: Vec<Inline>,
    },
    Strong {
        content: Vec<Inline>,
    },
    Strikethrough {
        content: Vec<Inline>,
    },
    Code {
        value: String,
    },
    Link {
        href: String,
        title: Option<String>,
        content: Vec<Inline>,
    },
    HardBreak,
    SoftBreak,
    Image {
        asset_id: String,
        alt: String,
        title: Option<String>,
    },
    RawHtml {
        source: String,
        plain_text: String,
    },
}

impl Serialize for Inline {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::ser::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        InlineSerde::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for Inline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::de::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        InlineSerde::deserialize(deserializer)
    }
}

impl Inline {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text {
            value: value.into(),
        }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    fn plain_text_at_depth(&self, depth: usize) -> String {
        if depth > MAX_DOCUMENT_DEPTH {
            return String::new();
        }
        match self {
            Self::Text { value } | Self::Code { value } => value.clone(),
            Self::Emphasis { content }
            | Self::Strong { content }
            | Self::Strikethrough { content }
            | Self::Link { content, .. } => inline_plain_text_at_depth(content, depth + 1),
            Self::HardBreak => "\n".to_string(),
            Self::SoftBreak => " ".to_string(),
            Self::Image { alt, title, .. } => {
                join_visible_text([title.as_deref().unwrap_or_default(), alt.as_str()])
            }
            Self::RawHtml { plain_text, .. } => plain_text.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocNode {
    pub id: String,
    pub label: String,
    pub target: TocTarget,
    pub children: Vec<TocNode>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "TocNode")]
struct TocNodeSerde {
    id: String,
    label: String,
    target: TocTarget,
    children: Vec<TocNode>,
}

impl Serialize for TocNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::ser::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        TocNodeSerde::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for TocNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::de::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        TocNodeSerde::deserialize(deserializer)
    }
}

impl TocNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>, target: TocTarget) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            target,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TocTarget {
    Unit { unit_id: String },
    Block { unit_id: String, block_id: String },
}

impl TocTarget {
    pub fn unit(unit_id: impl Into<String>) -> Self {
        Self::Unit {
            unit_id: unit_id.into(),
        }
    }

    pub fn block(unit_id: impl Into<String>, block_id: impl Into<String>) -> Self {
        Self::Block {
            unit_id: unit_id.into(),
            block_id: block_id.into(),
        }
    }

    pub fn unit_id(&self) -> &str {
        match self {
            Self::Unit { unit_id } | Self::Block { unit_id, .. } => unit_id,
        }
    }
}

/// A stable location in a document. Text offsets are UTF-8 byte offsets within
/// `Block::plain_text`, and `end_byte` is exclusive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLocator {
    pub book_id: String,
    pub unit_id: String,
    pub block_id: Option<String>,
    pub text_range: Option<TextRange>,
    /// Native-source coordinate for citations that need more than a block or
    /// UTF-8 text span. Visual worksheet pages use this to retain their exact
    /// A1 range, while slide/page renderers retain the one-based native page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocator>,
    /// Optional visual sub-region in normalized 0..=1000 page coordinates.
    /// Existing locators without this field continue to identify the complete
    /// logical page or content unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<NormalizedRect>,
}

impl DocumentLocator {
    pub fn unit(book_id: impl Into<String>, unit_id: impl Into<String>) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            block_id: None,
            text_range: None,
            source: None,
            region: None,
        }
    }

    pub fn block(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        block_id: impl Into<String>,
    ) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            block_id: Some(block_id.into()),
            text_range: None,
            source: None,
            region: None,
        }
    }

    pub fn text(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        block_id: impl Into<String>,
        start_byte: u64,
        end_byte: u64,
    ) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            block_id: Some(block_id.into()),
            text_range: Some(TextRange::new(start_byte, end_byte)),
            source: None,
            region: None,
        }
    }

    pub fn with_source(mut self, source: SourceLocator) -> Self {
        self.source = Some(source);
        self
    }

    pub fn with_region(mut self, region: NormalizedRect) -> Self {
        self.region = Some(region);
        self
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("locator", &mut validator);
        validator.finish()
    }

    fn validate_into(&self, path: &str, validator: &mut Validator) {
        validate_id(validator, &format!("{path}.book_id"), &self.book_id);
        validate_id(validator, &format!("{path}.unit_id"), &self.unit_id);
        if let Some(block_id) = self.block_id.as_deref() {
            validate_id(validator, &format!("{path}.block_id"), block_id);
        }
        if let Some(range) = self.text_range {
            if self.block_id.is_none() {
                validator.issue(
                    format!("{path}.text_range"),
                    "a text range requires a block ID",
                );
            }
            if range.start_byte > range.end_byte {
                validator.issue(
                    format!("{path}.text_range"),
                    "start_byte must not exceed end_byte",
                );
            }
        }
        if let Some(source) = &self.source {
            source.validate_into(&format!("{path}.source"), validator);
        }
        if let Some(region) = self.region {
            region.validate_into(&format!("{path}.region"), validator);
        }
    }
}

/// A strict, positive-area rectangle in normalized page coordinates. The
/// right and bottom edges are exclusive for containment calculations, while
/// still using the inclusive coordinate domain 0..=1000.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRect {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl NormalizedRect {
    pub const fn new(left: u16, top: u16, right: u16, bottom: u16) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("region", &mut validator);
        validator.finish()
    }

    fn validate_into(&self, path: &str, validator: &mut Validator) {
        if self.left > NORMALIZED_COORDINATE_MAX
            || self.top > NORMALIZED_COORDINATE_MAX
            || self.right > NORMALIZED_COORDINATE_MAX
            || self.bottom > NORMALIZED_COORDINATE_MAX
        {
            validator.issue(
                path,
                format!(
                    "normalized rectangle coordinates must be within 0..={NORMALIZED_COORDINATE_MAX}"
                ),
            );
        }
        if self.left >= self.right || self.top >= self.bottom {
            validator.issue(path, "normalized rectangle must have positive area");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start_byte: u64,
    pub end_byte: u64,
}

impl TextRange {
    pub const fn new(start_byte: u64, end_byte: u64) -> Self {
        Self {
            start_byte,
            end_byte,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetRole {
    OriginalSource,
    Cover,
    ContentImage,
    Audio,
    Video,
    Poster,
    Attachment,
    Font,
    Stylesheet,
}

/// Storage-neutral metadata for an immutable asset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: String,
    pub roles: Vec<AssetRole>,
    pub media_type: String,
    pub original_file_name: Option<String>,
    pub byte_len: u64,
    pub content_hash: String,
}

impl AssetRef {
    pub fn new(
        id: impl Into<String>,
        roles: Vec<AssetRole>,
        media_type: impl Into<String>,
        original_file_name: Option<String>,
        byte_len: u64,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            roles,
            media_type: media_type.into(),
            original_file_name,
            byte_len,
            content_hash: content_hash.into(),
        }
    }

    pub fn from_bytes(
        role: AssetRole,
        media_type: impl Into<String>,
        original_file_name: Option<String>,
        bytes: &[u8],
    ) -> Self {
        let content_hash = blake3::hash(bytes).to_hex().to_string();
        Self {
            id: format!("asset-{content_hash}"),
            roles: vec![role],
            media_type: media_type.into(),
            original_file_name,
            byte_len: bytes.len() as u64,
            content_hash,
        }
    }

    pub fn has_role(&self, role: AssetRole) -> bool {
        self.roles.contains(&role)
    }

    pub fn add_role(&mut self, role: AssetRole) {
        if !self.has_role(role) {
            self.roles.push(role);
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("asset", &mut validator);
        validator.finish()
    }

    fn validate_into(&self, path: &str, validator: &mut Validator) {
        validate_id(validator, &format!("{path}.id"), &self.id);
        if self.roles.is_empty() {
            validator.issue(format!("{path}.roles"), "at least one role is required");
        }
        let mut roles = HashSet::new();
        for (index, role) in self.roles.iter().copied().enumerate() {
            if !roles.insert(role) {
                validator.issue(format!("{path}.roles[{index}]"), "duplicate asset role");
            }
        }
        if self.media_type.trim().is_empty()
            || !self.media_type.contains('/')
            || self.media_type.chars().any(char::is_control)
        {
            validator.issue(format!("{path}.media_type"), "invalid media type");
        }
        validate_optional_file_name(
            validator,
            &format!("{path}.original_file_name"),
            self.original_file_name.as_deref(),
        );
        if self.content_hash.len() != 64
            || !self
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            validator.issue(
                format!("{path}.content_hash"),
                "content hash must be a 64-character hexadecimal BLAKE3 digest",
            );
        }
        for role in &self.roles {
            let compatible = match role {
                AssetRole::Cover | AssetRole::ContentImage | AssetRole::Poster => {
                    self.media_type.starts_with("image/")
                        || self.media_type == "application/svg+xml"
                }
                AssetRole::Audio => self.media_type.starts_with("audio/"),
                AssetRole::Video => self.media_type.starts_with("video/"),
                AssetRole::Stylesheet => self.media_type == "text/css",
                AssetRole::OriginalSource | AssetRole::Attachment | AssetRole::Font => true,
            };
            if !compatible {
                validator.issue(
                    format!("{path}.media_type"),
                    format!("media type is incompatible with the {role:?} role"),
                );
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

impl ValidationIssue {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationError {
    pub fn issues(&self) -> &[ValidationIssue] {
        &self.issues
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "document validation failed")?;
        for issue in &self.issues {
            write!(formatter, "; {}: {}", issue.path, issue.message)?;
        }
        Ok(())
    }
}

impl Error for ValidationError {}

#[derive(Default)]
struct Validator {
    issues: Vec<ValidationIssue>,
}

impl Validator {
    fn issue(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue::new(path, message));
    }

    fn finish(self) -> Result<(), ValidationError> {
        if self.issues.is_empty() {
            Ok(())
        } else {
            Err(ValidationError {
                issues: self.issues,
            })
        }
    }
}

fn validate_id(validator: &mut Validator, path: &str, id: &str) {
    if id.trim().is_empty() {
        validator.issue(path, "ID must not be empty");
    } else if id.trim() != id {
        validator.issue(path, "ID must not have leading or trailing whitespace");
    } else if id.chars().any(char::is_control) {
        validator.issue(path, "ID must not contain control characters");
    }
}

fn validate_required_text(validator: &mut Validator, path: &str, value: &str) {
    if value.trim().is_empty() {
        validator.issue(path, "value must not be blank");
    } else if value.chars().any(|character| character == '\0') {
        validator.issue(path, "value must not contain NUL");
    }
}

fn validate_optional_file_name(validator: &mut Validator, path: &str, value: Option<&str>) {
    if let Some(value) = value {
        if value.trim().is_empty() {
            validator.issue(path, "file name must not be blank");
        } else if value.chars().any(|character| character == '\0') {
            validator.issue(path, "file name must not contain NUL");
        }
    }
}

fn validate_source_location_text(validator: &mut Validator, path: &str, value: &str, label: &str) {
    if value.trim().is_empty() {
        validator.issue(path, format!("{label} must not be blank"));
    } else if value.chars().any(char::is_control) {
        validator.issue(path, format!("{label} must not contain control characters"));
    }
}

fn validate_one_based(validator: &mut Validator, path: &str, value: u32, label: &str) {
    if value == 0 {
        validator.issue(path, format!("{label} is one-based and must be at least 1"));
    }
}

fn validate_blocks(
    blocks: &[Block],
    path: &str,
    depth: usize,
    block_ids: &mut HashSet<String>,
    validator: &mut Validator,
) {
    if depth > MAX_DOCUMENT_DEPTH {
        validator.issue(path, format!("block nesting exceeds {MAX_DOCUMENT_DEPTH}"));
        return;
    }
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{index}]");
        let id_path = format!("{block_path}.id");
        validate_id(validator, &id_path, block.id());
        if !block.id().is_empty() && !block_ids.insert(block.id().to_string()) {
            validator.issue(id_path, "duplicate block ID");
        }
        match block {
            Block::Paragraph { content, .. } => validate_inlines(
                content,
                &format!("{block_path}.content"),
                depth + 1,
                validator,
            ),
            Block::Heading { level, content, .. } => {
                if !(1..=6).contains(level) {
                    validator.issue(format!("{block_path}.level"), "heading level must be 1..=6");
                }
                validate_inlines(
                    content,
                    &format!("{block_path}.content"),
                    depth + 1,
                    validator,
                );
            }
            Block::BlockQuote { blocks, .. } => {
                if blocks.is_empty() {
                    validator.issue(
                        format!("{block_path}.blocks"),
                        "block quote must not be empty",
                    );
                }
                validate_blocks(
                    blocks,
                    &format!("{block_path}.blocks"),
                    depth + 1,
                    block_ids,
                    validator,
                );
            }
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                if items.is_empty() {
                    validator.issue(format!("{block_path}.items"), "list must not be empty");
                }
                if let Block::OrderedList { start, .. } = block
                    && *start == 0
                {
                    validator.issue(
                        format!("{block_path}.start"),
                        "ordered-list start must be at least 1",
                    );
                }
                for (item_index, item) in items.iter().enumerate() {
                    let item_path = format!("{block_path}.items[{item_index}].blocks");
                    if item.blocks.is_empty() {
                        validator.issue(&item_path, "list item must not be empty");
                    }
                    validate_blocks(&item.blocks, &item_path, depth + 1, block_ids, validator);
                }
            }
            Block::CodeBlock { language, .. } => {
                if language.as_deref().is_some_and(|language| {
                    language.contains(['\r', '\n']) || language.chars().any(char::is_control)
                }) {
                    validator.issue(
                        format!("{block_path}.language"),
                        "code language must be a single printable line",
                    );
                }
            }
            Block::Table { header, rows, .. } => {
                validate_table(header.as_ref(), rows, &block_path, depth, validator);
            }
            Block::Image {
                asset_id, caption, ..
            }
            | Block::Audio {
                asset_id, caption, ..
            } => {
                validate_id(validator, &format!("{block_path}.asset_id"), asset_id);
                validate_inlines(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    validator,
                );
            }
            Block::Video {
                asset_id,
                poster_asset_id,
                caption,
                ..
            } => {
                validate_id(validator, &format!("{block_path}.asset_id"), asset_id);
                if let Some(poster_asset_id) = poster_asset_id.as_deref() {
                    validate_id(
                        validator,
                        &format!("{block_path}.poster_asset_id"),
                        poster_asset_id,
                    );
                }
                validate_inlines(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    validator,
                );
            }
            Block::RawHtml {
                source, plain_text, ..
            } => validate_raw_html(source, plain_text, &block_path, validator),
            Block::ThematicBreak { .. } => {}
        }
    }
}

fn validate_inlines(inlines: &[Inline], path: &str, depth: usize, validator: &mut Validator) {
    if inlines.is_empty() {
        return;
    }
    if depth > MAX_DOCUMENT_DEPTH {
        validator.issue(path, format!("inline nesting exceeds {MAX_DOCUMENT_DEPTH}"));
        return;
    }
    for (index, inline) in inlines.iter().enumerate() {
        let inline_path = format!("{path}[{index}]");
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content } => validate_inlines(
                content,
                &format!("{inline_path}.content"),
                depth + 1,
                validator,
            ),
            Inline::Link { href, content, .. } => {
                if href.trim().is_empty() || href.chars().any(char::is_control) {
                    validator.issue(format!("{inline_path}.href"), "invalid link target");
                }
                validate_inlines(
                    content,
                    &format!("{inline_path}.content"),
                    depth + 1,
                    validator,
                );
            }
            Inline::Image { asset_id, .. } => {
                validate_id(validator, &format!("{inline_path}.asset_id"), asset_id)
            }
            Inline::RawHtml { source, plain_text } => {
                validate_raw_html(source, plain_text, &inline_path, validator)
            }
            Inline::Text { .. } | Inline::Code { .. } | Inline::HardBreak | Inline::SoftBreak => {}
        }
    }
}

fn validate_raw_html(source: &str, plain_text: &str, path: &str, validator: &mut Validator) {
    if source.is_empty() {
        validator.issue(format!("{path}.source"), "raw HTML must not be empty");
        return;
    }
    if source.contains('\0') {
        validator.issue(format!("{path}.source"), "raw HTML must not contain NUL");
    }
    match raw_html_plain_text(source) {
        None => validator.issue(
            format!("{path}.source"),
            format!("raw HTML nesting exceeds {MAX_DOCUMENT_DEPTH}"),
        ),
        Some(expected) if plain_text != expected => validator.issue(
            format!("{path}.plain_text"),
            "raw HTML plain text must match its inert source projection",
        ),
        Some(_) => {}
    }
}

/// Computes the one canonical inert projection used by RawHtml producers and
/// validators. Traversal is iterative and detaches children as it goes so even
/// a rejected, extremely deep DOM is not recursively dropped.
pub(crate) fn raw_html_plain_text(source: &str) -> Option<String> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    let mut pending = vec![(dom.document.clone(), 0_usize, false)];
    let mut segments = Vec::new();
    let mut too_deep = false;

    while let Some((node, depth, suppressed)) = pending.pop() {
        let mut child_depth = depth;
        let mut child_suppressed = suppressed;
        match &node.data {
            NodeData::Text { contents } if !suppressed => {
                segments.push(contents.borrow().to_string());
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                let transport_wrapper = matches!(tag, "html" | "head" | "body");
                if !transport_wrapper {
                    if depth > MAX_DOCUMENT_DEPTH {
                        too_deep = true;
                    }
                    child_depth = depth + 1;
                }
                child_suppressed = suppressed || matches!(tag, "script" | "style");
                if !child_suppressed
                    && tag == "img"
                    && let Some(alt) = attrs
                        .borrow()
                        .iter()
                        .find(|attribute| attribute.name.local.as_ref() == "alt")
                        .map(|attribute| attribute.value.to_string())
                        .filter(|alt| !alt.trim().is_empty())
                {
                    segments.push(alt);
                }
            }
            _ => {}
        }

        let children = mem::take(&mut *node.children.borrow_mut());
        pending.extend(
            children
                .into_iter()
                .rev()
                .map(|child| (child, child_depth, child_suppressed)),
        );
    }

    if too_deep {
        return None;
    }
    let mut output = String::new();
    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        let mut previous_space = false;
        for character in segment.chars() {
            if character.is_whitespace() {
                if !previous_space {
                    output.push(' ');
                    previous_space = true;
                }
            } else {
                output.push(character);
                previous_space = false;
            }
        }
    }
    Some(output)
}

fn validate_table(
    header: Option<&TableRow>,
    rows: &[TableRow],
    path: &str,
    depth: usize,
    validator: &mut Validator,
) {
    let expected_width = header
        .map(|header| header.cells.len())
        .or_else(|| rows.first().map(|row| row.cells.len()))
        .unwrap_or_default();
    if expected_width == 0 {
        validator.issue(
            format!("{path}.rows"),
            "table must contain at least one cell",
        );
        return;
    }
    for (row_index, row) in header.into_iter().chain(rows.iter()).enumerate() {
        let row_path = format!("{path}.rows[{row_index}]");
        if row.cells.len() != expected_width {
            validator.issue(
                &row_path,
                format!("table row must contain exactly {expected_width} cells"),
            );
        }
        for (cell_index, cell) in row.cells.iter().enumerate() {
            validate_inlines(
                &cell.content,
                &format!("{row_path}.cells[{cell_index}].content"),
                depth + 1,
                validator,
            );
        }
    }
}

fn validate_block_asset_uses(
    blocks: &[Block],
    path: &str,
    depth: usize,
    assets: &HashMap<&str, &AssetRef>,
    validator: &mut Validator,
) {
    // Structural validation already records the depth error. Stop this second
    // traversal at the same boundary so an invalid document cannot overflow
    // the stack before `BookDocument::validate` returns that error.
    if depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{index}]");
        match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                validate_inline_asset_uses(
                    content,
                    &format!("{block_path}.content"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::BlockQuote { blocks, .. } => validate_block_asset_uses(
                blocks,
                &format!("{block_path}.blocks"),
                depth + 1,
                assets,
                validator,
            ),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                for (item_index, item) in items.iter().enumerate() {
                    validate_block_asset_uses(
                        &item.blocks,
                        &format!("{block_path}.items[{item_index}].blocks"),
                        depth + 1,
                        assets,
                        validator,
                    );
                }
            }
            Block::Table { header, rows, .. } => {
                for (row_index, row) in header.iter().chain(rows.iter()).enumerate() {
                    for (cell_index, cell) in row.cells.iter().enumerate() {
                        validate_inline_asset_uses(
                            &cell.content,
                            &format!("{block_path}.rows[{row_index}].cells[{cell_index}].content"),
                            depth + 1,
                            assets,
                            validator,
                        );
                    }
                }
            }
            Block::Image {
                asset_id, caption, ..
            } => {
                validate_asset_use(
                    validator,
                    &format!("{block_path}.asset_id"),
                    asset_id,
                    assets,
                    &[AssetRole::ContentImage, AssetRole::Cover, AssetRole::Poster],
                );
                validate_inline_asset_uses(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::Audio {
                asset_id, caption, ..
            } => {
                validate_asset_use(
                    validator,
                    &format!("{block_path}.asset_id"),
                    asset_id,
                    assets,
                    &[AssetRole::Audio],
                );
                validate_inline_asset_uses(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::Video {
                asset_id,
                poster_asset_id,
                caption,
                ..
            } => {
                validate_asset_use(
                    validator,
                    &format!("{block_path}.asset_id"),
                    asset_id,
                    assets,
                    &[AssetRole::Video],
                );
                if let Some(poster_asset_id) = poster_asset_id.as_deref() {
                    validate_asset_use(
                        validator,
                        &format!("{block_path}.poster_asset_id"),
                        poster_asset_id,
                        assets,
                        &[AssetRole::Poster, AssetRole::ContentImage, AssetRole::Cover],
                    );
                }
                validate_inline_asset_uses(
                    caption,
                    &format!("{block_path}.caption"),
                    depth + 1,
                    assets,
                    validator,
                );
            }
            Block::CodeBlock { .. } | Block::ThematicBreak { .. } | Block::RawHtml { .. } => {}
        }
    }
}

fn validate_inline_asset_uses(
    inlines: &[Inline],
    path: &str,
    depth: usize,
    assets: &HashMap<&str, &AssetRef>,
    validator: &mut Validator,
) {
    if depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    for (index, inline) in inlines.iter().enumerate() {
        let inline_path = format!("{path}[{index}]");
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => validate_inline_asset_uses(
                content,
                &format!("{inline_path}.content"),
                depth + 1,
                assets,
                validator,
            ),
            Inline::Image { asset_id, .. } => validate_asset_use(
                validator,
                &format!("{inline_path}.asset_id"),
                asset_id,
                assets,
                &[AssetRole::ContentImage, AssetRole::Cover, AssetRole::Poster],
            ),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

fn validate_asset_use(
    validator: &mut Validator,
    path: &str,
    asset_id: &str,
    assets: &HashMap<&str, &AssetRef>,
    allowed_roles: &[AssetRole],
) {
    let Some(asset) = assets.get(asset_id).copied() else {
        if !asset_id.is_empty() {
            validator.issue(path, "referenced asset does not exist");
        }
        return;
    };
    if !allowed_roles.iter().any(|role| asset.has_role(*role)) {
        validator.issue(path, "referenced asset has an incompatible role");
    }
}

fn validate_toc_nodes(
    nodes: &[TocNode],
    path: &str,
    depth: usize,
    units: &HashMap<&str, &ContentUnit>,
    toc_ids: &mut HashSet<String>,
    validator: &mut Validator,
) {
    if nodes.is_empty() {
        return;
    }
    if depth > MAX_DOCUMENT_DEPTH {
        validator.issue(path, format!("TOC nesting exceeds {MAX_DOCUMENT_DEPTH}"));
        return;
    }
    for (index, node) in nodes.iter().enumerate() {
        let node_path = format!("{path}[{index}]");
        validate_id(validator, &format!("{node_path}.id"), &node.id);
        if !node.id.is_empty() && !toc_ids.insert(node.id.clone()) {
            validator.issue(format!("{node_path}.id"), "duplicate TOC-node ID");
        }
        validate_required_text(validator, &format!("{node_path}.label"), &node.label);
        let unit_id = node.target.unit_id();
        validate_id(validator, &format!("{node_path}.target.unit_id"), unit_id);
        match units.get(unit_id).copied() {
            Some(unit) => {
                if let TocTarget::Block { block_id, .. } = &node.target {
                    validate_id(validator, &format!("{node_path}.target.block_id"), block_id);
                    if unit.find_block(block_id).is_none() {
                        validator.issue(
                            format!("{node_path}.target.block_id"),
                            "TOC target block does not exist in content unit",
                        );
                    }
                }
            }
            None => validator.issue(
                format!("{node_path}.target.unit_id"),
                "TOC target content unit does not exist",
            ),
        }
        validate_toc_nodes(
            &node.children,
            &format!("{node_path}.children"),
            depth + 1,
            units,
            toc_ids,
            validator,
        );
    }
}

fn validate_text_range(text: &str, range: TextRange, validator: &mut Validator) {
    let Ok(start) = usize::try_from(range.start_byte) else {
        validator.issue("locator.text_range.start_byte", "offset does not fit usize");
        return;
    };
    let Ok(end) = usize::try_from(range.end_byte) else {
        validator.issue("locator.text_range.end_byte", "offset does not fit usize");
        return;
    };
    if start > end {
        return;
    }
    if end > text.len() {
        validator.issue("locator.text_range", "text range exceeds block text");
        return;
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        validator.issue(
            "locator.text_range",
            "text range must end on UTF-8 character boundaries",
        );
    }
}

fn inline_plain_text_at_depth(inlines: &[Inline], depth: usize) -> String {
    if depth > MAX_DOCUMENT_DEPTH {
        return String::new();
    }
    let mut output = String::new();
    for inline in inlines {
        output.push_str(&inline.plain_text_at_depth(depth));
    }
    output
}

fn join_block_text_at_depth(blocks: &[Block], separator: &str, depth: usize) -> String {
    if depth > MAX_DOCUMENT_DEPTH {
        return String::new();
    }
    blocks
        .iter()
        .map(|block| block.plain_text_at_depth(depth))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

fn join_visible_text<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut visible = Vec::<&str>::new();
    for part in parts {
        let part = part.trim();
        if !part.is_empty() && !visible.contains(&part) {
            visible.push(part);
        }
    }
    visible.join("\n")
}

fn push_non_blank(output: &mut Vec<String>, value: &str) {
    if !value.trim().is_empty() {
        output.push(value.to_string());
    }
}

fn collect_inline_asset_ids<'a>(inlines: &'a [Inline], output: &mut Vec<&'a str>, depth: usize) {
    if depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    for inline in inlines {
        match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => collect_inline_asset_ids(content, output, depth + 1),
            Inline::Image { asset_id, .. } => output.push(asset_id),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::HardBreak
            | Inline::SoftBreak
            | Inline::RawHtml { .. } => {}
        }
    }
}

fn stable_json_hash(value: &impl Serialize) -> String {
    match serde_json::to_vec(value) {
        Ok(bytes) => blake3::hash(&bytes).to_hex().to_string(),
        Err(error) => {
            // Public hash helpers cannot return an error without breaking their
            // established API. Invalid over-deep models are nevertheless
            // rejected by Serialize without recursing to the bottom; hash the
            // stable rejection instead of panicking or overflowing the stack.
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"invalid-document\0");
            hasher.update(error.to_string().as_bytes());
            hasher.finalize().to_hex().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_book() -> BookDocument {
        let original = AssetRef::from_bytes(
            AssetRole::OriginalSource,
            "application/epub+zip",
            Some("sample.epub".to_string()),
            b"source",
        );
        let image = AssetRef::from_bytes(
            AssetRole::ContentImage,
            "image/png",
            Some("figure.png".to_string()),
            b"image",
        );
        let audio = AssetRef::from_bytes(
            AssetRole::Audio,
            "audio/mpeg",
            Some("voice.mp3".to_string()),
            b"audio",
        );
        let video = AssetRef::from_bytes(
            AssetRole::Video,
            "video/mp4",
            Some("demo.mp4".to_string()),
            b"video",
        );
        let poster = AssetRef::from_bytes(
            AssetRole::Poster,
            "image/jpeg",
            Some("poster.jpg".to_string()),
            b"poster",
        );

        let heading_id = deterministic_id("block", b"chapter-1/heading");
        let paragraph_id = deterministic_id("block", b"chapter-1/paragraph");
        let list_id = deterministic_id("block", b"chapter-1/list");
        let table_id = deterministic_id("block", b"chapter-1/table");
        let image_id = image.id.clone();
        let audio_id = audio.id.clone();
        let video_id = video.id.clone();
        let poster_id = poster.id.clone();
        let blocks = vec![
            Block::heading(&heading_id, 1, "第一章"),
            Block::Paragraph {
                id: paragraph_id,
                content: vec![
                    Inline::text("你好，"),
                    Inline::Strong {
                        content: vec![Inline::text("世界")],
                    },
                    Inline::SoftBreak,
                    Inline::Link {
                        href: "#details".to_string(),
                        title: None,
                        content: vec![Inline::text("详情")],
                    },
                ],
            },
            Block::BulletList {
                id: list_id,
                items: vec![ListItem::task(
                    true,
                    vec![Block::paragraph(
                        deterministic_id("block", b"chapter-1/list/item-1"),
                        "已完成",
                    )],
                )],
            },
            Block::Table {
                id: table_id,
                header: Some(TableRow::new(vec![TableCell::text("名称")])),
                rows: vec![TableRow::new(vec![TableCell::text("墨页")])],
            },
            Block::Image {
                id: deterministic_id("block", b"chapter-1/image"),
                asset_id: image_id,
                alt: "示意图".to_string(),
                title: None,
                caption: vec![Inline::text("图片说明")],
            },
            Block::Audio {
                id: deterministic_id("block", b"chapter-1/audio"),
                asset_id: audio_id,
                title: Some("朗读".to_string()),
                caption: Vec::new(),
            },
            Block::Video {
                id: deterministic_id("block", b"chapter-1/video"),
                asset_id: video_id,
                poster_asset_id: Some(poster_id),
                title: Some("演示".to_string()),
                caption: vec![Inline::text("视频说明")],
            },
        ];
        let unit = ContentUnit::new(
            "unit-1",
            ContentUnitKind::Chapter,
            "第一章",
            "<h1>第一章</h1>",
            BlockDocument::new(blocks),
        )
        .with_source_locator(SourceLocator::epub("OEBPS/chapter-1.xhtml"));
        let mut book = BookDocument::new(
            "book-1",
            "示例图书",
            BookSource::imported(
                BookFormat::Epub,
                original.id.clone(),
                Some("sample.epub".to_string()),
            ),
        );
        book.authors = vec!["墨页".to_string()];
        book.language = Some("zh-CN".to_string());
        book.description = Some("用于模型测试".to_string());
        book.units.push(unit);
        book.toc.push(TocNode::new(
            "toc-1",
            "第一章",
            TocTarget::block("unit-1", heading_id),
        ));
        book.assets = vec![original, image, audio, video, poster];
        book
    }

    #[test]
    fn serde_round_trip_preserves_the_complete_model() {
        let book = sample_book();
        book.validate().expect("sample model is valid");

        let json = serde_json::to_string_pretty(&book).expect("serialize book");
        let decoded: BookDocument = serde_json::from_str(&json).expect("deserialize book");

        assert_eq!(decoded, book);
        assert!(!json.contains("source_kind"));
        assert_eq!(decoded.schema_version, 2);
        assert!(json.contains(r#""type": "imported""#));
        assert!(json.contains(r#""type": "video""#));
        assert_eq!(decoded.document_hash(), book.document_hash());
    }

    #[test]
    fn previous_document_schema_is_rejected() {
        let mut book = sample_book();
        book.schema_version = DOCUMENT_SCHEMA_VERSION - 1;
        assert!(book.validate().is_err());
    }

    #[test]
    fn valid_depth_boundary_round_trips_through_default_serde_json() {
        let mut block = Block::ThematicBreak {
            id: "boundary-leaf".to_string(),
        };
        for depth in 0..MAX_DOCUMENT_DEPTH {
            block = Block::BulletList {
                id: format!("boundary-list-{depth}"),
                items: vec![ListItem::new(vec![block])],
            };
        }

        let mut book = sample_book();
        book.units[0].document = BlockDocument::new(vec![block]);
        let mut toc = TocNode::new(
            "boundary-toc-leaf",
            "边界叶子",
            TocTarget::unit(&book.units[0].id),
        );
        for depth in 0..MAX_DOCUMENT_DEPTH {
            let mut parent = TocNode::new(
                format!("boundary-toc-{depth}"),
                format!("边界目录 {depth}"),
                TocTarget::unit(&book.units[0].id),
            );
            parent.children.push(toc);
            toc = parent;
        }
        book.toc = vec![toc];
        book.validate()
            .expect("the shared semantic depth boundary is valid");

        let json = serde_json::to_string(&book)
            .expect("the valid boundary must fit serde_json's default recursion limit");
        let decoded: BookDocument = serde_json::from_str(&json)
            .expect("the valid boundary must deserialize with default serde_json settings");
        assert_eq!(decoded, book);
        decoded
            .validate()
            .expect("the round-tripped boundary model remains valid");
    }

    #[test]
    fn serde_rejects_a_toc_beyond_the_shared_depth_limit() {
        let mut toc = TocNode::new("over-toc-leaf", "叶子", TocTarget::unit("unit-1"));
        for depth in 0..=MAX_DOCUMENT_DEPTH {
            let mut parent = TocNode::new(
                format!("over-toc-{depth}"),
                "过深目录",
                TocTarget::unit("unit-1"),
            );
            parent.children.push(toc);
            toc = parent;
        }
        assert!(serde_json::to_vec(&toc).is_err());
    }

    #[test]
    fn deterministic_ids_and_asset_ids_are_reproducible() {
        assert_eq!(
            deterministic_id("chapter", b"same seed"),
            deterministic_id("chapter", b"same seed")
        );
        assert_ne!(
            deterministic_id("chapter", b"same seed"),
            deterministic_id("block", b"same seed")
        );

        let first = AssetRef::from_bytes(
            AssetRole::Attachment,
            "application/octet-stream",
            None,
            b"same bytes",
        );
        let second = AssetRef::from_bytes(
            AssetRole::Attachment,
            "application/octet-stream",
            None,
            b"same bytes",
        );
        assert_eq!(first.id, second.id);
        assert_eq!(first.content_hash, second.content_hash);
    }

    #[test]
    fn validation_reports_all_structural_and_reference_errors() {
        let mut book = sample_book();
        book.schema_version = 99;
        book.units.push(book.units[0].clone());
        book.toc.push(TocNode::new(
            "toc-1",
            "丢失目标",
            TocTarget::block("missing-unit", "missing-block"),
        ));
        if let Block::Heading { level, .. } = &mut book.units[0].document.blocks[0] {
            *level = 7;
        }
        if let Block::Video { asset_id, .. } = &mut book.units[0].document.blocks[6] {
            *asset_id = "missing-video".to_string();
        }
        book.units[0].source_locator = Some(SourceLocator::pdf_page(0));

        let error = book.validate().expect_err("invalid document must fail");
        let paths = error
            .issues()
            .iter()
            .map(|issue| issue.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"schema_version"));
        assert!(paths.contains(&"units[0].document.blocks[0].level"));
        assert!(paths.contains(&"units[0].document.blocks[6].asset_id"));
        assert!(paths.contains(&"units[0].source_locator.page"));
        assert!(paths.contains(&"units[1].id"));
        assert!(paths.contains(&"toc[1].id"));
        assert!(paths.contains(&"toc[1].target.unit_id"));
    }

    #[test]
    fn invalid_deep_models_stop_every_validation_and_lookup_traversal() {
        const DEEP_NESTING: usize = 4_096;

        fn dismantle_block(mut block: Block) {
            while let Block::BlockQuote { mut blocks, .. } = block {
                block = blocks.pop().expect("nested quote has one child");
            }
        }

        fn dismantle_inline(mut inline: Inline) {
            while let Inline::Strong { mut content } = inline {
                inline = content.pop().expect("nested strong has one child");
            }
        }

        let mut book = sample_book();
        book.toc.clear();
        let image_asset_id = book.assets[1].id.clone();
        let mut block = Block::Image {
            id: "deep-image".to_string(),
            asset_id: image_asset_id.clone(),
            alt: String::new(),
            title: None,
            caption: Vec::new(),
        };
        for depth in 0..DEEP_NESTING {
            block = Block::BlockQuote {
                id: format!("deep-quote-{depth}"),
                blocks: vec![block],
            };
        }
        book.units[0].document.blocks = vec![block];

        let error = book
            .validate()
            .expect_err("over-deep blocks must be rejected without exhausting the stack");
        assert!(error.to_string().contains("block nesting exceeds"));
        assert!(book.units[0].plain_text().trim().is_empty());
        assert_eq!(book.document_hash().len(), 64);
        assert!(serde_json::to_vec(&book).is_err());
        let locator = DocumentLocator::text(&book.id, &book.units[0].id, "deep-quote-4095", 0, 0);
        assert!(book.validate_locator(&locator).is_err());
        assert!(book.units[0].document.find_block("deep-image").is_none());
        assert!(book.units[0].document.referenced_asset_ids().is_empty());
        dismantle_block(book.units[0].document.blocks.pop().unwrap());

        let mut inline = Inline::Image {
            asset_id: image_asset_id,
            alt: String::new(),
            title: None,
        };
        for _ in 0..DEEP_NESTING {
            inline = Inline::Strong {
                content: vec![inline],
            };
        }
        book.units[0].document.blocks = vec![Block::Paragraph {
            id: "deep-inline-paragraph".to_string(),
            content: vec![inline],
        }];

        let error = book
            .validate()
            .expect_err("over-deep inlines must be rejected without exhausting the stack");
        assert!(error.to_string().contains("inline nesting exceeds"));
        assert!(book.units[0].plain_text().trim().is_empty());
        assert_eq!(book.units[0].content_hash().len(), 64);
        assert!(serde_json::to_vec(&book.units[0].document).is_err());
        assert!(book.units[0].document.referenced_asset_ids().is_empty());
        let Block::Paragraph { mut content, .. } = book.units[0].document.blocks.pop().unwrap()
        else {
            unreachable!();
        };
        dismantle_inline(content.pop().unwrap());
    }

    #[test]
    fn raw_html_validation_rejects_a_forged_plain_text_projection() {
        let document = BlockDocument::new(vec![Block::RawHtml {
            id: "raw-html".to_string(),
            source: "<em>可见正文</em>".to_string(),
            plain_text: "隐藏的提示词".to_string(),
        }]);
        let error = document
            .validate()
            .expect_err("raw HTML search text must be derived from its source");
        assert!(
            error
                .issues()
                .iter()
                .any(|issue| issue.path.ends_with(".plain_text"))
        );

        let deep_source = format!("{}正文{}", "<div>".repeat(4_096), "</div>".repeat(4_096));
        let document = BlockDocument::new(vec![Block::RawHtml {
            id: "deep-raw-html".to_string(),
            source: deep_source,
            plain_text: "正文".to_string(),
        }]);
        let error = document
            .validate()
            .expect_err("extreme raw HTML nesting must be rejected iteratively");
        assert!(error.to_string().contains("raw HTML nesting exceeds"));
    }

    #[test]
    fn plain_text_is_stable_and_contains_visible_content_only() {
        let book = sample_book();
        let text = book.plain_text();
        assert!(text.starts_with("示例图书\n\n墨页\n\n用于模型测试"));
        assert!(text.contains("你好，世界 详情"));
        assert!(text.contains("名称\n墨页"));
        assert!(text.contains("示意图\n图片说明"));
        assert!(text.contains("朗读"));
        assert!(text.contains("演示\n视频说明"));
        assert!(!text.contains("#details"));
        assert!(!text.contains("asset-"));
    }

    #[test]
    fn image_titles_are_searchable_without_duplicate_alt_or_caption_text() {
        let block = Block::Image {
            id: "image-block".to_string(),
            asset_id: "image-asset".to_string(),
            alt: "相同说明".to_string(),
            title: Some("人物关系图".to_string()),
            caption: vec![Inline::text("相同说明")],
        };
        assert_eq!(block.plain_text(), "人物关系图\n相同说明");

        let inline = Inline::Image {
            asset_id: "inline-image".to_string(),
            alt: "替代文字".to_string(),
            title: Some("行内图片标题".to_string()),
        };
        assert_eq!(inline.plain_text(), "行内图片标题\n替代文字");
    }

    #[test]
    fn locators_use_checked_utf8_byte_ranges() {
        let book = sample_book();
        let block = &book.units[0].document.blocks[1];
        let text = block.plain_text();
        let world_start = text.find("世界").expect("world text") as u64;
        let world_end = world_start + "世界".len() as u64;
        let locator = DocumentLocator::text(
            &book.id,
            &book.units[0].id,
            block.id(),
            world_start,
            world_end,
        );
        book.validate_locator(&locator)
            .expect("UTF-8 aligned range is valid");

        let split_character = DocumentLocator::text(
            &book.id,
            &book.units[0].id,
            block.id(),
            world_start + 1,
            world_end,
        );
        let error = book
            .validate_locator(&split_character)
            .expect_err("offset splitting a character must fail");
        assert_eq!(error.issues[0].path, "locator.text_range");

        let stale = DocumentLocator::block("another-book", "unit-1", block.id());
        assert!(book.validate_locator(&stale).is_err());
    }

    #[test]
    fn visual_regions_round_trip_and_old_locators_default_to_full_page() {
        let locator = DocumentLocator::unit("book-1", "unit-1")
            .with_source(SourceLocator::pdf_page(2))
            .with_region(NormalizedRect::new(25, 100, 975, 900));
        locator.validate().expect("normalized region is valid");

        let json = serde_json::to_string(&locator).expect("serialize visual locator");
        let decoded: DocumentLocator =
            serde_json::from_str(&json).expect("deserialize visual locator");
        assert_eq!(decoded, locator);

        let legacy: DocumentLocator = serde_json::from_str(
            r#"{"book_id":"book-1","unit_id":"unit-1","block_id":null,"text_range":null,"source":{"type":"pdf_page","page":2}}"#,
        )
        .expect("locator JSON written before visual regions remains readable");
        assert_eq!(legacy.region, None);
        legacy
            .validate()
            .expect("full-page legacy locator is valid");
    }

    #[test]
    fn visual_regions_require_bounded_positive_area() {
        NormalizedRect::new(0, 0, NORMALIZED_COORDINATE_MAX, NORMALIZED_COORDINATE_MAX)
            .validate()
            .expect("the complete normalized page is valid");

        for region in [
            NormalizedRect::new(10, 10, 10, 20),
            NormalizedRect::new(10, 20, 30, 20),
            NormalizedRect::new(30, 10, 20, 20),
            NormalizedRect::new(0, 0, NORMALIZED_COORDINATE_MAX + 1, 100),
            NormalizedRect::new(0, 0, 100, NORMALIZED_COORDINATE_MAX + 1),
        ] {
            assert!(region.validate().is_err(), "invalid region: {region:?}");
            assert!(
                DocumentLocator::unit("book-1", "unit-1")
                    .with_region(region)
                    .validate()
                    .is_err()
            );
        }
    }

    #[test]
    fn revisions_do_not_wrap() {
        assert_eq!(Revision::INITIAL.checked_next(), Some(Revision(1)));
        assert_eq!(Revision(u64::MAX).checked_next(), None);
    }

    #[test]
    fn source_locators_round_trip_and_use_one_based_numbers() {
        let locators = vec![
            SourceLocator::created(),
            SourceLocator::epub("EPUB/Text/start.xhtml"),
            SourceLocator::pdf_page(1),
            SourceLocator::office_rendered_page(1),
            SourceLocator::office_section(2),
            SourceLocator::slide(3),
            SourceLocator::worksheet("汇总", Some("A1:C20".to_string())),
            SourceLocator::kindle_section(4, Some("Text/part-4.html".to_string())),
        ];
        let json = serde_json::to_string(&locators).expect("serialize source locators");
        let decoded: Vec<SourceLocator> =
            serde_json::from_str(&json).expect("deserialize source locators");
        assert_eq!(decoded, locators);
        for locator in decoded {
            locator.validate().expect("sample locator is valid");
        }

        for locator in [
            SourceLocator::pdf_page(0),
            SourceLocator::office_rendered_page(0),
            SourceLocator::office_section(0),
            SourceLocator::slide(0),
            SourceLocator::kindle_section(0, None),
        ] {
            let error = locator.validate().expect_err("zero is not a source index");
            assert!(error.to_string().contains("one-based"));
        }
        assert!(SourceLocator::epub("\n").validate().is_err());
        assert!(SourceLocator::worksheet("", None).validate().is_err());
    }

    #[test]
    fn document_locator_optionally_carries_a_native_source_coordinate() {
        let locator = DocumentLocator::block("book-1", "unit-1", "block-1")
            .with_source(SourceLocator::worksheet("汇总", Some("C5:F12".to_string())));
        let json = serde_json::to_string(&locator).expect("serialize document locator");
        let decoded: DocumentLocator =
            serde_json::from_str(&json).expect("deserialize document locator");
        assert_eq!(decoded, locator);
        decoded.validate().expect("source coordinate is valid");

        let legacy: DocumentLocator = serde_json::from_str(
            r#"{"book_id":"book-1","unit_id":"unit-1","block_id":null,"text_range":null}"#,
        )
        .expect("source remains additive for locator JSON");
        assert_eq!(legacy.source, None);
    }

    #[test]
    fn block_hash_changes_only_when_block_document_changes() {
        let mut book = sample_book();
        let before = book.units[0].content_hash();
        book.title = "另一个书名".to_string();
        assert_eq!(before, book.units[0].content_hash());

        if let Block::Paragraph { content, .. } = &mut book.units[0].document.blocks[1] {
            content.push(Inline::text("新增"));
        }
        assert_ne!(before, book.units[0].content_hash());
    }
}
