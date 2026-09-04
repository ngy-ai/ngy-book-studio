//! Format probing and import/export boundaries.
//!
//! Third-party parser representations are converted immediately into the
//! application's [`BookDocument`](crate::document::BookDocument). They never
//! cross this module or become database row types.

mod epub;
mod kindle;
mod office;
mod pdf;

use std::{
    cmp::Reverse,
    collections::HashMap,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use html5ever::{
    parse_document,
    serialize::{SerializeOpts, TraversalScope, serialize},
    tendril::TendrilSink as _,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};

use crate::document::{AssetRef, AssetRole, Block, BlockDocument, BookDocument, BookFormat};

pub use epub::EpubImporter;
pub use kindle::KindleImporter;
pub use office::OfficeImporter;
pub use pdf::PdfImporter;
pub(crate) use pdf::{MAX_PDF_PAGES, MAX_PDF_SOURCE_BYTES};

pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_UNITS: usize = 20_000;
pub const DEFAULT_MAX_UNIT_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_TEXT_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_ASSETS: usize = 20_000;
pub const DEFAULT_MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_ASSET_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportLimits {
    pub max_source_bytes: u64,
    pub max_units: usize,
    pub max_unit_text_bytes: usize,
    pub max_total_text_bytes: usize,
    /// Maximum retained immutable assets, including the original source.
    pub max_assets: usize,
    /// Maximum bytes for any one retained asset after extraction/decompression.
    pub max_asset_bytes: u64,
    /// Maximum aggregate bytes for retained assets after extraction/decompression.
    pub max_total_asset_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_units: DEFAULT_MAX_UNITS,
            max_unit_text_bytes: DEFAULT_MAX_UNIT_TEXT_BYTES,
            max_total_text_bytes: DEFAULT_MAX_TOTAL_TEXT_BYTES,
            max_assets: DEFAULT_MAX_ASSETS,
            max_asset_bytes: DEFAULT_MAX_ASSET_BYTES,
            max_total_asset_bytes: DEFAULT_MAX_TOTAL_ASSET_BYTES,
        }
    }
}

/// Tracks the immutable payloads retained by one import. Importers use this
/// before cloning or accumulating extracted bytes; [`ImportedBook::validate`]
/// repeats the same checks over the final graph so custom importers cannot
/// bypass the limits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AssetBudget {
    max_assets: usize,
    max_asset_bytes: u64,
    max_total_asset_bytes: u64,
    assets: usize,
    total_bytes: u64,
}

impl AssetBudget {
    pub(crate) fn new(limits: &ImportLimits) -> Self {
        Self {
            max_assets: limits.max_assets,
            max_asset_bytes: limits.max_asset_bytes,
            max_total_asset_bytes: limits.max_total_asset_bytes,
            assets: 0,
            total_bytes: 0,
        }
    }

    pub(crate) fn with_original(
        limits: &ImportLimits,
        byte_len: usize,
        label: &str,
    ) -> Result<Self> {
        let mut budget = Self::new(limits);
        budget.add_usize(byte_len, label)?;
        Ok(budget)
    }

    pub(crate) fn with_original_u64(
        limits: &ImportLimits,
        byte_len: u64,
        label: &str,
    ) -> Result<Self> {
        let mut budget = Self::new(limits);
        budget.add(byte_len, label)?;
        Ok(budget)
    }

    pub(crate) fn check_single_usize(&self, byte_len: usize, label: &str) -> Result<u64> {
        let byte_len = u64::try_from(byte_len).context("asset byte length does not fit u64")?;
        self.check_single(byte_len, label)?;
        Ok(byte_len)
    }

    pub(crate) fn check_single(&self, byte_len: u64, label: &str) -> Result<()> {
        if byte_len > self.max_asset_bytes {
            bail!(
                "{label} exceeds the {} byte single-asset safety limit",
                self.max_asset_bytes
            );
        }
        Ok(())
    }

    pub(crate) fn add_usize(&mut self, byte_len: usize, label: &str) -> Result<()> {
        let byte_len = self.check_single_usize(byte_len, label)?;
        self.add_checked(byte_len, label)
    }

    pub(crate) fn add(&mut self, byte_len: u64, label: &str) -> Result<()> {
        self.check_single(byte_len, label)?;
        self.add_checked(byte_len, label)
    }

    fn add_checked(&mut self, byte_len: u64, label: &str) -> Result<()> {
        let assets = self
            .assets
            .checked_add(1)
            .context("asset count overflowed")?;
        if assets > self.max_assets {
            bail!(
                "{label} exceeds the {} asset count safety limit",
                self.max_assets
            );
        }
        let total_bytes = self
            .total_bytes
            .checked_add(byte_len)
            .context("total imported asset size overflowed")?;
        if total_bytes > self.max_total_asset_bytes {
            bail!(
                "{label} exceeds the {} byte aggregate asset safety limit",
                self.max_total_asset_bytes
            );
        }
        self.assets = assets;
        self.total_bytes = total_bytes;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ImportSource {
    pub file_name: Option<String>,
    pub bytes: Arc<Vec<u8>>,
}

impl ImportSource {
    pub fn from_path(path: &Path, limits: &ImportLimits) -> Result<Self> {
        let metadata = path
            .metadata()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if !metadata.is_file() {
            bail!("import source is not a regular file: {}", path.display());
        }
        if metadata.len() > limits.max_source_bytes {
            bail!(
                "import source is larger than the {} byte safety limit",
                limits.max_source_bytes
            );
        }
        AssetBudget::with_original_u64(limits, metadata.len(), "import source")?;
        let file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let initial_capacity = usize::try_from(metadata.len().min(8 * 1024 * 1024)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(initial_capacity);
        file.take(limits.max_source_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.len() as u64 > limits.max_source_bytes {
            bail!(
                "import source is larger than the {} byte safety limit",
                limits.max_source_bytes
            );
        }
        AssetBudget::with_original(limits, bytes.len(), "import source")?;
        Ok(Self {
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            bytes: Arc::new(bytes),
        })
    }

    pub fn extension(&self) -> Option<String> {
        self.file_name
            .as_deref()
            .and_then(|name| Path::new(name).extension())
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
    }

    pub fn stem(&self) -> String {
        self.file_name
            .as_deref()
            .and_then(|name| Path::new(name).file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("Untitled")
            .to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeConfidence {
    NoMatch = 0,
    Extension = 20,
    Container = 60,
    Magic = 100,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub format: BookFormat,
    pub confidence: ProbeConfidence,
    pub detail: Option<String>,
}

impl ProbeResult {
    pub fn no_match(format: BookFormat) -> Self {
        Self {
            format,
            confidence: ProbeConfidence::NoMatch,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImporterCapabilities {
    pub importer: &'static str,
    pub formats: &'static [BookFormat],
    pub parser_version: &'static str,
}

#[derive(Clone, Debug)]
pub struct ImportedAsset {
    pub metadata: AssetRef,
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct ImportedBook {
    pub document: BookDocument,
    pub assets: Vec<ImportedAsset>,
}

impl ImportedBook {
    pub fn validate(&self, limits: &ImportLimits) -> Result<()> {
        if self.assets.len() > limits.max_assets || self.document.assets.len() > limits.max_assets {
            bail!(
                "document exceeds the {} asset count safety limit",
                limits.max_assets
            );
        }
        self.document.validate()?;
        if self.document.units.len() > limits.max_units {
            bail!("document has too many content units");
        }
        let mut total = 0_usize;
        for unit in &self.document.units {
            if unit.source.len() > limits.max_unit_text_bytes {
                bail!("content unit {} exceeds the text safety limit", unit.id);
            }
            total = total
                .checked_add(unit.source.len())
                .context("total imported text size overflowed")?;
            if total > limits.max_total_text_bytes {
                bail!("document exceeds the total text safety limit");
            }
        }
        if self.assets.len() != self.document.assets.len() {
            bail!("imported asset payloads do not match document asset metadata");
        }
        let mut asset_budget = AssetBudget::new(limits);
        for asset in &self.assets {
            asset_budget.add_usize(asset.bytes.len(), &format!("asset {}", asset.metadata.id))?;
            if asset.bytes.len() as u64 != asset.metadata.byte_len {
                bail!(
                    "asset {} byte length does not match metadata",
                    asset.metadata.id
                );
            }
            let digest = blake3::hash(asset.bytes.as_slice()).to_hex().to_string();
            if digest != asset.metadata.content_hash {
                bail!(
                    "asset {} content hash does not match metadata",
                    asset.metadata.id
                );
            }
        }
        Ok(())
    }

    pub fn original_asset(&self) -> Option<&ImportedAsset> {
        self.assets
            .iter()
            .find(|asset| asset.metadata.roles.contains(&AssetRole::OriginalSource))
    }
}

pub trait DocumentImporter: Send + Sync {
    fn capabilities(&self) -> ImporterCapabilities;
    fn probe(&self, source: &ImportSource) -> ProbeResult;
    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook>;
}

#[derive(Clone, Default)]
pub struct FormatRegistry {
    importers: Vec<Arc<dyn DocumentImporter>>,
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
        let (importer, _) = best_candidates[0];
        let imported = importer.import(source, limits)?;
        imported.validate(limits)?;
        Ok(imported)
    }

    pub fn import_path(&self, path: &Path, limits: &ImportLimits) -> Result<ImportedBook> {
        let source = ImportSource::from_path(path, limits)?;
        self.import(&source, limits)
            .with_context(|| format!("failed to import {}", path.display()))
    }
}

pub(crate) fn original_asset(
    source: &ImportSource,
    format: BookFormat,
    media_type: &str,
) -> ImportedAsset {
    asset_from_bytes(
        &format!("original-{format:?}"),
        vec![AssetRole::OriginalSource],
        media_type,
        source.file_name.clone(),
        source.bytes.clone(),
    )
}

pub(crate) fn asset_from_bytes(
    id_seed: &str,
    roles: Vec<AssetRole>,
    media_type: impl Into<String>,
    original_file_name: Option<String>,
    bytes: Arc<Vec<u8>>,
) -> ImportedAsset {
    let content_hash = blake3::hash(bytes.as_slice()).to_hex().to_string();
    let id =
        crate::document::deterministic_id("asset", format!("{id_seed}\0{content_hash}").as_bytes());
    ImportedAsset {
        metadata: AssetRef {
            id,
            roles,
            media_type: media_type.into(),
            original_file_name,
            byte_len: bytes.len() as u64,
            content_hash,
        },
        bytes,
    }
}

pub(crate) fn text_block_document(seed: &str, text: &str) -> BlockDocument {
    let blocks = text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .enumerate()
        .map(|(index, paragraph)| {
            Block::paragraph(
                crate::document::deterministic_id(
                    "block",
                    format!("{seed}\0{index}\0{paragraph}").as_bytes(),
                ),
                paragraph,
            )
        })
        .collect();
    BlockDocument::new(blocks)
}

/// Rewrites media URLs from an imported HTML chapter to opaque canonical
/// asset IDs before the chapter enters the editable AST. Only resources that
/// were declared by the source container are retained. The chapter base and
/// every candidate path are normalized with encoded dot-segments resolved, so
/// an authored `../` can reach a sibling manifest resource but cannot escape
/// the logical archive root.
pub(crate) fn rewrite_imported_html_assets(
    source: &str,
    chapter_href: &str,
    asset_ids_by_href: &HashMap<String, String>,
) -> Result<String> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    rewrite_media_attributes(&dom.document, chapter_href, asset_ids_by_href);

    let serializable = SerializableHandle::from(dom.document.clone());
    let mut bytes = Vec::new();
    serialize(
        &mut bytes,
        &serializable,
        SerializeOpts {
            traversal_scope: TraversalScope::ChildrenOnly(None),
            scripting_enabled: false,
            create_missing_parent: true,
        },
    )
    .context("failed to serialize imported HTML after asset rewriting")?;
    String::from_utf8(bytes).context("rewritten imported HTML is not UTF-8")
}

/// Produces the lookup key used by importers for manifest resources.
pub(crate) fn normalized_archive_href(href: &str) -> Option<String> {
    normalize_archive_path(href.split(['?', '#']).next().unwrap_or(href))
}

fn rewrite_media_attributes(
    node: &Handle,
    chapter_href: &str,
    asset_ids_by_href: &HashMap<String, String>,
) {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let tag = name.local.as_ref();
        attrs.borrow_mut().retain_mut(|attribute| {
            let attribute_name = attribute.name.local.as_ref();
            let is_asset_url = matches!(
                (tag, attribute_name),
                ("img" | "audio" | "video" | "source", "src") | ("video", "poster")
            );
            if !is_asset_url {
                return true;
            }
            let Some(asset_id) = resolve_imported_asset_href(
                chapter_href,
                attribute.value.as_ref(),
                asset_ids_by_href,
            ) else {
                return false;
            };
            attribute.value = format!("moye-asset:{asset_id}").into();
            true
        });
    }

    // Clone the handles so the RefCell borrow is not held during recursion.
    let children = node.children.borrow().iter().cloned().collect::<Vec<_>>();
    for child in children {
        rewrite_media_attributes(&child, chapter_href, asset_ids_by_href);
    }
}

fn resolve_imported_asset_href<'a>(
    chapter_href: &str,
    reference: &str,
    asset_ids_by_href: &'a HashMap<String, String>,
) -> Option<&'a String> {
    let reference = reference.trim();
    let reference_path = reference.split(['?', '#']).next().unwrap_or(reference);
    if reference_path.is_empty()
        || reference_path.starts_with("//")
        || reference_path.starts_with('\\')
        || reference_path
            .split(['/', '\\'])
            .next()
            .is_some_and(|head| head.contains(':'))
    {
        return None;
    }

    let joined = if reference_path.starts_with('/') {
        reference_path.to_string()
    } else {
        let mut base = normalized_archive_href(chapter_href)?;
        if let Some((directory, _)) = base.rsplit_once('/') {
            base = directory.to_string();
        } else {
            base.clear();
        }
        if base.is_empty() {
            reference_path.to_string()
        } else {
            format!("{base}/{reference_path}")
        }
    };
    let normalized = normalize_archive_path(&joined)?;
    asset_ids_by_href.get(&normalized)
}

fn normalize_archive_path(path: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    if decoded.contains('\\') || decoded.chars().any(char::is_control) {
        return None;
    }
    let mut segments = Vec::new();
    for segment in decoded.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            value => segments.push(value),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

pub(crate) fn safe_title(value: Option<&str>, fallback: &str) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

pub(crate) fn target_path_with_extension(path: &Path, extension: &str) -> PathBuf {
    let mut target = path.to_path_buf();
    target.set_extension(extension);
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NeverImporter;

    impl DocumentImporter for NeverImporter {
        fn capabilities(&self) -> ImporterCapabilities {
            ImporterCapabilities {
                importer: "never",
                formats: &[BookFormat::Epub],
                parser_version: "test",
            }
        }

        fn probe(&self, _: &ImportSource) -> ProbeResult {
            ProbeResult::no_match(BookFormat::Epub)
        }

        fn import(&self, _: &ImportSource, _: &ImportLimits) -> Result<ImportedBook> {
            unreachable!()
        }
    }

    #[test]
    fn registry_rejects_unknown_bytes() {
        let mut registry = FormatRegistry::default();
        registry.register(NeverImporter);
        let source = ImportSource {
            file_name: Some("unknown.bin".into()),
            bytes: Arc::new(vec![1, 2, 3]),
        };
        assert!(registry.import(&source, &ImportLimits::default()).is_err());
    }

    #[test]
    fn original_asset_is_content_addressed_and_byte_exact() {
        let source = ImportSource {
            file_name: Some("book.epub".into()),
            bytes: Arc::new(b"source bytes".to_vec()),
        };
        let asset = original_asset(&source, BookFormat::Epub, "application/epub+zip");
        assert_eq!(asset.bytes.as_slice(), b"source bytes");
        assert!(asset.metadata.roles.contains(&AssetRole::OriginalSource));
        assert_eq!(asset.metadata.byte_len, 12);
    }

    fn imported_book_with_asset_sizes(sizes: &[usize]) -> ImportedBook {
        let assets = sizes
            .iter()
            .enumerate()
            .map(|(index, size)| {
                asset_from_bytes(
                    &format!("budget-asset-{index}"),
                    vec![AssetRole::ContentImage],
                    "image/png",
                    Some(format!("asset-{index}.png")),
                    Arc::new(vec![u8::try_from(index).unwrap_or(u8::MAX); *size]),
                )
            })
            .collect::<Vec<_>>();
        let mut document = BookDocument::created("budget-book", "Budget book");
        document.assets = assets.iter().map(|asset| asset.metadata.clone()).collect();
        ImportedBook { document, assets }
    }

    fn asset_limits(max_assets: usize, max_asset_bytes: u64, total_bytes: u64) -> ImportLimits {
        ImportLimits {
            max_assets,
            max_asset_bytes,
            max_total_asset_bytes: total_bytes,
            ..ImportLimits::default()
        }
    }

    #[test]
    fn imported_asset_budget_accepts_exact_boundaries() {
        imported_book_with_asset_sizes(&[2, 3])
            .validate(&asset_limits(2, 3, 5))
            .expect("exact asset limits should be accepted");
    }

    #[test]
    fn imported_asset_budget_rejects_too_many_assets() {
        let error = imported_book_with_asset_sizes(&[1, 1, 1])
            .validate(&asset_limits(2, 8, 8))
            .expect_err("asset count above the limit must be rejected");
        assert!(error.to_string().contains("asset count safety limit"));
    }

    #[test]
    fn imported_asset_budget_rejects_one_oversized_asset() {
        let error = imported_book_with_asset_sizes(&[4])
            .validate(&asset_limits(1, 3, 8))
            .expect_err("single asset above the limit must be rejected");
        assert!(error.to_string().contains("single-asset safety limit"));
    }

    #[test]
    fn imported_asset_budget_rejects_aggregate_overflow() {
        let error = imported_book_with_asset_sizes(&[2, 3])
            .validate(&asset_limits(2, 3, 4))
            .expect_err("aggregate asset bytes above the limit must be rejected");
        assert!(error.to_string().contains("aggregate asset safety limit"));
    }

    #[test]
    fn import_path_applies_the_size_limit_before_and_during_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized.bin");
        fs::write(&path, b"123456789").unwrap();
        let limits = ImportLimits {
            max_source_bytes: 8,
            ..ImportLimits::default()
        };
        let error = ImportSource::from_path(&path, &limits).unwrap_err();
        assert!(error.to_string().contains("8 byte safety limit"));
    }

    #[test]
    fn import_path_applies_original_asset_limits_before_reading() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized-asset.bin");
        fs::write(&path, b"123456789").unwrap();
        let limits = ImportLimits {
            max_source_bytes: 16,
            max_asset_bytes: 8,
            max_total_asset_bytes: 16,
            ..ImportLimits::default()
        };
        let error = ImportSource::from_path(&path, &limits).unwrap_err();
        assert!(error.to_string().contains("single-asset safety limit"));
    }

    #[test]
    fn imported_html_media_urls_resolve_against_the_chapter_and_become_asset_ids() {
        let assets = HashMap::from([
            ("OPS/media/picture.png".to_string(), "image-id".to_string()),
            ("OPS/media/movie.mp4".to_string(), "video-id".to_string()),
            ("OPS/media/poster.png".to_string(), "poster-id".to_string()),
            ("OPS/media/sound.mp3".to_string(), "audio-id".to_string()),
        ]);
        let rewritten = rewrite_imported_html_assets(
            r#"<html><body>
                <img src="../media/picture.png?size=2#part" alt="cover">
                <audio><source src="%2e%2e/media/sound.mp3"></audio>
                <video src="../media/movie.mp4" poster="/OPS/media/poster.png"></video>
                <img src="../../../outside.png">
                <img src="https://example.invalid/tracker.png">
                <img src="../media/not-in-manifest.png">
            </body></html>"#,
            "OPS/text/chapter.xhtml",
            &assets,
        )
        .expect("rewrite imported media");

        assert!(rewritten.contains("moye-asset:image-id"));
        assert!(rewritten.contains("moye-asset:audio-id"));
        assert!(rewritten.contains("moye-asset:video-id"));
        assert!(rewritten.contains("moye-asset:poster-id"));
        assert!(!rewritten.contains("outside.png"));
        assert!(!rewritten.contains("tracker.png"));
        assert!(!rewritten.contains("not-in-manifest.png"));

        let parsed = crate::markup::parse_source_for_unit(
            crate::document::SourceKind::Html,
            &rewritten,
            "imported-unit",
        )
        .expect("parse rewritten HTML into the canonical AST");
        let mut referenced = parsed.document.referenced_asset_ids();
        referenced.sort_unstable();
        assert_eq!(
            referenced,
            vec!["audio-id", "image-id", "poster-id", "video-id"]
        );
    }

    #[test]
    fn archive_href_normalization_rejects_encoded_root_escape_and_backslashes() {
        assert_eq!(
            normalized_archive_href("/OPS/images/%70icture.png?x=1#fragment"),
            Some("OPS/images/picture.png".to_string())
        );
        assert_eq!(
            normalized_archive_href("OPS/text/../../cover.png"),
            Some("cover.png".into())
        );
        assert_eq!(normalized_archive_href("%2e%2e/secret.png"), None);
        assert_eq!(normalized_archive_href("OPS\\images\\picture.png"), None);
    }
}
