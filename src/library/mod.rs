//! Library orchestration over SQLite metadata and content-addressed objects.
//!
//! The database never owns source, cover, or media bytes. Importers first
//! publish immutable objects through [`BlobStore`], then one SQLite transaction
//! makes the corresponding document graph visible.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    annotations::{Annotation, AnnotationDraft, AnnotationOverview, TextAnchor},
    db,
    document::{
        AssetRef as DocumentAsset, AssetRole, Block, BlockDocument, BookDocument, BookFormat,
        BookSource as DocumentSource, ContentUnit as DocumentUnit, ContentUnitKind,
        DocumentLocator, Revision, SourceLocator, TocNode, TocTarget, deterministic_id,
    },
    export::{BuiltinDocumentExporter, ExportFormat},
    formats::{FormatRegistry, ImportLimits, ImportedAsset, ImportedBook},
    runtime::IoRuntime,
    storage::{BlobKey, BlobPublicationLock, BlobStore, LocalBlobStore},
};
use anyhow::{Context as _, Result, bail, ensure};
use directories::ProjectDirs;

pub use crate::db::book_search::SearchHit;
pub use crate::db::books::BookRecord;
pub use crate::db::groups::BookGroup;

mod annotations;

mod content;

mod convert;

mod graph;

mod import;

mod maintenance;

mod media;

mod publish;

mod search;

mod store;

pub(crate) use content::*;

pub(crate) use convert::*;

pub(crate) use graph::*;

pub(crate) use maintenance::*;

pub(crate) use media::*;

pub(crate) use search::*;

#[cfg(test)]
mod tests;
pub(crate) const OBJECT_DIRECTORY: &str = "objects";

pub(crate) const MAX_GROUP_DEPTH: usize = 5;

pub(crate) const MAX_GROUP_NAME_LEN: usize = 40;

pub(crate) const MAX_SEARCH_QUERY_CHARS: usize = 200;

pub(crate) const MAX_SEARCH_RESULTS: usize = 200;

pub(crate) const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) const MAX_COVER_DIMENSION: u32 = 16_384;

pub(crate) const MAX_COVER_PIXELS: u64 = 50_000_000;

pub(crate) const MAX_AUDIO_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) const SEARCH_CHUNK_CHARACTERS: usize = 1_600;

pub(crate) const SEARCH_CHUNK_OVERLAP: usize = 160;

pub(crate) const MAX_SEARCH_CHUNKS_PER_DOCUMENT: usize = 100_000;

pub(crate) static NEXT_PROGRESS_INCARNATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_progress_incarnation() -> u64 {
    NEXT_PROGRESS_INCARNATION.fetch_add(1, Ordering::Relaxed)
}

impl BookRecord {
    pub fn progress_label(&self, unit_count: usize) -> String {
        if unit_count == 0 || self.last_spine == 0 {
            "尚未开始".to_string()
        } else {
            let current = (self.last_spine + 1).min(unit_count);
            format!("已读至 {current} / {unit_count}")
        }
    }
}

#[derive(Debug)]
pub enum ImportOutcome {
    Added(BookRecord),
    AlreadyExists(BookRecord),
}

/// One published canonical revision with the revision every chapter received.
/// An edit only advances the chapters whose body changed, so a caller that keeps
/// an in-memory projection of the saved document must take the per-chapter
/// revisions from here instead of assuming the document revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedDocument {
    pub record: BookRecord,
    /// One revision per content unit, in the document's own unit order.
    pub unit_revisions: Vec<Revision>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverDraft {
    pub(crate) mime: String,
    pub(crate) bytes: Arc<Vec<u8>>,
}

impl CoverDraft {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = path
            .metadata()
            .with_context(|| format!("无法读取封面图片信息：{}", path.display()))?;
        ensure!(metadata.is_file(), "封面图片不存在：{}", path.display());
        ensure!(
            metadata.len() <= MAX_COVER_BYTES,
            "封面图片超过 32 MiB 安全上限"
        );
        let file = fs::File::open(path)
            .with_context(|| format!("无法打开封面图片：{}", path.display()))?;
        let initial_capacity = usize::try_from(metadata.len().min(8 * 1024 * 1024)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(initial_capacity);
        file.take(MAX_COVER_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("无法读取封面图片：{}", path.display()))?;
        ensure!(
            bytes.len() as u64 <= MAX_COVER_BYTES,
            "封面图片超过 32 MiB 安全上限"
        );
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let cover = Self::from_arc(Arc::new(bytes))?;
        ensure!(
            replacement_cover_extension(&cover.mime).is_some(),
            "替换封面仅支持 JPEG、PNG 或 GIF"
        );
        Ok(cover)
    }

    pub fn from_arc(bytes: Arc<Vec<u8>>) -> Result<Self> {
        ensure!(
            bytes.len() as u64 <= MAX_COVER_BYTES,
            "封面图片超过 32 MiB 安全上限"
        );
        let reader = image::ImageReader::new(Cursor::new(bytes.as_slice()))
            .with_guessed_format()
            .context("无法识别封面图片格式")?;
        let format = reader.format().context("无法识别封面图片格式")?;
        let mime = match format {
            image::ImageFormat::Jpeg => "image/jpeg",
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Gif => "image/gif",
            image::ImageFormat::WebP => "image/webp",
            _ => bail!("封面仅支持 JPEG、PNG、GIF 或 WebP 图片"),
        };
        let (width, height) = reader.into_dimensions().context("封面图片已损坏")?;
        validate_cover_dimensions(width, height)?;
        let mut decoder = image::ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_COVER_DIMENSION);
        limits.max_image_height = Some(MAX_COVER_DIMENSION);
        decoder.limits(limits);
        decoder.decode().context("封面图片已损坏或无法解码")?;
        Ok(Self {
            mime: mime.to_string(),
            bytes,
        })
    }

    pub fn mime(&self) -> &str {
        &self.mime
    }

    pub fn bytes(&self) -> &Arc<Vec<u8>> {
        &self.bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

impl MediaKind {
    pub(crate) const fn role(self) -> AssetRole {
        match self {
            Self::Image => AssetRole::ContentImage,
            Self::Audio => AssetRole::Audio,
            Self::Video => AssetRole::Video,
        }
    }

    pub(crate) const fn mime_prefix(self) -> &'static str {
        match self {
            Self::Image => "image/",
            Self::Audio => "audio/",
            Self::Video => "video/",
        }
    }

    /// Maximum accepted payload size for a newly selected media object.
    ///
    /// UI callers use this before allocating the file buffer; `from_bytes`
    /// repeats the check so this is never only a presentation-layer limit.
    pub const fn max_bytes(self) -> u64 {
        match self {
            Self::Image => MAX_COVER_BYTES,
            Self::Audio => MAX_AUDIO_BYTES,
            Self::Video => MAX_VIDEO_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDraft {
    pub kind: MediaKind,
    pub media_type: String,
    pub original_file_name: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub(crate) bytes: Arc<Vec<u8>>,
}

impl MediaDraft {
    pub fn from_bytes(
        kind: MediaKind,
        media_type: impl Into<String>,
        original_file_name: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<Self> {
        let media_type = media_type.into().trim().to_ascii_lowercase();
        ensure!(!bytes.is_empty(), "媒体文件不能为空");
        ensure!(
            bytes.len() as u64 <= kind.max_bytes(),
            "媒体文件超过 {} 字节安全上限",
            kind.max_bytes()
        );
        ensure!(
            media_type.starts_with(kind.mime_prefix())
                && media_type
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && byte != b';'),
            "媒体 MIME 与资源类型不匹配"
        );
        if kind == MediaKind::Image {
            CoverDraft::from_arc(Arc::new(bytes.clone())).context("图片资源无效")?;
        }
        if let Some(file_name) = original_file_name.as_deref() {
            ensure!(
                !file_name.trim().is_empty()
                    && !file_name.contains(['/', '\\'])
                    && !file_name.chars().any(char::is_control),
                "媒体文件名无效"
            );
        }
        Ok(Self {
            kind,
            media_type,
            original_file_name,
            title: None,
            description: None,
            bytes: Arc::new(bytes),
        })
    }

    pub fn bytes(&self) -> &Arc<Vec<u8>> {
        &self.bytes
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = non_empty_option(title.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = non_empty_option(description.into());
        self
    }
}

#[derive(Clone)]
pub struct LibraryStore {
    pub(crate) data_dir: PathBuf,
    pub(crate) db_path: PathBuf,
    pub(crate) blob_store: Arc<LocalBlobStore>,
    pub(crate) blob_publication: BlobPublicationLock,
    pub(crate) io: IoRuntime,
    pub(crate) registry: Arc<FormatRegistry>,
    pub(crate) auto_run_background_jobs: Arc<AtomicBool>,
    pub(crate) books: Vec<BookRecord>,
    pub(crate) groups: Vec<BookGroup>,
    pub(crate) cover_cache: HashMap<String, Arc<Vec<u8>>>,
    /// Highest Reader action accepted for each book by the process-level
    /// projection. Actions are numbered before their SQLite jobs are spawned,
    /// so a delayed older window cannot replace a newer window's position.
    pub(crate) progress_write_sequences: HashMap<(String, u64), u64>,
    /// Process-local book identity that survives document revisions but not a
    /// delete/reimport cycle. Open Reader windows capture this token so an old
    /// window cannot write into a newly imported book that reused stable IDs.
    pub(crate) progress_incarnations: HashMap<String, u64>,
    #[cfg(test)]
    pub(crate) fail_next_cover_cache_hydration: bool,
}
