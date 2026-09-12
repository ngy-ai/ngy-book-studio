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

const OBJECT_DIRECTORY: &str = "objects";
const MAX_GROUP_DEPTH: usize = 5;
const MAX_GROUP_NAME_LEN: usize = 40;
const MAX_SEARCH_QUERY_CHARS: usize = 200;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_COVER_DIMENSION: u32 = 16_384;
const MAX_COVER_PIXELS: u64 = 50_000_000;
const MAX_AUDIO_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SEARCH_CHUNK_CHARACTERS: usize = 1_600;
const SEARCH_CHUNK_OVERLAP: usize = 160;
const MAX_SEARCH_CHUNKS_PER_DOCUMENT: usize = 100_000;
static NEXT_PROGRESS_INCARNATION: AtomicU64 = AtomicU64::new(1);

fn next_progress_incarnation() -> u64 {
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
    mime: String,
    bytes: Arc<Vec<u8>>,
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
    const fn role(self) -> AssetRole {
        match self {
            Self::Image => AssetRole::ContentImage,
            Self::Audio => AssetRole::Audio,
            Self::Video => AssetRole::Video,
        }
    }

    const fn mime_prefix(self) -> &'static str {
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
    bytes: Arc<Vec<u8>>,
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
    data_dir: PathBuf,
    db_path: PathBuf,
    blob_store: Arc<LocalBlobStore>,
    blob_publication: BlobPublicationLock,
    io: IoRuntime,
    registry: Arc<FormatRegistry>,
    auto_run_background_jobs: Arc<AtomicBool>,
    books: Vec<BookRecord>,
    groups: Vec<BookGroup>,
    cover_cache: HashMap<String, Arc<Vec<u8>>>,
    /// Highest Reader action accepted for each book by the process-level
    /// projection. Actions are numbered before their SQLite jobs are spawned,
    /// so a delayed older window cannot replace a newer window's position.
    progress_write_sequences: HashMap<(String, u64), u64>,
    /// Process-local book identity that survives document revisions but not a
    /// delete/reimport cycle. Open Reader windows capture this token so an old
    /// window cannot write into a newly imported book that reused stable IDs.
    progress_incarnations: HashMap<String, u64>,
    #[cfg(test)]
    fail_next_cover_cache_hydration: bool,
}

impl std::fmt::Debug for LibraryStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LibraryStore")
            .field("data_dir", &self.data_dir)
            .field("db_path", &self.db_path)
            .field("book_count", &self.books.len())
            .field("group_count", &self.groups.len())
            .finish()
    }
}

impl LibraryStore {
    pub fn default_data_dir() -> Result<PathBuf> {
        let project_dirs =
            ProjectDirs::from("dev", "moye", "Moye EPUB Reader").context("无法确定应用数据目录")?;
        Ok(project_dirs.data_local_dir().to_path_buf())
    }

    pub fn load() -> Result<Self> {
        Self::load_from(Self::default_data_dir()?)
    }

    pub fn load_from(data_dir: PathBuf) -> Result<Self> {
        Self::load_from_with_runtime(data_dir, IoRuntime::default())
    }

    /// Opens a library using the process service runtime. AppServices uses
    /// this constructor so storage, parsing, indexing and AI do not each own a
    /// redundant Tokio worker pool.
    pub fn load_from_with_runtime(data_dir: PathBuf, io: IoRuntime) -> Result<Self> {
        let data_dir = absolute_path(data_dir)?;
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("无法创建数据目录：{}", data_dir.display()))?;
        let db_path = data_dir.join(db::DATABASE_FILE);
        let opened = db::open_or_recreate_with_state(&db_path)?;
        let object_root = data_dir.join(OBJECT_DIRECTORY);
        if opened.state != db::DatabaseOpenState::Current && object_root.exists() {
            ensure!(
                object_root.parent() == Some(data_dir.as_path()),
                "拒绝清理数据目录之外的对象存储"
            );
            if let Err(error) = fs::remove_dir_all(&object_root) {
                let cleanup_error = anyhow::Error::new(error)
                    .context(format!("无法清理旧对象目录：{}", object_root.display()));
                if let Err(rollback_error) =
                    db::discard_opened_database(opened.connection, &db_path)
                {
                    return Err(rollback_error).context(format!(
                        "对象目录重置失败（{cleanup_error:#}），且无法移除本次创建的空数据库"
                    ));
                }
                return Err(cleanup_error);
            }
        }
        let blob_store = Arc::new(LocalBlobStore::new(&object_root)?);
        let blob_publication = BlobPublicationLock::for_store(&blob_store)?;
        let startup_publication_guard = io.block_on(blob_publication.acquire());
        run_startup_blob_gc(&opened.connection, &blob_store, &io)?;
        drop(startup_publication_guard);
        let books = db::books::list(&opened.connection)?;
        let groups = db::groups::list(&opened.connection)?;
        drop(opened.connection);

        let progress_incarnations = books
            .iter()
            .map(|book| (book.id.clone(), next_progress_incarnation()))
            .collect();
        let mut library = Self {
            data_dir,
            db_path,
            blob_store,
            blob_publication,
            io,
            registry: Arc::new(FormatRegistry::with_builtin_importers()),
            auto_run_background_jobs: Arc::new(AtomicBool::new(
                crate::services::DEFAULT_AUTO_RUN_BACKGROUND_JOBS,
            )),
            books,
            groups,
            cover_cache: HashMap::new(),
            progress_write_sequences: HashMap::new(),
            progress_incarnations,
            #[cfg(test)]
            fail_next_cover_cache_hydration: false,
        };
        let cover_ids = library
            .books
            .iter()
            .filter(|book| book.cover_object_key.is_some())
            .map(|book| book.id.clone())
            .collect::<Vec<_>>();
        for book_id in cover_ids {
            if let Err(error) = library.reload_cover(&book_id) {
                tracing::warn!(%book_id, %error, "忽略无法读取的封面对象");
            }
        }
        Ok(library)
    }

    pub fn books(&self) -> &[BookRecord] {
        &self.books
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn blob_store(&self) -> Arc<LocalBlobStore> {
        self.blob_store.clone()
    }

    /// Shared process-level switch consulted when a document revision commits.
    /// AppServices owns the user-facing persisted setting and updates this exact
    /// flag so cloned LibraryStore projections observe the latest value.
    pub(crate) fn background_job_auto_run_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.auto_run_background_jobs)
    }

    pub(crate) fn blob_publication_lock(&self) -> BlobPublicationLock {
        self.blob_publication.clone()
    }

    pub fn io_runtime(&self) -> IoRuntime {
        self.io.clone()
    }

    pub fn format_registry(&self) -> Arc<FormatRegistry> {
        self.registry.clone()
    }

    pub fn groups(&self) -> &[BookGroup] {
        &self.groups
    }

    pub fn book_record(&self, book_id: &str) -> Result<BookRecord> {
        let conn = db::open_conn(&self.db_path)?;
        db::books::get(&conn, book_id)?.with_context(|| format!("图书不存在：{book_id}"))
    }

    pub fn progress_incarnation(&self, book_id: &str) -> Option<u64> {
        self.progress_incarnations.get(book_id).copied()
    }

    pub fn refresh_book(&mut self, book_id: &str) -> Result<BookRecord> {
        let record = self.book_record(book_id)?;
        if let Some(position) = self.books.iter().position(|book| book.id == book_id) {
            self.books[position] = record.clone();
        } else {
            self.books.insert(0, record.clone());
            self.progress_incarnations
                .insert(book_id.to_string(), next_progress_incarnation());
        }
        self.reload_cover(book_id)?;
        Ok(record)
    }

    /// Applies a book/cover projection already loaded by a background service
    /// operation. This method performs no filesystem or SQLite work and is
    /// therefore safe to use when a GPUI completion callback updates its local
    /// view model. A delayed completion cannot replace a newer revision.
    pub fn apply_cached_book_update(
        &mut self,
        record: BookRecord,
        cover: Option<Arc<Vec<u8>>>,
    ) -> bool {
        let position = self.books.iter().position(|book| book.id == record.id);
        if position.is_some_and(|position| {
            let current = &self.books[position];
            current.revision > record.revision
                || (current.revision == record.revision && current.updated_at > record.updated_at)
        }) {
            return false;
        }
        let book_id = record.id.clone();
        if let Some(position) = position {
            self.books[position] = record;
        } else {
            self.books.insert(0, record);
            self.progress_incarnations
                .insert(book_id.clone(), next_progress_incarnation());
        }
        match cover {
            Some(bytes) => {
                self.cover_cache.insert(book_id, bytes);
            }
            None => {
                self.cover_cache.remove(&book_id);
            }
        }
        true
    }

    /// Merges a service-owned projection into this window-local clone without
    /// allowing an older full snapshot to roll back a newer editor result.
    /// Group membership and removals come from the authoritative snapshot;
    /// records already advanced to a later revision remain intact.
    pub fn merge_cached_projection(&mut self, snapshot: LibraryStore) {
        debug_assert_eq!(self.db_path, snapshot.db_path);

        let mut local_books = std::mem::take(&mut self.books)
            .into_iter()
            .map(|book| (book.id.clone(), book))
            .collect::<HashMap<_, _>>();
        let local_covers = std::mem::take(&mut self.cover_cache);
        let mut next_covers = HashMap::new();
        let mut next_books = Vec::with_capacity(snapshot.books.len());

        for incoming in snapshot.books {
            let book_id = incoming.id.clone();
            let local = local_books.remove(&book_id);
            let keep_local = local.as_ref().is_some_and(|current| {
                current.revision > incoming.revision
                    || (current.revision == incoming.revision
                        && current.updated_at > incoming.updated_at)
            });
            if keep_local {
                if let Some(cover) = local_covers.get(&book_id) {
                    next_covers.insert(book_id, Arc::clone(cover));
                }
                let mut local = local.expect("local book was checked above");
                // These fields are independent of the document revision. A
                // stale editor result may legitimately be newer in content
                // while the service snapshot is newer in shelf/progress state.
                local.group_id = incoming.group_id.clone();
                local.last_spine = incoming.last_spine;
                next_books.push(local);
            } else {
                if let Some(cover) = snapshot.cover_cache.get(&book_id) {
                    next_covers.insert(book_id, Arc::clone(cover));
                }
                next_books.push(incoming);
            }
        }

        self.books = next_books;
        self.groups = snapshot.groups;
        self.cover_cache = next_covers;
        self.progress_incarnations = snapshot.progress_incarnations;
        self.progress_write_sequences = snapshot.progress_write_sequences;
    }

    pub fn group(&self, id: &str) -> Option<&BookGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    pub fn child_groups(&self, parent_id: Option<&str>) -> Vec<&BookGroup> {
        self.groups
            .iter()
            .filter(|group| group.parent_id.as_deref() == parent_id)
            .collect()
    }

    pub fn group_path(&self, id: &str) -> Vec<BookGroup> {
        let mut path = Vec::new();
        let mut current = self.group(id).cloned();
        while let Some(group) = current {
            current = group
                .parent_id
                .as_deref()
                .and_then(|parent_id| self.group(parent_id).cloned());
            path.push(group);
            if path.len() > self.groups.len() {
                break;
            }
        }
        path.reverse();
        path
    }

    pub fn group_subtree_ids(&self, id: &str) -> Vec<String> {
        let mut ids = vec![id.to_string()];
        let mut pending = vec![id.to_string()];
        while let Some(current) = pending.pop() {
            for child in self.child_groups(Some(&current)) {
                ids.push(child.id.clone());
                pending.push(child.id.clone());
            }
        }
        ids
    }

    pub fn group_book_count(&self, group_id: &str) -> usize {
        self.books
            .iter()
            .filter(|book| book.group_id.as_deref() == Some(group_id))
            .count()
    }

    pub fn group_subtree_book_count(&self, group_id: &str) -> usize {
        let subtree = self.group_subtree_ids(group_id);
        let ids = subtree.iter().map(String::as_str).collect::<HashSet<_>>();
        self.books
            .iter()
            .filter(|book| book.group_id.as_deref().is_some_and(|id| ids.contains(id)))
            .count()
    }

    pub fn ungrouped_book_count(&self) -> usize {
        self.books
            .iter()
            .filter(|book| book.group_id.is_none())
            .count()
    }

    pub fn import(&mut self, source_path: &Path) -> Result<ImportOutcome> {
        let imported = self
            .registry
            .import_path(source_path, &ImportLimits::default())?;
        if let Some(existing) = self
            .books
            .iter()
            .find(|book| book.id == imported.document.id)
        {
            return Ok(ImportOutcome::AlreadyExists(existing.clone()));
        }
        let original = imported
            .original_asset()
            .context("导入器没有保留字节一致的原文件")?;
        let source_name = imported_source_name(&imported.document);
        let media_type = original.metadata.media_type.clone();
        let source_bytes = original.bytes.clone();
        let record = self.persist_document(
            imported,
            source_bytes.as_slice(),
            &media_type,
            source_name,
            "original",
            None,
        )?;
        Ok(self.finish_committed_import(record))
    }

    /// Updates the in-memory projection after an import transaction has
    /// committed. Cover hydration is deliberately best-effort here: failing
    /// to read a derived UI cache must not turn a durable import into an
    /// apparent failure. In particular, callers such as
    /// `AppServices::spawn_library` must still observe `Ok` so they wake the
    /// persisted derivative-index queue.
    fn finish_committed_import(&mut self, record: BookRecord) -> ImportOutcome {
        self.progress_incarnations
            .insert(record.id.clone(), next_progress_incarnation());
        self.books.insert(0, record.clone());
        if let Err(error) = self.reload_cover(&record.id) {
            tracing::warn!(
                book_id = %record.id,
                error = %format!("{error:#}"),
                "图书导入已提交，但封面缓存加载失败；继续以无封面状态完成导入"
            );
        }
        ImportOutcome::Added(record)
    }

    /// Creates an editable book with one HTML chapter. A generated EPUB is
    /// kept as its current view/export projection; it is not treated as an
    /// imported original.
    pub fn create_book(&mut self, title: &str, author: &str) -> Result<BookRecord> {
        let title = title.trim();
        ensure!(!title.is_empty(), "图书名称不能为空");
        let now = now_nanos();
        let book_id = deterministic_id("book", format!("created\0{title}\0{now}").as_bytes());
        let unit_id = deterministic_id("unit", format!("{book_id}\0chapter-1").as_bytes());
        let document = BlockDocument::new(vec![
            Block::heading(
                deterministic_id("block", format!("{unit_id}\0heading").as_bytes()),
                1,
                "第一章",
            ),
            Block::paragraph(
                deterministic_id("block", format!("{unit_id}\0paragraph").as_bytes()),
                "开始写作……",
            ),
        ]);
        let source = crate::markup::serialize_source(&document)?;
        let unit = DocumentUnit::new(
            unit_id.clone(),
            ContentUnitKind::Chapter,
            "第一章",
            source,
            document,
        )
        .with_source_locator(SourceLocator::created());
        let mut book = BookDocument::created(book_id, title);
        if !author.trim().is_empty() {
            book.authors.push(author.trim().to_string());
        }
        book.units.push(unit);
        book.toc.push(TocNode::new(
            deterministic_id("toc", format!("{}\0chapter-1", book.id).as_bytes()),
            "第一章",
            TocTarget::unit(unit_id),
        ));
        book.validate()?;
        let epub = generated_epub(&book)?;
        let imported = ImportedBook {
            document: book,
            assets: Vec::new(),
        };
        let record = self.persist_document(
            imported,
            &epub,
            "application/epub+zip",
            None,
            "created",
            None,
        )?;
        self.progress_incarnations
            .insert(record.id.clone(), next_progress_incarnation());
        self.books.insert(0, record.clone());
        Ok(record)
    }

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

    /// Checks against the exact EPUB chapter served to the reading WebView,
    /// including original EPUB markup and headings added by normalized export.
    /// Call on a background worker because resource parsing performs I/O.
    pub fn validate_annotation_anchor(
        &self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        ensure!(anchor.start < anchor.end, "请选择非空文本后添加笔记");
        ensure!(
            anchor.quote.len() <= crate::annotations::MAX_ANNOTATION_QUOTE_BYTES,
            "所选文本过长，请缩小笔记范围"
        );
        ensure!(
            !crate::annotations::compact_text(&anchor.quote).is_empty(),
            "请选择非空文本后添加笔记"
        );
        let document = self.document(book_id)?;
        ensure!(
            document.revision.get() == document_revision,
            "图书已更新，请重新打开章节后添加笔记"
        );
        let (ordinal, unit) = document
            .units
            .iter()
            .enumerate()
            .find(|(_, unit)| unit.id == content_unit_id)
            .context("笔记章节不属于当前图书")?;
        ensure!(
            unit.revision.get() == unit_revision,
            "章节已更新，请重新打开后添加笔记"
        );
        let opened = crate::reader::OpenedBook::open_bytes(self.reader_epub_bytes(book_id)?)?;
        ensure!(
            opened.spine.len() == document.units.len(),
            "阅读章节与图书结构不一致，请重新打开图书"
        );
        let href = &opened.spine[ordinal].href;
        let response = crate::reader::load_resource(&opened.epub, href)?;
        let html = std::str::from_utf8(&response.bytes).context("无法解码笔记所在章节")?;
        let text = crate::annotations::reader_body_text(html);
        crate::annotations::validate_anchor(&text, anchor)
    }

    /// PDF notes are anchored to the pinned PDF.js text layer of one page.
    ///
    /// No host extractor reproduces that projection byte-for-byte, so the
    /// quote cannot be compared against the canonical page text the way EPUB
    /// chapters are: doing so would reject ordinary selections on any page
    /// whose PDF.js layout differs from the importer's. Offsets stay stable
    /// because the imported original and the bundled PDF.js build are both
    /// immutable, so verify every ownership, scope and revision rule plus the
    /// anchor's own bounds instead.
    pub fn validate_pdf_annotation_anchor(
        &self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        ensure!(anchor.start < anchor.end, "请选择非空文本后添加笔记");
        ensure!(
            anchor.quote.len() <= crate::annotations::MAX_ANNOTATION_QUOTE_BYTES,
            "所选文本过长，请缩小笔记范围"
        );
        let compacted = crate::annotations::compact_text(&anchor.quote);
        ensure!(!compacted.is_empty(), "请选择非空文本后添加笔记");
        // The page range and its quote are two views of one selection: the
        // reader derives both from the same compacted text layer, so their
        // lengths must agree even though the page text itself is not readable
        // here. A range that cannot describe this quote is never stored.
        ensure!(
            u64::from(anchor.end - anchor.start) == compacted.encode_utf16().count() as u64,
            "所选文本与页面位置不一致，请重新选择后添加笔记"
        );
        let document = self.document(book_id)?;
        ensure!(
            document.revision.get() == document_revision,
            "图书已更新，请重新打开页面后添加笔记"
        );
        let unit = document
            .units
            .iter()
            .find(|unit| unit.id == content_unit_id)
            .context("笔记页面不属于当前图书")?;
        ensure!(unit.kind == ContentUnitKind::Page, "笔记位置不是 PDF 页面");
        ensure!(
            unit.revision.get() == unit_revision,
            "页面已更新，请重新打开后添加笔记"
        );
        Ok(())
    }

    /// Persists a PDF page note. Shares the single notes table, the exclusive
    /// mark replacement and every revision rule with EPUB notes; only the
    /// canonical-text comparison is replaced by
    /// [`Self::validate_pdf_annotation_anchor`].
    pub fn create_pdf_annotation(
        &mut self,
        book_id: &str,
        draft: &AnnotationDraft,
    ) -> Result<Annotation> {
        crate::annotations::validate_draft(draft)?;
        self.validate_pdf_annotation_anchor(
            book_id,
            &draft.content_unit_id,
            draft.document_revision,
            draft.unit_revision,
            &draft.anchor,
        )?;
        let now = now_secs();
        let note = Annotation {
            id: deterministic_id("annotation", format!("{book_id}\0{}", now_nanos())),
            book_id: book_id.to_string(),
            content_unit_id: draft.content_unit_id.clone(),
            document_revision: draft.document_revision,
            unit_revision: draft.unit_revision,
            anchor: draft.anchor.clone(),
            kind: draft.kind,
            comment: draft.comment.clone(),
            created_at: now,
            updated_at: now,
            stale: false,
        };
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::insert_annotation(&mut conn, &note)
    }

    /// Removes only the mark on this exact PDF selection. Thoughts and other
    /// occurrences of the same quote are retained; an unmarked range succeeds.
    pub fn delete_pdf_annotation_marks(
        &mut self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        self.validate_pdf_annotation_anchor(
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )?;
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::delete_annotation_marks(
            &mut conn,
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )
    }

    /// All marks and both kinds of thought share one persisted notes table.
    /// A mark replaces the style of the same exact selection, retaining its
    /// identity; repeating the existing style leaves the note unchanged.
    pub fn create_annotation(
        &mut self,
        book_id: &str,
        draft: &AnnotationDraft,
    ) -> Result<Annotation> {
        crate::annotations::validate_draft(draft)?;
        self.validate_annotation_anchor(
            book_id,
            &draft.content_unit_id,
            draft.document_revision,
            draft.unit_revision,
            &draft.anchor,
        )?;
        let now = now_secs();
        let note = Annotation {
            id: deterministic_id("annotation", format!("{book_id}\0{}", now_nanos())),
            book_id: book_id.to_string(),
            content_unit_id: draft.content_unit_id.clone(),
            document_revision: draft.document_revision,
            unit_revision: draft.unit_revision,
            anchor: draft.anchor.clone(),
            kind: draft.kind,
            comment: draft.comment.clone(),
            created_at: now,
            updated_at: now,
            stale: false,
        };
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::insert_annotation(&mut conn, &note)
    }

    pub fn list_annotations(
        &self,
        book_id: &str,
        content_unit_id: Option<&str>,
    ) -> Result<Vec<Annotation>> {
        let conn = db::open_conn(&self.db_path)?;
        let book = db::books::get(&conn, book_id)?.context("图书不存在")?;
        let source = db::book_sources::get_revision(&conn, book_id, book.revision)?
            .context("当前图书来源不存在")?;
        let units = db::content_units::list_for_source(&conn, &source.id)?
            .into_iter()
            .map(|unit| (unit.id, unit.revision))
            .collect::<HashMap<_, _>>();
        let mut notes = db::annotations::list(&conn, book_id, content_unit_id)?;
        for note in &mut notes {
            note.stale = note.document_revision != book.revision
                || units.get(&note.content_unit_id) != Some(&note.unit_revision);
        }
        Ok(notes)
    }

    /// Reads notes for one book, or the entire library, newest update first.
    /// Queries current catalog metadata directly without loading chapter bodies
    /// or relying on this store clone's cached book projection.
    pub fn annotation_overview(&self, book_id: Option<&str>) -> Result<Vec<AnnotationOverview>> {
        let conn = db::open_conn(&self.db_path)?;
        if let Some(book_id) = book_id {
            db::books::get(&conn, book_id)?.context("图书不存在")?;
        }
        db::annotations::overview(&conn, book_id)
    }

    /// Only the body of a human thought is editable. Its author kind, quote,
    /// and original location cannot be changed through this operation.
    pub fn update_human_comment(
        &mut self,
        book_id: &str,
        id: &str,
        comment: &str,
    ) -> Result<Annotation> {
        crate::annotations::validate_comment(
            comment,
            crate::annotations::AnnotationKind::HumanComment,
        )?;
        let conn = db::open_conn(&self.db_path)?;
        let mut existing = self
            .list_annotations(book_id, None)?
            .into_iter()
            .find(|note| note.id == id)
            .context("笔记不存在或不属于当前图书")?;
        ensure!(
            existing.kind == crate::annotations::AnnotationKind::HumanComment,
            "只能编辑人工想法，AI 想法保留原始来源"
        );
        let updated_at = now_secs().max(existing.updated_at);
        ensure!(
            db::annotations::update_human_comment(&conn, book_id, id, comment, updated_at)? == 1,
            "笔记已被删除，无法保存想法"
        );
        existing.comment = Some(comment.to_string());
        existing.updated_at = updated_at;
        Ok(existing)
    }

    pub fn delete_annotation(&mut self, book_id: &str, id: &str) -> Result<()> {
        let conn = db::open_conn(&self.db_path)?;
        ensure!(
            db::annotations::delete(&conn, book_id, id)? == 1,
            "笔记不存在或不属于当前图书"
        );
        Ok(())
    }

    /// Removes only the mark on this exact selection. Thoughts and other
    /// occurrences of the same quote are retained; an unmarked range succeeds.
    pub fn delete_annotation_marks(
        &mut self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        self.validate_annotation_anchor(
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )?;
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::delete_annotation_marks(
            &mut conn,
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )
    }

    pub fn cover_bytes(&self, book_id: &str) -> Result<Option<Arc<Vec<u8>>>> {
        if let Some(bytes) = self.cover_cache.get(book_id) {
            return Ok(Some(bytes.clone()));
        }
        let record = self.book_record(book_id)?;
        record
            .cover_object_key
            .as_deref()
            .map(|key| self.read_blob(key).map(Arc::new))
            .transpose()
    }

    /// Returns only the startup/import hydrated cache and never performs I/O.
    /// This is safe to call from a GPUI render callback.
    pub fn cover_bytes_cached(&self, book_id: &str) -> Option<Arc<Vec<u8>>> {
        self.cover_cache.get(book_id).cloned()
    }

    /// Resolves an asset only after proving that it belongs to the requested
    /// book. This is the same authorization boundary used by exports and the
    /// media protocol.
    pub fn asset_bytes(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
        let conn = db::open_conn(&self.db_path)?;
        let asset = db::assets::get(&conn, asset_id)?.context("资源不存在")?;
        ensure!(asset.book_id == book_id, "资源不属于指定图书");
        drop(conn);
        self.read_blob(&asset.object_key)
    }

    pub fn asset_range(
        &self,
        book_id: &str,
        asset_id: &str,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>> {
        let conn = db::open_conn(&self.db_path)?;
        let asset = db::assets::get(&conn, asset_id)?.context("资源不存在")?;
        ensure!(asset.book_id == book_id, "资源不属于指定图书");
        drop(conn);
        let key = BlobKey::parse(&asset.object_key)?;
        self.io.block_on(self.blob_store.get_range(&key, range))
    }

    pub fn asset_metadata(&self, book_id: &str, asset_id: &str) -> Result<(String, u64)> {
        let conn = db::open_conn(&self.db_path)?;
        let asset = db::assets::get(&conn, asset_id)?.context("资源不存在")?;
        ensure!(asset.book_id == book_id, "资源不属于指定图书");
        Ok((asset.media_type, asset.byte_len))
    }

    /// Loads one exact persisted source revision for a native visual renderer.
    /// The source ID is opaque outside the storage layer and is checked along
    /// with book ownership and revision before any bytes are returned.
    pub(crate) fn visual_source_bytes(
        &self,
        book_id: &str,
        source_id: &str,
        revision: Revision,
    ) -> Result<(String, String, String, Vec<u8>)> {
        let conn = db::open_conn(&self.db_path)?;
        let source = db::book_sources::get(&conn, source_id)?.context("图书来源不存在")?;
        ensure!(source.book_id == book_id, "图书来源不属于指定图书");
        ensure!(
            source.revision == revision.get(),
            "图书来源 revision 与视觉任务不一致"
        );
        let blob = db::blobs::get(&conn, &source.object_key)?.context("图书来源对象不存在")?;
        let format = source.format;
        let source_kind = source.source_kind;
        let media_type = blob.media_type;
        let expected_len = blob.byte_len;
        drop(conn);
        let bytes = self.read_blob(&source.object_key)?;
        ensure!(
            bytes.len() as u64 == expected_len,
            "图书来源对象长度与数据库元数据不一致"
        );
        Ok((format, source_kind, media_type, bytes))
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

    pub fn search_library(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        search_index(&self.db_path, None, query, limit)
    }

    pub fn search_book(&self, book_id: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        ensure!(
            self.books.iter().any(|book| book.id == book_id),
            "图书不存在"
        );
        search_index(&self.db_path, Some(book_id), query, limit)
    }

    /// Saves a validated canonical document as a normalized EPUB revision.
    /// A stale caller is rejected by the revision check; imported originals
    /// remain referenced as immutable assets.
    pub fn apply_document(&mut self, document: BookDocument) -> Result<BookRecord> {
        self.apply_document_with_assets(document, HashMap::new())
    }

    /// Publishes a canonical revision together with newly inserted immutable
    /// assets. Existing asset bytes are resolved only after a book-ownership
    /// check; supplied bytes must exactly match their document metadata.
    pub fn apply_document_with_assets(
        &mut self,
        document: BookDocument,
        new_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    ) -> Result<BookRecord> {
        self.publish_document_with_assets(document, new_asset_bytes)
            .map(|published| published.record)
    }

    /// Publishes a canonical revision and reports the revision every chapter
    /// received, so a caller that keeps an in-memory projection of the saved
    /// document does not have to guess: a chapter whose body did not change
    /// keeps its own revision and therefore its notes and whole-book译文.
    pub fn publish_document_with_assets(
        &mut self,
        mut document: BookDocument,
        new_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    ) -> Result<PublishedDocument> {
        let position = self
            .books
            .iter()
            .position(|book| book.id == document.id)
            .context("图书不存在")?;
        let current = self.book_record(&document.id)?;
        ensure!(
            document.revision.get() == current.revision,
            "图书已被其它窗口修改，请重新打开后再保存"
        );
        document.validate()?;
        for (asset_id, bytes) in &new_asset_bytes {
            let metadata = document
                .find_asset(asset_id)
                .with_context(|| format!("新资源不在文档模型中：{asset_id}"))?;
            ensure!(metadata.byte_len == bytes.len() as u64, "新资源长度不匹配");
            ensure!(
                metadata.content_hash == blake3::hash(bytes).to_hex().to_string(),
                "新资源内容哈希不匹配"
            );
        }
        let resolver = |asset_id: &str| match new_asset_bytes.get(asset_id) {
            Some(bytes) => Ok(bytes.as_ref().clone()),
            None => self.asset_bytes(&document.id, asset_id),
        };
        let new_bytes =
            BuiltinDocumentExporter.export_bytes(&document, ExportFormat::Epub, &resolver)?;
        let published = self.published_unit_content(&document.id, current.revision)?;
        document.revision = Revision::new(current.revision + 1);
        for unit in &mut document.units {
            // Only a chapter whose body really changed takes the new revision.
            // Every other chapter keeps its own revision, so its notes and its
            // whole-book translation stay valid instead of being translated
            // again only because another chapter was edited.
            unit.revision = match published.get(&unit.id) {
                Some((revision, hash)) if hash == &unit.content_hash() => Revision::new(*revision),
                _ => document.revision,
            };
        }
        let assets = document
            .assets
            .iter()
            .cloned()
            .map(|metadata| {
                let bytes = resolver(&metadata.id)?;
                Ok(ImportedAsset {
                    metadata,
                    bytes: Arc::new(bytes),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let book_id = document.id.clone();
        let title = document.title.clone();
        let unit_revisions = document
            .units
            .iter()
            .map(|unit| unit.revision)
            .collect::<Vec<_>>();
        let imported = ImportedBook { document, assets };
        let updated = self.persist_document(
            imported,
            &new_bytes,
            "application/epub+zip",
            Some(format!("{}.epub", safe_file_stem(&title))),
            "normalized",
            Some(&current),
        )?;
        self.books[position] = updated.clone();
        if let Err(error) = self.reload_cover(&book_id) {
            tracing::warn!(
                %book_id,
                revision = updated.revision,
                error = %format!("{error:#}"),
                "图书保存已提交，但封面缓存加载失败；已保留持久修订，重新加载图书时将重试封面"
            );
        }
        Ok(PublishedDocument {
            record: updated,
            unit_revisions,
        })
    }

    /// Content hash and own revision of every chapter of one published
    /// revision. `apply_document_with_assets` uses this to keep a chapter's
    /// revision — and therefore its notes and译文 — when an edit did not change
    /// that chapter's body. A chapter whose stored structure cannot be read is
    /// reported as changed so nothing is ever reused by accident.
    fn published_unit_content(
        &self,
        book_id: &str,
        revision: u64,
    ) -> Result<HashMap<String, (u64, String)>> {
        let conn = db::open_conn(&self.db_path)?;
        let Some(source) = db::book_sources::get_revision(&conn, book_id, revision)? else {
            return Ok(HashMap::new());
        };
        Ok(db::content_units::list_for_source(&conn, &source.id)?
            .into_iter()
            .map(|unit| {
                let hash = serde_json::from_str::<BlockDocument>(&unit.block_json)
                    .map(|document| document.content_hash())
                    .unwrap_or_default();
                (unit.id, (unit.revision, hash))
            })
            .collect())
    }

    /// Parses and cleans a source edit before atomically publishing the next
    /// canonical revision. Invalid HTML never reaches SQLite.
    pub fn update_content_unit_source(
        &mut self,
        book_id: &str,
        unit_id: &str,
        source: &str,
    ) -> Result<BookRecord> {
        let document = self.document(book_id)?;
        let mut editor = crate::editing::DocumentEditor::new(document)?;
        editor.update_unit_source(unit_id, source)?;
        self.apply_document(editor.into_document())
    }

    pub fn insert_media_block(
        &mut self,
        book_id: &str,
        unit_id: &str,
        position: usize,
        draft: MediaDraft,
    ) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let asset = DocumentAsset::from_bytes(
            draft.kind.role(),
            draft.media_type.clone(),
            draft.original_file_name.clone(),
            draft.bytes.as_slice(),
        );
        let asset_id = asset.id.clone();
        merge_document_asset(&mut document, asset)?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == unit_id)
            .context("内容单元不存在")?;
        ensure!(position <= unit.document.blocks.len(), "媒体插入位置越界");
        let block_id = unique_block_id(unit, &asset_id, position);
        let caption = draft
            .description
            .as_deref()
            .map(|value| vec![crate::document::Inline::text(value)])
            .unwrap_or_default();
        let block = media_block(&draft, block_id, asset_id.clone(), caption);
        unit.document.blocks.insert(position, block);
        unit.source = crate::markup::serialize_source(&unit.document)?;
        document.validate()?;
        self.apply_document_with_assets(document, HashMap::from([(asset_id, draft.bytes.clone())]))
    }

    pub fn replace_media_block(
        &mut self,
        book_id: &str,
        unit_id: &str,
        block_id: &str,
        draft: MediaDraft,
    ) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let asset = DocumentAsset::from_bytes(
            draft.kind.role(),
            draft.media_type.clone(),
            draft.original_file_name.clone(),
            draft.bytes.as_slice(),
        );
        let asset_id = asset.id.clone();
        merge_document_asset(&mut document, asset)?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == unit_id)
            .context("内容单元不存在")?;
        let replaced =
            replace_media_in_blocks(&mut unit.document.blocks, block_id, &draft, &asset_id);
        ensure!(replaced, "媒体块不存在");
        unit.source = crate::markup::serialize_source(&unit.document)?;
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document_with_assets(document, HashMap::from([(asset_id, draft.bytes.clone())]))
    }

    pub fn delete_media_block(
        &mut self,
        book_id: &str,
        unit_id: &str,
        block_id: &str,
    ) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == unit_id)
            .context("内容单元不存在")?;
        ensure!(
            delete_media_from_blocks(&mut unit.document.blocks, block_id),
            "媒体块不存在"
        );
        unit.source = crate::markup::serialize_source(&unit.document)?;
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document(document)
    }

    pub fn set_cover(&mut self, book_id: &str, cover: CoverDraft) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let mut asset = DocumentAsset::from_bytes(
            AssetRole::Cover,
            cover.mime.clone(),
            None,
            cover.bytes.as_slice(),
        );
        asset.add_role(AssetRole::ContentImage);
        let asset_id = asset.id.clone();
        merge_document_asset(&mut document, asset)?;
        document.cover_asset_id = Some(asset_id.clone());
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document_with_assets(document, HashMap::from([(asset_id, cover.bytes.clone())]))
    }

    pub fn clear_cover(&mut self, book_id: &str) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        document.cover_asset_id = None;
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document(document)
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

    pub fn remove_book(&mut self, book_id: &str) -> Result<()> {
        let position = self
            .books
            .iter()
            .position(|book| book.id == book_id)
            .context("图书不存在")?;
        let mut conn = db::open_conn(&self.db_path)?;
        let unreferenced = db::transactions::delete_document(&mut conn, book_id)?;
        self.books.remove(position);
        self.cover_cache.remove(book_id);
        self.progress_incarnations.remove(book_id);
        self.progress_write_sequences
            .retain(|(candidate_id, _), _| candidate_id != book_id);
        self.schedule_blob_deletions(unreferenced);
        Ok(())
    }

    fn progress_action_is_stale(&self, id: &str, ordered_write: Option<(u64, u64)>) -> bool {
        let Some((incarnation, sequence)) = ordered_write else {
            return false;
        };
        self.progress_write_sequences
            .get(&(id.to_string(), incarnation))
            .is_some_and(|current| sequence <= *current)
    }

    fn accept_progress_action(&mut self, id: &str, ordered_write: Option<(u64, u64)>) {
        if let Some((incarnation, sequence)) = ordered_write {
            self.progress_write_sequences
                .insert((id.to_string(), incarnation), sequence);
        }
    }

    /// Persists progress against a stable canonical content unit. Fixed-layout
    /// readers use this path so a stored PDF page can be resolved through the
    /// unit's `SourceLocator::PdfPage` instead of relying on an ordinal alone.
    pub fn update_progress_at(
        &mut self,
        id: &str,
        last_spine: usize,
        content_unit_id: &str,
    ) -> Result<()> {
        self.update_progress_at_inner(id, last_spine, content_unit_id, None)
            .map(|_| ())
    }

    /// Ordered stable-locator variant used by concurrent Reader windows.
    pub fn update_progress_at_ordered(
        &mut self,
        id: &str,
        last_spine: usize,
        content_unit_id: &str,
        book_incarnation: u64,
        write_sequence: u64,
    ) -> Result<bool> {
        self.update_progress_at_inner(
            id,
            last_spine,
            content_unit_id,
            Some((book_incarnation, write_sequence)),
        )
    }

    fn update_progress_at_inner(
        &mut self,
        id: &str,
        _last_spine: usize,
        content_unit_id: &str,
        ordered_write: Option<(u64, u64)>,
    ) -> Result<bool> {
        if self.progress_action_is_stale(id, ordered_write) {
            return Ok(false);
        }
        if ordered_write
            .is_some_and(|(incarnation, _)| self.progress_incarnation(id) != Some(incarnation))
        {
            self.accept_progress_action(id, ordered_write);
            return Ok(false);
        }
        let Some(position) = self.books.iter().position(|book| book.id == id) else {
            self.accept_progress_action(id, ordered_write);
            return Ok(false);
        };
        let locator = DocumentLocator::unit(id, content_unit_id);
        let locator_json = serde_json::to_string(&locator)?;
        let mut conn = db::open_conn(&self.db_path)?;
        match db::transactions::update_current_unit_progress(
            &mut conn,
            id,
            content_unit_id,
            &locator_json,
            now_secs(),
        )? {
            db::transactions::CurrentProgressWrite::Updated {
                source_revision,
                spine_index,
            } => {
                self.accept_progress_action(id, ordered_write);
                // The caller's ordinal belongs to its opening snapshot. The
                // transaction resolves the stable unit in the current source.
                self.books[position].last_spine = spine_index;
                self.books[position].revision = source_revision;
                Ok(true)
            }
            db::transactions::CurrentProgressWrite::MissingBook if ordered_write.is_some() => {
                self.accept_progress_action(id, ordered_write);
                Ok(false)
            }
            db::transactions::CurrentProgressWrite::MissingBook => bail!("图书不存在"),
            db::transactions::CurrentProgressWrite::MissingUnit
            | db::transactions::CurrentProgressWrite::UnitNotCurrent
                if ordered_write.is_some() =>
            {
                // A Reader may finish closing after an editor has removed its
                // unit. Consume this obsolete action without replacing the
                // current progress or trapping the window in a retry loop.
                self.accept_progress_action(id, ordered_write);
                Ok(false)
            }
            db::transactions::CurrentProgressWrite::MissingUnit => bail!("阅读位置不存在"),
            db::transactions::CurrentProgressWrite::UnitNotCurrent => {
                bail!("阅读位置不属于指定图书的当前版本")
            }
        }
    }

    pub fn create_group(&mut self, name: &str, parent_id: Option<&str>) -> Result<BookGroup> {
        let name = validate_group_name(name)?;
        let group = BookGroup {
            id: self.new_group_id(&name, parent_id),
            name,
            parent_id: parent_id.map(str::to_owned),
            created_at: now_secs(),
        };
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::create_group(&mut conn, &group, MAX_GROUP_DEPTH)?;
        self.groups.push(group.clone());
        Ok(group)
    }

    pub fn rename_group(&mut self, id: &str, name: &str) -> Result<()> {
        let name = validate_group_name(name)?;
        let position = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .context("分组不存在")?;
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::rename_group(&mut conn, id, &name)?;
        self.groups[position].name = name;
        Ok(())
    }

    pub fn delete_group(&mut self, id: &str) -> Result<()> {
        ensure!(self.groups.iter().any(|group| group.id == id), "分组不存在");
        let cached = self
            .group_subtree_ids(id)
            .into_iter()
            .collect::<HashSet<_>>();
        let mut conn = db::open_conn(&self.db_path)?;
        let removed = db::transactions::delete_group_tree(&mut conn, id)?
            .into_iter()
            .chain(cached)
            .collect::<HashSet<_>>();
        self.groups.retain(|group| !removed.contains(&group.id));
        for book in &mut self.books {
            if book
                .group_id
                .as_ref()
                .is_some_and(|group_id| removed.contains(group_id))
            {
                book.group_id = None;
            }
        }
        Ok(())
    }

    pub fn set_book_group(&mut self, book_id: &str, group_id: Option<&str>) -> Result<()> {
        let position = self
            .books
            .iter()
            .position(|book| book.id == book_id)
            .context("图书不存在")?;
        let conn = db::open_conn(&self.db_path)?;
        db::transactions::set_book_group(&conn, book_id, group_id)?;
        self.books[position].group_id = group_id.map(str::to_owned);
        Ok(())
    }

    fn persist_document(
        &self,
        imported: ImportedBook,
        current_source_bytes: &[u8],
        source_media_type: &str,
        source_name: Option<String>,
        source_kind: &str,
        previous: Option<&BookRecord>,
    ) -> Result<BookRecord> {
        imported.validate(&ImportLimits::default())?;
        // This method is called on an AppServices blocking worker (or during
        // startup/tests), never from a GPUI entity callback. Keep every object
        // put and the reference-publishing SQLite transaction under one gate
        // so a concurrent collector cannot remove a reused content hash in the
        // gap between those two operations.
        let publication_guard = self.io.block_on(self.blob_publication.acquire());
        let source_key = self
            .io
            .block_on(self.blob_store.put(current_source_bytes))?;
        let mut asset_keys = HashMap::new();
        for asset in &imported.assets {
            let key = self
                .io
                .block_on(self.blob_store.put(asset.bytes.as_slice()))?;
            ensure!(
                key == BlobKey::from_bytes(asset.bytes.as_slice()),
                "对象存储返回了错误的内容地址"
            );
            asset_keys.insert(asset.metadata.id.clone(), key);
        }
        let mut conn = db::open_conn(&self.db_path)?;
        let previous_progress = previous
            .map(|record| db::progress::get(&conn, &record.id))
            .transpose()?
            .flatten();
        let graph = build_persisted_graph(
            imported,
            source_key,
            current_source_bytes.len() as u64,
            source_media_type,
            source_name,
            source_kind,
            asset_keys,
            previous,
            previous_progress.as_ref(),
        )?;
        // `graph.book` is the exact catalog row validated and published by
        // the transaction below. Do not issue a fallible SELECT after commit:
        // a transient read error at that point would misreport an already
        // durable import/save as failed and leave the UI projection stale.
        let committed_record = graph.book.clone();
        let auto_run_background_jobs = self.auto_run_background_jobs.load(Ordering::Acquire);
        let unreferenced = if previous.is_some() {
            db::transactions::install_document_revision(
                &mut conn,
                &graph.borrow(),
                auto_run_background_jobs,
            )?
        } else {
            db::transactions::insert_document(
                &mut conn,
                &graph.borrow(),
                auto_run_background_jobs,
            )?;
            Vec::new()
        };
        drop(conn);
        drop(publication_guard);
        self.schedule_blob_deletions(unreferenced);
        Ok(committed_record)
    }

    fn read_blob(&self, object_key: &str) -> Result<Vec<u8>> {
        let key = BlobKey::parse(object_key)?;
        self.io.block_on(self.blob_store.get(&key))
    }

    fn reload_cover(&mut self, book_id: &str) -> Result<()> {
        self.cover_cache.remove(book_id);
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_cover_cache_hydration) {
            bail!("测试注入的封面缓存加载失败");
        }
        let Some(record) = self.books.iter().find(|book| book.id == book_id) else {
            return Ok(());
        };
        if let Some(key) = record.cover_object_key.as_deref() {
            self.cover_cache
                .insert(book_id.to_string(), Arc::new(self.read_blob(key)?));
        }
        Ok(())
    }

    fn new_group_id(&self, name: &str, parent_id: Option<&str>) -> String {
        deterministic_id(
            "group",
            format!("{name}\0{}\0{}", parent_id.unwrap_or("-"), now_nanos()).as_bytes(),
        )
    }

    fn schedule_blob_deletions(&self, blobs: Vec<db::blobs::BlobRecord>) {
        if blobs.is_empty() {
            return;
        }
        let store = Arc::clone(&self.blob_store);
        let db_path = self.db_path.clone();
        let publication = self.blob_publication.clone();
        self.io.spawn(async move {
            reclaim_unreferenced_blobs(&db_path, &store, &publication, blobs, "对象").await;
        });
    }
}

/// Reclaims stale candidates without trusting the snapshot that produced
/// them. Every candidate is serialized with publication and re-checked in a
/// fresh connection immediately before its bytes are removed.
pub(crate) async fn reclaim_unreferenced_blobs(
    db_path: &Path,
    store: &LocalBlobStore,
    publication: &BlobPublicationLock,
    blobs: Vec<db::blobs::BlobRecord>,
    label: &'static str,
) {
    for blob in blobs {
        let publication_guard = publication.acquire().await;
        reclaim_unreferenced_blob_while_locked(db_path, store, &blob, label).await;
        drop(publication_guard);
    }
}

async fn reclaim_unreferenced_blob_while_locked(
    db_path: &Path,
    store: &LocalBlobStore,
    blob: &db::blobs::BlobRecord,
    label: &'static str,
) {
    let object_key = blob.object_key.clone();
    let check_path = db_path.to_path_buf();
    let still_unreferenced = match tokio::task::spawn_blocking(move || {
        db::blobs::is_unreferenced(&db::open_conn(&check_path)?, &object_key)
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            tracing::warn!(object_key = %blob.object_key, %error, "{label}回收前无法复核引用，保留对象");
            return;
        }
        Err(error) => {
            tracing::warn!(object_key = %blob.object_key, %error, "{label}引用复核任务异常退出，保留对象");
            return;
        }
    };
    if !still_unreferenced {
        return;
    }

    let key = match BlobKey::parse(&blob.object_key) {
        Ok(key) => key,
        Err(error) => {
            tracing::error!(object_key = %blob.object_key, %error, "忽略无效的{label}待回收对象键");
            return;
        }
    };
    if let Err(error) = store.delete(&key).await {
        tracing::warn!(object_key = %blob.object_key, %error, "异步删除未引用{label}失败，启动 GC 将重试");
        return;
    }

    let cleanup_path = db_path.to_path_buf();
    let object_key = blob.object_key.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = db::open_conn(&cleanup_path)?;
        db::blobs::delete_if_unreferenced(&conn, &object_key)?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "{label}已删除，但清理对象元数据失败，启动 GC 将重试")
        }
        Err(error) => tracing::warn!(%error, "{label}对象元数据清理任务异常退出"),
    }
}

/// Startup is the only point where a global object sweep is safe: no import
/// can have published bytes while its SQLite transaction is still pending.
/// Registered-but-unreferenced rows are removed after their bytes, then
/// content-addressed files left by an interrupted transaction are collected.
fn run_startup_blob_gc(
    conn: &rusqlite::Connection,
    store: &LocalBlobStore,
    io: &IoRuntime,
) -> Result<()> {
    for blob in db::blobs::list_unreferenced(conn)? {
        let key = BlobKey::parse(&blob.object_key)?;
        match io.block_on(store.delete(&key)) {
            Ok(()) => {
                db::blobs::delete(conn, &blob.object_key)?;
            }
            Err(error) => {
                tracing::warn!(object_key = %blob.object_key, %error, "启动 GC 无法删除未引用对象，将保留后重试");
            }
        }
    }

    let registered = db::blobs::list(conn)?
        .into_iter()
        .map(|blob| BlobKey::parse(&blob.object_key))
        .collect::<Result<Vec<_>>>()?;
    if let Err(error) = io.block_on(store.delete_orphans(&registered)) {
        tracing::warn!(%error, "启动 GC 无法完成孤儿对象扫描，将在下次启动重试");
    }
    Ok(())
}

struct PersistedGraph {
    book: db::books::BookRecord,
    source_blob: db::blobs::BlobRecord,
    additional_blobs: Vec<db::blobs::BlobRecord>,
    source: db::book_sources::BookSource,
    progress: db::progress::ReadingProgress,
    units: Vec<db::content_units::ContentUnit>,
    toc: Vec<db::toc_entries::TocEntry>,
    assets: Vec<db::assets::Asset>,
    asset_refs: Vec<db::asset_refs::AssetRef>,
    search_chunks: Vec<db::search_chunks::SearchChunk>,
    visual_pages: Vec<db::visual_pages::VisualPage>,
}

impl PersistedGraph {
    fn borrow(&self) -> db::transactions::DocumentGraph<'_> {
        db::transactions::DocumentGraph {
            book: &self.book,
            source_blob: &self.source_blob,
            additional_blobs: &self.additional_blobs,
            source: &self.source,
            progress: Some(&self.progress),
            content_units: &self.units,
            toc_entries: &self.toc,
            assets: &self.assets,
            asset_refs: &self.asset_refs,
            search_chunks: &self.search_chunks,
            visual_pages: &self.visual_pages,
        }
    }
}

fn build_persisted_graph(
    imported: ImportedBook,
    source_key: BlobKey,
    source_byte_len: u64,
    source_media_type: &str,
    source_name: Option<String>,
    source_kind: &str,
    asset_keys: HashMap<String, BlobKey>,
    previous: Option<&BookRecord>,
    previous_progress: Option<&db::progress::ReadingProgress>,
) -> Result<PersistedGraph> {
    let now = now_secs();
    let document = imported.document;
    let revision = previous
        .map(|record| record.revision + 1)
        .unwrap_or(document.revision.get());
    let source_id = deterministic_id(
        "source",
        format!("{}\0{revision}\0{source_key}", document.id).as_bytes(),
    );
    let mut blobs_by_key = HashMap::<String, db::blobs::BlobRecord>::new();
    let mut assets = Vec::new();
    for payload in &imported.assets {
        let key = asset_keys
            .get(&payload.metadata.id)
            .context("导入资源缺少对象键")?;
        blobs_by_key
            .entry(key.to_string())
            .or_insert_with(|| db::blobs::BlobRecord {
                object_key: key.to_string(),
                media_type: payload.metadata.media_type.clone(),
                byte_len: payload.metadata.byte_len,
                hash: payload.metadata.content_hash.clone(),
                created_at: now,
            });
        assets.push(db::assets::Asset {
            id: payload.metadata.id.clone(),
            book_id: document.id.clone(),
            source_id: source_id.clone(),
            object_key: key.to_string(),
            kind: asset_kind(&payload.metadata.roles).to_string(),
            href: format!("asset/{}", payload.metadata.id),
            media_type: payload.metadata.media_type.clone(),
            byte_len: payload.metadata.byte_len,
            width: None,
            height: None,
            created_at: now,
        });
    }
    let source_hash = source_key
        .as_str()
        .rsplit('/')
        .next()
        .context("无效的来源对象键")?
        .to_string();
    let source_blob = db::blobs::BlobRecord {
        object_key: source_key.to_string(),
        media_type: source_media_type.to_string(),
        byte_len: source_byte_len,
        hash: source_hash,
        created_at: now,
    };
    // The graph needs the exact source length; initial imports also have an
    // original asset with that key, so use its verified metadata when present.
    let source_blob = blobs_by_key
        .get(source_key.as_str())
        .cloned()
        .unwrap_or(source_blob);
    blobs_by_key.remove(source_key.as_str());

    let units = document
        .units
        .iter()
        .enumerate()
        .map(|(ordinal, unit)| {
            // An update publishes the per-chapter revisions its caller assigned.
            // A fresh import has no earlier chapter and starts every unit at the
            // document revision.
            let unit_revision = if previous.is_some() {
                unit.revision.get()
            } else {
                revision
            };
            db_unit(&document.id, &source_id, ordinal, unit, unit_revision, now)
        })
        .collect::<Result<Vec<_>>>()?;
    let href_by_unit = units
        .iter()
        .map(|unit| (unit.id.as_str(), unit.href.clone()))
        .collect::<HashMap<_, _>>();
    let mut toc = Vec::new();
    flatten_toc(
        &document.id,
        &source_id,
        &document.toc,
        None,
        0,
        &href_by_unit,
        now,
        &mut toc,
    )?;
    let mut asset_refs = Vec::new();
    for unit in &document.units {
        for (ordinal, asset_id) in unit.document.referenced_asset_ids().into_iter().enumerate() {
            let locator = DocumentLocator::unit(&document.id, &unit.id);
            asset_refs.push(db::asset_refs::AssetRef {
                id: deterministic_id(
                    "asset-ref",
                    format!("{}\0{asset_id}\0{ordinal}", unit.id).as_bytes(),
                ),
                content_unit_id: unit.id.clone(),
                asset_id: asset_id.to_string(),
                relation: "content".to_string(),
                ordinal,
                locator_json: serde_json::to_string(&locator)?,
                created_at: now,
            });
        }
    }
    let search_chunks = build_search_chunks(&document, &source_id, now)?;
    let cover = document
        .cover_asset_id
        .as_deref()
        .and_then(|id| assets.iter().find(|asset| asset.id == id));
    let format = if source_kind == "normalized" {
        "epub"
    } else {
        document_format(&document)
    }
    .to_string();
    let author = if document.authors.is_empty() {
        "未知作者".to_string()
    } else {
        document.authors.join("、")
    };
    let fallback_spine = previous
        .map(|record| record.last_spine)
        .unwrap_or(0)
        .min(units.len().saturating_sub(1));
    let progress_spine = previous_progress
        .and_then(|progress| progress.content_unit_id.as_deref())
        .and_then(|unit_id| units.iter().position(|unit| unit.id == unit_id))
        .unwrap_or(fallback_spine);
    let book = db::books::BookRecord {
        id: document.id.clone(),
        title: document.title.clone(),
        author,
        language: document.language.clone(),
        description: document.description.clone(),
        format: format.clone(),
        revision,
        source_object_key: source_key.to_string(),
        cover_asset_id: cover.map(|asset| asset.id.clone()),
        cover_object_key: cover.map(|asset| asset.object_key.clone()),
        cover_mime: cover.map(|asset| asset.media_type.clone()),
        added_at: previous.map(|record| record.added_at).unwrap_or(now),
        updated_at: now,
        last_spine: progress_spine,
        group_id: previous.and_then(|record| record.group_id.clone()),
    };
    let source = db::book_sources::BookSource {
        id: source_id,
        book_id: document.id,
        revision,
        format,
        source_kind: source_kind.to_string(),
        object_key: source_key.to_string(),
        source_name,
        created_at: now,
    };
    let progress = db::progress::ReadingProgress {
        book_id: book.id.clone(),
        source_revision: revision,
        content_unit_id: units.get(progress_spine).map(|unit| unit.id.clone()),
        spine_index: progress_spine,
        locator_json: units
            .get(progress_spine)
            .map(|unit| serde_json::to_string(&DocumentLocator::unit(&book.id, &unit.id)))
            .transpose()?
            .unwrap_or_else(|| "{}".to_string()),
        fraction: previous_progress
            .map(|progress| progress.fraction)
            .unwrap_or(0.0),
        updated_at: previous_progress
            .map(|progress| progress.updated_at)
            .unwrap_or(now),
    };
    Ok(PersistedGraph {
        book,
        source_blob,
        additional_blobs: blobs_by_key.into_values().collect(),
        source,
        progress,
        units,
        toc,
        assets,
        asset_refs,
        search_chunks,
        visual_pages: Vec::new(),
    })
}

fn db_unit(
    book_id: &str,
    source_id: &str,
    ordinal: usize,
    unit: &DocumentUnit,
    revision: u64,
    now: u64,
) -> Result<db::content_units::ContentUnit> {
    Ok(db::content_units::ContentUnit {
        id: unit.id.clone(),
        book_id: book_id.to_string(),
        source_id: source_id.to_string(),
        parent_id: None,
        ordinal,
        kind: unit_kind(unit.kind).to_string(),
        href: unit_href(unit),
        source_locator_json: serde_json::to_string(&unit.source_locator)?,
        title: Some(unit.title.clone()),
        media_type: Some("text/html".to_string()),
        source_text: Some(unit.source.clone()),
        block_json: serde_json::to_string(&unit.document)?,
        revision,
        created_at: now,
        updated_at: now,
    })
}

#[allow(clippy::too_many_arguments)]
fn flatten_toc(
    book_id: &str,
    source_id: &str,
    nodes: &[TocNode],
    parent_id: Option<&str>,
    depth: usize,
    href_by_unit: &HashMap<&str, Option<String>>,
    now: u64,
    output: &mut Vec<db::toc_entries::TocEntry>,
) -> Result<()> {
    for node in nodes {
        let ordinal = output.len();
        let unit_id = node.target.unit_id().to_string();
        let block_id = match &node.target {
            TocTarget::Unit { .. } => None,
            TocTarget::Block { block_id, .. } => Some(block_id.clone()),
        };
        let locator = match block_id.as_deref() {
            Some(block) => DocumentLocator::block(book_id, &unit_id, block),
            None => DocumentLocator::unit(book_id, &unit_id),
        };
        output.push(db::toc_entries::TocEntry {
            id: node.id.clone(),
            book_id: book_id.to_string(),
            source_id: source_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            ordinal,
            depth,
            label: node.label.clone(),
            href: href_by_unit.get(unit_id.as_str()).cloned().flatten(),
            content_unit_id: unit_id,
            target_block_id: block_id,
            locator_json: serde_json::to_string(&locator)?,
            created_at: now,
        });
        flatten_toc(
            book_id,
            source_id,
            &node.children,
            Some(&node.id),
            depth + 1,
            href_by_unit,
            now,
            output,
        )?;
    }
    Ok(())
}

fn build_search_chunks(
    document: &BookDocument,
    source_id: &str,
    now: u64,
) -> Result<Vec<db::search_chunks::SearchChunk>> {
    let mut result = Vec::new();
    for unit in &document.units {
        let mut ordinal = 0;
        for block in &unit.document.blocks {
            let text = block.plain_text();
            if text.trim().is_empty() {
                continue;
            }
            let ranges = character_chunks(&text, SEARCH_CHUNK_CHARACTERS, SEARCH_CHUNK_OVERLAP);
            for (start, end) in ranges {
                ensure!(
                    result.len() < MAX_SEARCH_CHUNKS_PER_DOCUMENT,
                    "文档搜索分块超过 {MAX_SEARCH_CHUNKS_PER_DOCUMENT} 个安全上限"
                );
                let body = text[start..end].to_string();
                let mut locator = DocumentLocator::text(
                    &document.id,
                    &unit.id,
                    block.id(),
                    start as u64,
                    end as u64,
                );
                if let Some(source) = unit.source_locator.clone() {
                    locator = locator.with_source(source);
                }
                result.push(db::search_chunks::SearchChunk {
                    id: deterministic_id(
                        "chunk",
                        format!("{}\0{}\0{}\0{}", unit.id, block.id(), start, end).as_bytes(),
                    ),
                    book_id: document.id.clone(),
                    source_id: source_id.to_string(),
                    content_unit_id: unit.id.clone(),
                    ordinal,
                    heading: unit.title.clone(),
                    token_count: body.chars().count().div_ceil(4),
                    content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
                    body,
                    locator_json: serde_json::to_string(&locator)?,
                    created_at: now,
                });
                ordinal += 1;
            }
        }
        if ordinal == 0 {
            ensure!(
                result.len() < MAX_SEARCH_CHUNKS_PER_DOCUMENT,
                "文档搜索分块超过 {MAX_SEARCH_CHUNKS_PER_DOCUMENT} 个安全上限"
            );
            let mut locator = DocumentLocator::unit(&document.id, &unit.id);
            if let Some(source) = unit.source_locator.clone() {
                locator = locator.with_source(source);
            }
            result.push(db::search_chunks::SearchChunk {
                id: deterministic_id("chunk", format!("{}\0empty", unit.id).as_bytes()),
                book_id: document.id.clone(),
                source_id: source_id.to_string(),
                content_unit_id: unit.id.clone(),
                ordinal: 0,
                heading: unit.title.clone(),
                body: String::new(),
                token_count: 0,
                content_hash: blake3::hash(b"").to_hex().to_string(),
                locator_json: serde_json::to_string(&locator)?,
                created_at: now,
            });
        }
    }
    Ok(result)
}

fn character_chunks(text: &str, size: usize, overlap: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let character_count = boundaries.len() - 1;
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < character_count {
        let end = (start + size).min(character_count);
        ranges.push((boundaries[start], boundaries[end]));
        if end == character_count {
            break;
        }
        start = end.saturating_sub(overlap.min(size.saturating_sub(1)));
    }
    ranges
}

fn generated_epub(document: &BookDocument) -> Result<Vec<u8>> {
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

fn document_unit_from_row(row: db::content_units::ContentUnit) -> Result<DocumentUnit> {
    Ok(DocumentUnit {
        id: row.id,
        revision: Revision::new(row.revision),
        kind: parse_unit_kind(&row.kind)?,
        title: row.title.unwrap_or_else(|| "未命名单元".to_string()),
        source_locator: serde_json::from_str(&row.source_locator_json)?,
        source: row.source_text.unwrap_or_default(),
        document: serde_json::from_str(&row.block_json)?,
    })
}

fn restore_toc(rows: Vec<db::toc_entries::TocEntry>) -> Vec<TocNode> {
    fn children(parent: Option<&str>, rows: &[db::toc_entries::TocEntry]) -> Vec<TocNode> {
        rows.iter()
            .filter(|row| row.parent_id.as_deref() == parent)
            .map(|row| {
                let target = row
                    .target_block_id
                    .as_ref()
                    .map(|block| TocTarget::block(&row.content_unit_id, block))
                    .unwrap_or_else(|| TocTarget::unit(&row.content_unit_id));
                let mut node = TocNode::new(&row.id, &row.label, target);
                node.children = children(Some(&row.id), rows);
                node
            })
            .collect()
    }
    children(None, &rows)
}

fn search_index(
    db_path: &Path,
    book_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if limit == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    ensure!(
        query.chars().count() <= MAX_SEARCH_QUERY_CHARS,
        "搜索内容最多 {MAX_SEARCH_QUERY_CHARS} 个字符"
    );
    ensure!(
        !query
            .chars()
            .any(|character| character == '\0'
                || (character.is_control() && !character.is_whitespace())),
        "搜索内容包含无效控制字符"
    );
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    db::book_search::search(
        &db::open_conn(db_path)?,
        book_id,
        &terms,
        limit.min(MAX_SEARCH_RESULTS) as i64,
    )
}

fn unit_href(unit: &DocumentUnit) -> Option<String> {
    match unit.source_locator.as_ref() {
        Some(SourceLocator::Epub { href }) => Some(href.clone()),
        Some(SourceLocator::PdfPage { page }) => Some(format!("page:{page}")),
        Some(SourceLocator::OfficeRenderedPage { page }) => Some(format!("office-page:{page}")),
        Some(SourceLocator::OfficeSection { index }) => Some(format!("section:{index}")),
        Some(SourceLocator::Slide { index }) => Some(format!("slide:{index}")),
        Some(SourceLocator::Worksheet { name, range }) => Some(match range {
            Some(range) => format!("sheet:{name}:{range}"),
            None => format!("sheet:{name}"),
        }),
        Some(SourceLocator::KindleSection { index, href }) => {
            href.clone().or_else(|| Some(format!("kindle:{index}")))
        }
        Some(SourceLocator::DjvuPage { page }) => Some(format!("djvu:{page}")),
        Some(SourceLocator::Created) | None => Some(format!("unit:{}", unit.id)),
    }
}

fn non_empty_option(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn merge_document_asset(document: &mut BookDocument, incoming: DocumentAsset) -> Result<()> {
    if let Some(existing) = document
        .assets
        .iter_mut()
        .find(|asset| asset.id == incoming.id)
    {
        ensure!(
            existing.content_hash == incoming.content_hash
                && existing.byte_len == incoming.byte_len
                && existing.media_type == incoming.media_type,
            "同一资源 ID 对应了不同内容"
        );
        for role in incoming.roles {
            existing.add_role(role);
        }
        if existing.original_file_name.is_none() {
            existing.original_file_name = incoming.original_file_name;
        }
    } else {
        document.assets.push(incoming);
    }
    Ok(())
}

fn unique_block_id(unit: &DocumentUnit, asset_id: &str, position: usize) -> String {
    for salt in 0_u64.. {
        let id = deterministic_id(
            "block",
            format!(
                "{}\0{}\0{}\0{}\0{salt}",
                unit.id,
                unit.revision.get(),
                asset_id,
                position
            )
            .as_bytes(),
        );
        if unit.find_block(&id).is_none() {
            return id;
        }
    }
    unreachable!("u64 block ID salt space exhausted")
}

fn media_block(
    draft: &MediaDraft,
    block_id: String,
    asset_id: String,
    caption: Vec<crate::document::Inline>,
) -> Block {
    match draft.kind {
        MediaKind::Image => Block::Image {
            id: block_id,
            asset_id,
            alt: draft.description.clone().unwrap_or_default(),
            title: draft.title.clone(),
            caption,
        },
        MediaKind::Audio => Block::Audio {
            id: block_id,
            asset_id,
            title: draft.title.clone(),
            caption,
        },
        MediaKind::Video => Block::Video {
            id: block_id,
            asset_id,
            poster_asset_id: None,
            title: draft.title.clone(),
            caption,
        },
    }
}

fn replace_media_in_blocks(
    blocks: &mut [Block],
    block_id: &str,
    draft: &MediaDraft,
    asset_id: &str,
) -> bool {
    for block in blocks {
        if block.id() == block_id {
            let old = std::mem::replace(
                block,
                Block::ThematicBreak {
                    id: block_id.to_string(),
                },
            );
            let caption = match old {
                Block::Image { caption, .. }
                | Block::Audio { caption, .. }
                | Block::Video { caption, .. } => draft
                    .description
                    .as_deref()
                    .map(|value| vec![crate::document::Inline::text(value)])
                    .unwrap_or(caption),
                other => {
                    *block = other;
                    return false;
                }
            };
            *block = media_block(draft, block_id.to_string(), asset_id.to_string(), caption);
            return true;
        }
        let found = match block {
            Block::BlockQuote { blocks, .. } => {
                replace_media_in_blocks(blocks, block_id, draft, asset_id)
            }
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| replace_media_in_blocks(&mut item.blocks, block_id, draft, asset_id)),
            _ => false,
        };
        if found {
            return true;
        }
    }
    false
}

fn delete_media_from_blocks(blocks: &mut Vec<Block>, block_id: &str) -> bool {
    if let Some(position) = blocks.iter().position(|block| block.id() == block_id) {
        if matches!(
            blocks[position],
            Block::Image { .. } | Block::Audio { .. } | Block::Video { .. }
        ) {
            blocks.remove(position);
            return true;
        }
        return false;
    }
    for block in blocks {
        let removed = match block {
            Block::BlockQuote { blocks, .. } => delete_media_from_blocks(blocks, block_id),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| delete_media_from_blocks(&mut item.blocks, block_id)),
            _ => false,
        };
        if removed {
            return true;
        }
    }
    false
}

fn prune_unreferenced_assets(document: &mut BookDocument) {
    let referenced = document
        .units
        .iter()
        .flat_map(|unit| unit.document.referenced_asset_ids())
        .collect::<HashSet<_>>();
    let cover = document.cover_asset_id.as_deref();
    let original = match &document.source {
        DocumentSource::Imported {
            original_asset_id, ..
        } => Some(original_asset_id.as_str()),
        DocumentSource::Created => None,
    };
    document.assets.retain(|asset| {
        referenced.contains(asset.id.as_str())
            || cover == Some(asset.id.as_str())
            || original == Some(asset.id.as_str())
    });
}

fn asset_kind(roles: &[AssetRole]) -> &'static str {
    if roles.contains(&AssetRole::OriginalSource) {
        "original_source"
    } else if roles.contains(&AssetRole::Cover) {
        "cover"
    } else if roles.contains(&AssetRole::Audio) {
        "audio"
    } else if roles.contains(&AssetRole::Video) {
        "video"
    } else if roles.contains(&AssetRole::Poster) {
        "poster"
    } else if roles.contains(&AssetRole::ContentImage) {
        "image"
    } else if roles.contains(&AssetRole::Font) {
        "font"
    } else if roles.contains(&AssetRole::Stylesheet) {
        "stylesheet"
    } else {
        "attachment"
    }
}

fn asset_roles_from_kind(kind: &str) -> Vec<AssetRole> {
    match kind {
        "original_source" => vec![AssetRole::OriginalSource],
        "cover" => vec![AssetRole::Cover, AssetRole::ContentImage],
        "audio" => vec![AssetRole::Audio],
        "video" => vec![AssetRole::Video],
        "poster" => vec![AssetRole::Poster],
        "image" => vec![AssetRole::ContentImage],
        "font" => vec![AssetRole::Font],
        "stylesheet" => vec![AssetRole::Stylesheet],
        _ => vec![AssetRole::Attachment],
    }
}

fn document_format(document: &BookDocument) -> &'static str {
    match &document.source {
        DocumentSource::Created => "epub",
        DocumentSource::Imported { format, .. } => format_name(*format),
    }
}

fn format_name(format: BookFormat) -> &'static str {
    match format {
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
    }
}

fn parse_format(value: &str) -> Result<BookFormat> {
    Ok(match value {
        "epub" => BookFormat::Epub,
        "pdf" => BookFormat::Pdf,
        "doc" => BookFormat::Doc,
        "docx" => BookFormat::Docx,
        "pptx" => BookFormat::Pptx,
        "xlsx" => BookFormat::Xlsx,
        "mobi" => BookFormat::Mobi,
        "azw" => BookFormat::Azw,
        "azw3" => BookFormat::Azw3,
        "kfx" => BookFormat::Kfx,
        "djvu" => BookFormat::Djvu,
        _ => bail!("未知图书格式：{value}"),
    })
}

fn unit_kind(kind: ContentUnitKind) -> &'static str {
    match kind {
        ContentUnitKind::Chapter => "chapter",
        ContentUnitKind::Section => "section",
        ContentUnitKind::Page => "page",
        ContentUnitKind::Slide => "slide",
        ContentUnitKind::Worksheet => "worksheet",
    }
}

fn parse_unit_kind(value: &str) -> Result<ContentUnitKind> {
    Ok(match value {
        "chapter" => ContentUnitKind::Chapter,
        "section" => ContentUnitKind::Section,
        "page" => ContentUnitKind::Page,
        "slide" => ContentUnitKind::Slide,
        "worksheet" => ContentUnitKind::Worksheet,
        _ => bail!("未知内容单元类型：{value}"),
    })
}

fn imported_source_name(document: &BookDocument) -> Option<String> {
    match &document.source {
        DocumentSource::Imported {
            original_file_name, ..
        } => original_file_name.clone(),
        DocumentSource::Created => None,
    }
}

fn split_authors(value: &str) -> Vec<String> {
    value
        .split(['、', ',', ';'])
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "未知作者")
        .map(str::to_string)
        .collect()
}

fn non_empty(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn validate_group_name(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(!name.is_empty(), "分组名称不能为空");
    ensure!(
        name.chars().count() <= MAX_GROUP_NAME_LEN,
        "分组名称最多 {MAX_GROUP_NAME_LEN} 个字符"
    );
    Ok(name.to_string())
}

fn validate_cover_dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(width > 0 && height > 0, "封面图片尺寸无效");
    ensure!(
        width <= MAX_COVER_DIMENSION
            && height <= MAX_COVER_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_COVER_PIXELS,
        "封面图片尺寸过大"
    );
    Ok(())
}

fn replacement_cover_extension(mime: &str) -> Option<&'static str> {
    match mime.split(';').next()?.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn safe_file_stem(value: &str) -> String {
    let stem = value
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            character => character,
        })
        .collect::<String>();
    non_empty(&stem, "book")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("无法读取当前目录")?
            .join(path))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::AnnotationKind;
    use crate::editing::{DocumentEditor, NewContentUnit};

    fn initial_note_draft(
        library: &LibraryStore,
        book_id: &str,
        kind: AnnotationKind,
    ) -> AnnotationDraft {
        let document = library.document(book_id).unwrap();
        AnnotationDraft {
            content_unit_id: document.units[0].id.clone(),
            document_revision: document.revision.get(),
            unit_revision: document.units[0].revision.get(),
            anchor: TextAnchor {
                quote: "第一章".into(),
                start: 0,
                end: 3,
            },
            kind,
            comment: kind.is_comment().then(|| "对这一段的想法".to_string()),
        }
    }

    #[test]
    fn annotations_share_storage_reopen_and_preserve_human_ai_authorship() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let book = library.create_book("阅读笔记", "作者").unwrap();
        let other = library.create_book("另一书", "作者").unwrap();
        let mut ids = Vec::new();
        for kind in [
            AnnotationKind::Highlight,
            AnnotationKind::HumanComment,
            AnnotationKind::AiComment,
        ] {
            let draft = initial_note_draft(&library, &book.id, kind);
            ids.push(library.create_annotation(&book.id, &draft).unwrap().id);
        }
        assert!(
            library
                .list_annotations(&other.id, None)
                .unwrap()
                .is_empty()
        );
        assert!(library.delete_annotation(&other.id, &ids[0]).is_err());
        assert!(
            library
                .update_human_comment(&book.id, &ids[2], "伪成人工")
                .is_err()
        );
        assert!(
            library
                .update_human_comment(&other.id, &ids[1], "跨书修改")
                .is_err()
        );
        let edited = library
            .update_human_comment(&book.id, &ids[1], "人工修改后的想法")
            .unwrap();
        assert_eq!(edited.kind, AnnotationKind::HumanComment);
        assert_eq!(edited.comment.as_deref(), Some("人工修改后的想法"));
        drop(library);

        let mut reopened = LibraryStore::load_from(temp.path().into()).unwrap();
        let notes = reopened.list_annotations(&book.id, None).unwrap();
        assert_eq!(notes.len(), 3);
        assert!(notes.iter().all(|note| !note.stale));
        assert_eq!(
            notes.iter().find(|note| note.id == ids[2]).unwrap().kind,
            AnnotationKind::AiComment
        );
        reopened.delete_annotation(&book.id, &ids[0]).unwrap();
        assert_eq!(reopened.list_annotations(&book.id, None).unwrap().len(), 2);
        reopened.remove_book(&book.id).unwrap();
        let conn = db::open_conn(reopened.database_path()).unwrap();
        assert!(
            db::annotations::list(&conn, &book.id, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn annotations_reject_wrong_book_quote_revision_and_survive_document_changes_as_stale() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let book = library.create_book("阅读笔记", "作者").unwrap();
        let other = library.create_book("另一书", "作者").unwrap();
        let draft = initial_note_draft(&library, &book.id, AnnotationKind::HumanComment);
        assert!(library.create_annotation(&other.id, &draft).is_err());
        let mut wrong = draft.clone();
        wrong.anchor.quote = "不存在".into();
        assert!(library.create_annotation(&book.id, &wrong).is_err());
        wrong = draft.clone();
        wrong.unit_revision += 1;
        assert!(library.create_annotation(&book.id, &wrong).is_err());
        wrong = draft.clone();
        wrong.document_revision += 1;
        assert!(library.create_annotation(&book.id, &wrong).is_err());
        let note = library.create_annotation(&book.id, &draft).unwrap();
        library
            .update_content_unit_source(
                &book.id,
                &draft.content_unit_id,
                "<h1>新章</h1><p>正文已经改变</p>",
            )
            .unwrap();
        assert!(library.create_annotation(&book.id, &draft).is_err());
        drop(library);
        let mut reopened = LibraryStore::load_from(temp.path().into()).unwrap();
        let notes = reopened.list_annotations(&book.id, None).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].stale);
        assert_eq!(notes[0].anchor.quote, "第一章");
        let edited = reopened
            .update_human_comment(&book.id, &note.id, "仍可整理旧版想法")
            .unwrap();
        assert!(edited.stale);
    }

    #[test]
    fn annotations_replace_exclusive_marks_and_delete_only_the_exact_range() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let (book, first_id, second_id) = create_two_unit_book(&mut library);
        let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
        editor
            .update_unit_source(&first_id, "<h1>第一章</h1><p>第一章</p>")
            .unwrap();
        editor
            .update_unit_identity(&second_id, "第一章", ContentUnitKind::Chapter)
            .unwrap();
        editor
            .update_unit_source(&second_id, "<h1>第一章</h1>")
            .unwrap();
        library.apply_document(editor.into_document()).unwrap();
        let other = library.create_book("其他图书", "作者").unwrap();
        for kind in [AnnotationKind::HumanComment, AnnotationKind::AiComment] {
            let draft = initial_note_draft(&library, &book.id, kind);
            library.create_annotation(&book.id, &draft).unwrap();
        }

        let mut draft = initial_note_draft(&library, &book.id, AnnotationKind::Highlight);
        let first_mark = library.create_annotation(&book.id, &draft).unwrap();
        for kind in [
            AnnotationKind::Wavy,
            AnnotationKind::Underline,
            AnnotationKind::Highlight,
        ] {
            draft.kind = kind;
            let mark = library.create_annotation(&book.id, &draft).unwrap();
            assert_eq!(mark.id, first_mark.id);
            assert_eq!(mark.kind, kind);
            assert_eq!(library.create_annotation(&book.id, &draft).unwrap(), mark);
            let notes = library.list_annotations(&book.id, None).unwrap();
            assert_eq!(notes.len(), 3);
            assert_eq!(notes.iter().filter(|note| note.kind.is_mark()).count(), 1);
            assert_eq!(
                notes
                    .iter()
                    .filter(|note| note.kind == AnnotationKind::HumanComment)
                    .count(),
                1
            );
            assert_eq!(
                notes
                    .iter()
                    .filter(|note| note.kind == AnnotationKind::AiComment)
                    .count(),
                1
            );
        }
        // The SQLite constraint independently prevents two mark styles at the
        // same range, while the human/AI thoughts above share that range.
        let conn = db::open_conn(library.database_path()).unwrap();
        let mut duplicate = first_mark.clone();
        duplicate.id.push_str("-conflict");
        duplicate.kind = AnnotationKind::Wavy;
        let error = db::annotations::insert(&conn, &duplicate).unwrap_err();
        assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
        assert!(
            conn.execute(
                "UPDATE annotations SET kind = 'strikethrough' WHERE id = ?1",
                [&first_mark.id]
            )
            .is_err()
        );
        drop(conn);

        let mut duplicate_quote = draft.clone();
        duplicate_quote.anchor.start = 3;
        duplicate_quote.anchor.end = 6;
        let duplicate_mark = library
            .create_annotation(&book.id, &duplicate_quote)
            .unwrap();
        let mut other_chapter = draft.clone();
        other_chapter.content_unit_id = second_id;
        let other_chapter_mark = library.create_annotation(&book.id, &other_chapter).unwrap();
        let other_draft = initial_note_draft(&library, &other.id, AnnotationKind::Underline);
        let other_book_mark = library.create_annotation(&other.id, &other_draft).unwrap();

        assert!(
            library
                .delete_annotation_marks(
                    &other.id,
                    &draft.content_unit_id,
                    other_draft.document_revision,
                    other_draft.unit_revision,
                    &draft.anchor
                )
                .is_err()
        );
        let mut wrong_anchor = draft.anchor.clone();
        wrong_anchor.quote = "不存在".into();
        assert!(
            library
                .delete_annotation_marks(
                    &book.id,
                    &draft.content_unit_id,
                    draft.document_revision,
                    draft.unit_revision,
                    &wrong_anchor
                )
                .is_err()
        );
        for _ in 0..2 {
            library
                .delete_annotation_marks(
                    &book.id,
                    &draft.content_unit_id,
                    draft.document_revision,
                    draft.unit_revision,
                    &draft.anchor,
                )
                .unwrap();
        }
        let notes = library.list_annotations(&book.id, None).unwrap();
        let range_notes = notes
            .iter()
            .filter(|note| {
                note.content_unit_id == draft.content_unit_id
                    && note.anchor.start == draft.anchor.start
                    && note.anchor.end == draft.anchor.end
            })
            .collect::<Vec<_>>();
        assert_eq!(range_notes.len(), 2);
        assert!(range_notes.iter().all(|note| note.kind.is_comment()));
        assert!(notes.iter().any(|note| note.id == duplicate_mark.id));
        assert!(notes.iter().any(|note| note.id == other_chapter_mark.id));
        assert_eq!(
            library.list_annotations(&other.id, None).unwrap(),
            vec![other_book_mark]
        );

        // Late requests cannot erase marks anchored in another revision.
        let document = library.document(&book.id).unwrap();
        library.apply_document(document).unwrap();
        assert!(
            library
                .delete_annotation_marks(
                    &book.id,
                    &duplicate_quote.content_unit_id,
                    duplicate_quote.document_revision,
                    duplicate_quote.unit_revision,
                    &duplicate_quote.anchor
                )
                .is_err()
        );
        let mut conn = db::open_conn(library.database_path()).unwrap();
        assert!(
            db::transactions::delete_annotation_marks(
                &mut conn,
                &book.id,
                &duplicate_quote.content_unit_id,
                duplicate_quote.document_revision,
                duplicate_quote.unit_revision,
                &duplicate_quote.anchor
            )
            .is_err()
        );
        drop(conn);
        drop(library);
        let reopened = LibraryStore::load_from(temp.path().into()).unwrap();
        let notes = reopened.list_annotations(&book.id, None).unwrap();
        assert_eq!(notes.len(), 4);
        assert!(notes.iter().all(|note| note.stale));
        assert!(notes.iter().any(|note| note.id == duplicate_mark.id));
    }

    /// PDF page notes reuse the EPUB rules for storage, ownership, revisions
    /// and exclusive marks. Only the canonical-text comparison differs: a page
    /// anchor addresses the pinned PDF.js text layer, which no host extractor
    /// reproduces, so the range is checked against its own quote instead.
    #[test]
    fn pdf_page_notes_share_storage_and_scope_rules_without_reading_page_text() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let book = library.create_book("PDF 笔记", "作者").unwrap();
        let chapter_book = library.create_book("重排图书", "作者").unwrap();
        let document = library.document(&book.id).unwrap();
        let page_id = document.units[0].id.clone();
        let mut editor = DocumentEditor::new(document).unwrap();
        editor
            .update_unit_identity(&page_id, "第 1 页", ContentUnitKind::Page)
            .unwrap();
        library.apply_document(editor.into_document()).unwrap();
        let document = library.document(&book.id).unwrap();
        let mut draft = AnnotationDraft {
            content_unit_id: page_id.clone(),
            document_revision: document.revision.get(),
            unit_revision: document.units[0].revision.get(),
            anchor: TextAnchor {
                quote: "页面 选区".into(),
                start: 12,
                end: 16,
            },
            kind: AnnotationKind::Highlight,
            comment: None,
        };

        // The very same draft is not a valid reflowable chapter anchor.
        assert!(library.create_annotation(&book.id, &draft).is_err());
        let mark = library.create_pdf_annotation(&book.id, &draft).unwrap();
        for kind in [AnnotationKind::Wavy, AnnotationKind::Underline] {
            draft.kind = kind;
            let replaced = library.create_pdf_annotation(&book.id, &draft).unwrap();
            assert_eq!(replaced.id, mark.id);
            assert_eq!(replaced.kind, kind);
        }
        let mut thoughts = Vec::new();
        for kind in [AnnotationKind::HumanComment, AnnotationKind::AiComment] {
            let mut thought = draft.clone();
            thought.kind = kind;
            thought.comment = Some("对这一页的想法".to_string());
            thoughts.push(
                library
                    .create_pdf_annotation(&book.id, &thought)
                    .unwrap()
                    .id,
            );
        }
        assert_eq!(library.list_annotations(&book.id, None).unwrap().len(), 3);

        for invalid in [
            {
                // A range that cannot describe its own quote.
                let mut invalid = draft.clone();
                invalid.anchor.end += 1;
                invalid
            },
            {
                let mut invalid = draft.clone();
                invalid.anchor.quote = "   ".into();
                invalid
            },
            {
                let mut invalid = draft.clone();
                invalid.document_revision += 1;
                invalid
            },
            {
                let mut invalid = draft.clone();
                invalid.unit_revision += 1;
                invalid
            },
            {
                // A reflowable chapter is never a PDF page position.
                let other = library.document(&chapter_book.id).unwrap();
                let mut invalid = draft.clone();
                invalid.content_unit_id = other.units[0].id.clone();
                invalid.document_revision = other.revision.get();
                invalid.unit_revision = other.units[0].revision.get();
                invalid
            },
        ] {
            assert!(
                library.create_pdf_annotation(&book.id, &invalid).is_err(),
                "{invalid:?}"
            );
        }
        assert!(
            library
                .create_pdf_annotation(&chapter_book.id, &draft)
                .is_err()
        );

        // Removing the mark keeps both thoughts on the identical range.
        library
            .delete_pdf_annotation_marks(
                &book.id,
                &page_id,
                draft.document_revision,
                draft.unit_revision,
                &draft.anchor,
            )
            .unwrap();
        drop(library);
        let reopened = LibraryStore::load_from(temp.path().into()).unwrap();
        let notes = reopened.list_annotations(&book.id, Some(&page_id)).unwrap();
        assert_eq!(notes.len(), 2);
        assert!(
            notes
                .iter()
                .all(|note| note.kind.is_comment() && !note.stale)
        );
        assert!(thoughts.iter().all(|id| notes.iter().any(|n| &n.id == id)));
    }

    #[test]
    fn annotations_verify_duplicate_offsets_unicode_and_exported_heading() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let book = library.create_book("定位", "作者").unwrap();
        let unit_id = library.document(&book.id).unwrap().units[0].id.clone();
        library
            .update_content_unit_source(&book.id, &unit_id, "<p>甲😀重复</p><p>乙重复</p>")
            .unwrap();
        let document = library.document(&book.id).unwrap();
        let mut draft = AnnotationDraft {
            content_unit_id: unit_id,
            document_revision: document.revision.get(),
            unit_revision: document.units[0].revision.get(),
            anchor: TextAnchor {
                quote: "重 复".into(),
                start: 9,
                end: 11,
            },
            kind: AnnotationKind::Highlight,
            comment: None,
        };
        let note = library.create_annotation(&book.id, &draft).unwrap();
        assert_eq!(note.anchor.start, 9);
        draft.anchor.start = 8;
        draft.anchor.end = 10;
        assert!(library.create_annotation(&book.id, &draft).is_err());
        draft.anchor = TextAnchor {
            quote: "😀".into(),
            start: 4,
            end: 6,
        };
        library.create_annotation(&book.id, &draft).unwrap();
        draft.anchor.end = 5;
        assert!(library.create_annotation(&book.id, &draft).is_err());
    }

    #[test]
    fn annotations_retain_deleted_chapter_and_transaction_rejects_late_old_revision() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let (book, first_unit_id, _) = create_two_unit_book(&mut library);
        let draft = initial_note_draft(&library, &book.id, AnnotationKind::HumanComment);
        let note = library.create_annotation(&book.id, &draft).unwrap();
        let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
        editor.remove_unit(&first_unit_id).unwrap();
        library.apply_document(editor.into_document()).unwrap();
        let mut conn = db::open_conn(library.database_path()).unwrap();
        let mut late_note = note.clone();
        late_note.id.push_str("-late");
        assert!(db::transactions::insert_annotation(&mut conn, &late_note).is_err());
        drop(conn);
        drop(library);
        let reopened = LibraryStore::load_from(temp.path().into()).unwrap();
        let notes = reopened.list_annotations(&book.id, None).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].stale);
        assert_eq!(notes[0].content_unit_id, first_unit_id);
        assert_eq!(notes[0].comment, note.comment);
    }

    #[test]
    fn annotation_overview_reads_all_books_and_chapters_with_current_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        assert!(library.annotation_overview(None).unwrap().is_empty());
        let (book, first_unit, second_unit) = create_two_unit_book(&mut library);
        let empty = library.create_book("没有笔记", "作者").unwrap();
        let old_projection = library.clone();
        let other = library.create_book("另一书", "作者").unwrap();
        assert!(
            old_projection
                .books()
                .iter()
                .all(|item| item.id != other.id)
        );

        for kind in [AnnotationKind::Highlight, AnnotationKind::HumanComment] {
            let draft = initial_note_draft(&library, &book.id, kind);
            library.create_annotation(&book.id, &draft).unwrap();
        }
        let document = library.document(&book.id).unwrap();
        let mut ai_draft = initial_note_draft(&library, &book.id, AnnotationKind::AiComment);
        ai_draft.content_unit_id = second_unit.clone();
        ai_draft.unit_revision = document.units[1].revision.get();
        ai_draft.anchor.quote = "第二章".into();
        let ai_note = library.create_annotation(&book.id, &ai_draft).unwrap();
        let mut other_draft = initial_note_draft(&library, &other.id, AnnotationKind::Wavy);
        library.create_annotation(&other.id, &other_draft).unwrap();
        other_draft.kind = AnnotationKind::Underline;
        other_draft.anchor.quote = "第一".into();
        other_draft.anchor.end = 2;
        library.create_annotation(&other.id, &other_draft).unwrap();

        // Deterministic timestamps exercise both newest-first order and ties.
        let conn = db::open_conn(library.database_path()).unwrap();
        conn.execute(
            "UPDATE annotations SET updated_at = (SELECT MAX(created_at) + 1 FROM annotations)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE annotations SET updated_at = updated_at + 1 WHERE id = ?1",
            [&ai_note.id],
        )
        .unwrap();
        drop(conn);
        let notes = old_projection.annotation_overview(None).unwrap();
        assert_eq!(notes.len(), 5);
        assert_eq!(notes[0].annotation.id, ai_note.id);
        assert!(
            notes[1..]
                .windows(2)
                .all(|pair| { pair[0].annotation.id < pair[1].annotation.id })
        );
        assert!(notes.iter().all(|note| !note.annotation.stale));
        for kind in [
            AnnotationKind::Highlight,
            AnnotationKind::Wavy,
            AnnotationKind::Underline,
            AnnotationKind::HumanComment,
            AnnotationKind::AiComment,
        ] {
            assert!(notes.iter().any(|note| note.annotation.kind == kind));
        }
        let scoped = old_projection.annotation_overview(Some(&book.id)).unwrap();
        assert_eq!(scoped.len(), 3);
        assert!(
            scoped.iter().all(|note| {
                note.annotation.book_id == book.id && note.book_title == book.title
            })
        );
        assert_eq!(scoped[0].chapter_title.as_deref(), Some("第二章"));
        assert_eq!(scoped[0].chapter_index, Some(1));
        assert!(scoped[1..].iter().all(|note| {
            note.chapter_title.as_deref() == Some("第一章") && note.chapter_index == Some(0)
        }));
        assert!(
            library
                .annotation_overview(Some(&empty.id))
                .unwrap()
                .is_empty()
        );
        assert!(library.annotation_overview(Some("missing-book")).is_err());

        let mut editor = DocumentEditor::new(document).unwrap();
        editor
            .set_metadata("更新后的书名", vec!["作者".into()], None, None)
            .unwrap();
        editor
            .update_unit_identity(&first_unit, "改名的第一章", ContentUnitKind::Chapter)
            .unwrap();
        editor.remove_unit(&second_unit).unwrap();
        library.apply_document(editor.into_document()).unwrap();
        let scoped = old_projection.annotation_overview(Some(&book.id)).unwrap();
        assert_eq!(scoped.len(), 3);
        assert!(
            scoped
                .iter()
                .all(|note| note.annotation.stale && note.book_title == "更新后的书名")
        );
        let deleted_chapter = &scoped[0];
        assert_eq!(deleted_chapter.annotation.id, ai_note.id);
        assert_eq!(deleted_chapter.annotation.anchor, ai_note.anchor);
        assert_eq!(deleted_chapter.annotation.comment, ai_note.comment);
        assert_eq!(deleted_chapter.chapter_title, None);
        assert_eq!(deleted_chapter.chapter_index, None);
        assert!(scoped[1..].iter().all(|note| {
            note.chapter_title.as_deref() == Some("改名的第一章") && note.chapter_index == Some(0)
        }));
        library.remove_book(&other.id).unwrap();
        assert_eq!(old_projection.annotation_overview(None).unwrap().len(), 3);
        assert!(old_projection.annotation_overview(Some(&other.id)).is_err());
    }

    #[test]
    fn annotation_overview_never_uses_foreign_book_or_source_chapter_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let book = library.create_book("阅读笔记", "作者").unwrap();
        let other = library.create_book("另一书", "作者").unwrap();
        let draft = initial_note_draft(&library, &book.id, AnnotationKind::HumanComment);
        let mut note = library.create_annotation(&book.id, &draft).unwrap();
        note.id.push_str("-foreign-unit");
        note.content_unit_id = library.document(&other.id).unwrap().units[0].id.clone();
        // Fault-injected persisted row bypasses the host's ownership checks.
        let conn = db::open_conn(library.database_path()).unwrap();
        db::annotations::insert(&conn, &note).unwrap();
        let overview = library.annotation_overview(Some(&book.id)).unwrap();
        let foreign = overview
            .iter()
            .find(|entry| entry.annotation.id == note.id)
            .unwrap();
        assert!(foreign.annotation.stale);
        assert_eq!(foreign.chapter_title, None);
        assert_eq!(foreign.chapter_index, None);
        assert_eq!(foreign.book_title, book.title);
        assert!(
            library
                .annotation_overview(Some(&other.id))
                .unwrap()
                .is_empty()
        );
        let other_source = db::book_sources::get_revision(&conn, &other.id, other.revision)
            .unwrap()
            .unwrap();
        conn.execute(
            "UPDATE content_units SET source_id = ?1,
                ordinal = (SELECT COALESCE(MAX(ordinal), -1) + 1 FROM content_units WHERE source_id = ?1)
             WHERE id = ?2",
            rusqlite::params![other_source.id, draft.content_unit_id],
        )
        .unwrap();
        assert!(
            library
                .annotation_overview(Some(&book.id))
                .unwrap()
                .iter()
                .all(|entry| {
                    entry.annotation.stale
                        && entry.chapter_title.is_none()
                        && entry.chapter_index.is_none()
                })
        );
    }

    #[test]
    fn failed_object_reset_discards_the_fresh_database_and_retries_next_start() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        fs::create_dir_all(&data_dir).unwrap();
        let object_root = data_dir.join(OBJECT_DIRECTORY);

        // A non-directory at the managed object path deterministically makes
        // remove_dir_all fail on every supported platform. The database has
        // already been created by this point and must be rolled back.
        fs::write(&object_root, b"blocks object-directory reset").unwrap();
        let error = LibraryStore::load_from(data_dir.clone())
            .expect_err("an invalid managed-object path must fail startup");
        assert!(error.to_string().contains("无法清理旧对象目录"));
        assert!(!data_dir.join(db::DATABASE_FILE).exists());
        assert!(!data_dir.join("library.db-wal").exists());
        assert!(!data_dir.join("library.db-shm").exists());
        assert!(!data_dir.join("library.db-journal").exists());

        // Once the transient obstruction is gone, the missing database keeps
        // the whole reset eligible. A stale object is removed before the new
        // LocalBlobStore is published.
        fs::remove_file(&object_root).unwrap();
        fs::create_dir_all(&object_root).unwrap();
        let stale = object_root.join("stale-object");
        fs::write(&stale, b"orphan").unwrap();
        let library = LibraryStore::load_from(data_dir.clone())
            .expect("the next startup must retry and complete the reset");
        assert!(!stale.exists());
        assert!(data_dir.join(db::DATABASE_FILE).is_file());
        assert!(library.books().is_empty());
    }

    fn create_two_unit_book(library: &mut LibraryStore) -> (BookRecord, String, String) {
        let book = library.create_book("双章节进度", "作者").unwrap();
        let document = library.document(&book.id).unwrap();
        let first_unit_id = document.units[0].id.clone();
        let mut editor = DocumentEditor::new(document).unwrap();
        let second_unit_id = editor
            .add_unit(NewContentUnit::html_chapter("第二章", 1))
            .unwrap();
        let updated = library.apply_document(editor.into_document()).unwrap();
        (updated, first_unit_id, second_unit_id)
    }

    #[test]
    fn editing_one_chapter_keeps_the_other_chapters_revision_and_translation() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
        let (book, first_id, second_id) = create_two_unit_book(&mut library);
        let published = library.document(&book.id).unwrap();
        assert_eq!(published.revision.get(), book.revision);
        let second_revision = published
            .units
            .iter()
            .find(|unit| unit.id == second_id)
            .unwrap()
            .revision
            .get();

        // One translated block per chapter, written by the running task.
        let conn = db::open_conn(library.database_path()).unwrap();
        for unit_id in [&first_id, &second_id] {
            let unit = db::content_units::get(&conn, unit_id).unwrap().unwrap();
            db::translations::upsert(
                &conn,
                &db::translations::NewTranslation {
                    book_id: book.id.clone(),
                    content_unit_id: unit_id.clone(),
                    block_id: format!("{unit_id}::h0"),
                    ordinal: 0,
                    document_revision: book.revision,
                    unit_revision: unit.revision,
                    target_language: "zh-Hans".to_string(),
                    source_language: Some("zh".to_string()),
                    model: "chat-model".to_string(),
                    source_text: "原文".to_string(),
                    translated_text: serde_json::to_string(
                        &crate::translation::StoredTranslation {
                            execution_identity: "translation-v2:test".to_string(),
                            segments: vec![crate::translation::TranslationSegment {
                                source: "原文".to_string(),
                                translated: "译文".to_string(),
                            }],
                        },
                    )
                    .unwrap(),
                    created_at: 1,
                    updated_at: 1,
                },
            )
            .unwrap();
        }
        drop(conn);

        library
            .update_content_unit_source(&book.id, &first_id, "<h1>改写</h1><p>新的正文</p>")
            .unwrap();

        let edited = library.document(&book.id).unwrap();
        assert_eq!(edited.revision.get(), book.revision + 1);
        let first = edited
            .units
            .iter()
            .find(|unit| unit.id == first_id)
            .unwrap();
        let second = edited
            .units
            .iter()
            .find(|unit| unit.id == second_id)
            .unwrap();
        assert_eq!(
            first.revision, edited.revision,
            "the edited chapter must take the new revision"
        );
        assert_eq!(
            second.revision.get(),
            second_revision,
            "an untouched chapter must keep its own revision"
        );

        let conn = db::open_conn(library.database_path()).unwrap();
        let kept = db::translations::list_for_unit(&conn, &second_id, "zh-Hans").unwrap();
        assert_eq!(kept.len(), 1, "an untouched chapter keeps its译文");
        assert_eq!(
            kept[0].document_revision,
            edited.revision.get(),
            "and follows the newly published document revision"
        );
        assert_eq!(kept[0].unit_revision, second_revision);
        let stale = db::translations::list_for_unit(&conn, &first_id, "zh-Hans").unwrap();
        assert_eq!(stale.len(), 1);
        assert_ne!(
            stale[0].unit_revision,
            first.revision.get(),
            "the edited chapter's译文 must be stale"
        );
        assert_eq!(stale[0].document_revision, book.revision);
    }

    #[test]
    fn chunks_utf8_without_splitting_characters() {
        let text = "中文abc".repeat(1_000);
        let ranges = character_chunks(&text, 100, 10);
        assert!(ranges.len() > 1);
        assert!(
            ranges
                .iter()
                .all(|(start, end)| text.get(*start..*end).is_some())
        );
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, text.len());
    }

    #[test]
    fn search_chunks_keep_block_and_text_range_locators() {
        let long_text = "中文定位".repeat(500);
        let first = Block::paragraph("block-long", long_text);
        let second = Block::paragraph("block-short", "表格附近的说明");
        let unit = DocumentUnit::new(
            "unit-locators",
            ContentUnitKind::Chapter,
            "定位测试",
            "",
            BlockDocument::new(vec![first, second]),
        )
        .with_source_locator(SourceLocator::created());
        let mut document = BookDocument::created("book-locators", "定位测试");
        document.units.push(unit);

        let chunks = build_search_chunks(&document, "source-locators", 1).unwrap();
        assert_eq!(chunks.len(), 3, "long block splits twice; short block once");
        for (ordinal, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, ordinal);
            let locator: DocumentLocator = serde_json::from_str(&chunk.locator_json).unwrap();
            locator.validate().unwrap();
            assert_eq!(locator.source, Some(SourceLocator::created()));
            let block = document.units[0]
                .document
                .find_block(locator.block_id.as_deref().unwrap())
                .unwrap();
            let text = block.plain_text();
            let range = locator.text_range.unwrap();
            assert_eq!(
                &text[range.start_byte as usize..range.end_byte as usize],
                chunk.body
            );
        }
        assert_eq!(
            serde_json::from_str::<DocumentLocator>(&chunks[2].locator_json)
                .unwrap()
                .block_id
                .as_deref(),
            Some("block-short")
        );
    }

    #[test]
    fn created_book_uses_object_store_and_survives_reload() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = library.create_book("  测试新书  ", "作者").unwrap();
        assert_eq!(book.title, "测试新书");
        assert!(library.epub_bytes(&book.id).unwrap().starts_with(b"PK"));
        let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
        assert_eq!(restored.books().len(), 1);
        let document = restored.document(&book.id).unwrap();
        assert_eq!(document.units[0].source, "<h1>第一章</h1><p>开始写作……</p>");
        let conn = db::open_conn(restored.database_path()).unwrap();
        let stored = db::content_units::get(&conn, &document.units[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.media_type.as_deref(), Some("text/html"));
        assert_eq!(
            stored.source_text.as_deref(),
            Some(document.units[0].source.as_str())
        );
        assert!(matches!(document.source, DocumentSource::Created));
    }

    #[test]
    fn shared_auto_run_flag_pauses_new_and_edited_document_jobs() {
        fn assert_paused_jobs(library: &LibraryStore, book_id: &str, source_id: &str) {
            let conn = db::open_conn(library.database_path()).unwrap();
            let jobs = db::index_jobs::list_for_book(&conn, book_id).unwrap();
            assert_eq!(jobs.len(), 3);
            assert!(jobs.iter().all(|job| {
                job.source_id.as_deref() == Some(source_id)
                    && job.status == db::index_jobs::IndexJobStatus::Paused
                    && !job.pause_requested
                    && !job.cancel_requested
                    && job.attempts == 0
                    && job.started_at.is_none()
                    && job.finished_at.is_none()
            }));
        }

        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let flag = library.background_job_auto_run_flag();
        let projected_clone = library.clone();
        assert!(Arc::ptr_eq(
            &flag,
            &projected_clone.background_job_auto_run_flag()
        ));
        flag.store(false, Ordering::Release);

        let created = library.create_book("暂停派生任务", "作者").unwrap();
        let first_source = db::book_sources::get_revision(
            &db::open_conn(library.database_path()).unwrap(),
            &created.id,
            created.revision,
        )
        .unwrap()
        .unwrap();
        assert_paused_jobs(&library, &created.id, &first_source.id);

        let mut document = library.document(&created.id).unwrap();
        document.title = "暂停编辑后的派生任务".to_string();
        let updated = library.apply_document(document).unwrap();
        let second_source = db::book_sources::get_revision(
            &db::open_conn(library.database_path()).unwrap(),
            &updated.id,
            updated.revision,
        )
        .unwrap()
        .unwrap();
        assert_ne!(second_source.id, first_source.id);
        assert_paused_jobs(&library, &updated.id, &second_source.id);
        assert!(
            db::index_jobs::list_for_source_kind(
                &db::open_conn(library.database_path()).unwrap(),
                &first_source.id,
                "embedding",
            )
            .unwrap()
            .is_empty(),
            "the edited revision must remove superseded paused work"
        );
    }

    #[test]
    fn create_book_rejects_blank_title_without_persisting_a_book() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();

        let error = library
            .create_book("  \t\r\n  ", "")
            .expect_err("blank title must be rejected");

        assert!(format!("{error:#}").contains("图书名称不能为空"));
        assert!(library.books().is_empty());
        let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
        assert!(restored.books().is_empty());
    }

    #[test]
    fn committed_document_does_not_depend_on_a_postcommit_catalog_read() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let created = library.create_book("提交后无需读回", "作者").unwrap();
        let mut document = library.document(&created.id).unwrap();
        let unit_id = document.units[0].id.clone();
        let mut editor = crate::editing::DocumentEditor::new(document).unwrap();
        editor
            .update_unit_source(
                &unit_id,
                "<h1>已提交</h1><p>即使提交后读回失败，这一版也应成功。</p>",
            )
            .unwrap();
        document = editor.into_document();

        // `apply_document` reads the current row once, then the revision
        // transaction reads it once while holding its write lock. The former
        // implementation issued a third `books::get` after commit. Make that
        // exact next query fail: the save must still return the already
        // validated and published graph record.
        db::books::fail_get_after_for_test(2);
        let updated = library
            .apply_document(document)
            .expect("a post-commit SELECT failure must not turn a durable save into failure");
        assert_eq!(updated.revision, created.revision + 1);
        assert_eq!(
            library
                .books()
                .iter()
                .find(|record| record.id == updated.id),
            Some(&updated)
        );

        // Prove the injected failure was not silently consumed elsewhere and
        // that a subsequent normal read sees the durable row.
        let injected = library.book_record(&updated.id).unwrap_err();
        assert!(injected.to_string().contains("测试注入的图书记录读取失败"));
        assert_eq!(library.book_record(&updated.id).unwrap(), updated);
    }

    #[test]
    fn cached_projection_preserves_newer_content_but_applies_shelf_state_and_removals() {
        let temp = tempfile::tempdir().unwrap();
        let mut canonical = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = canonical.create_book("投影合并", "作者").unwrap();
        let group = canonical.create_group("待阅读", None).unwrap();
        canonical.set_book_group(&book.id, Some(&group.id)).unwrap();
        let service_projection = canonical.clone();

        let mut window = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let mut newer_editor_record = window.book_record(&book.id).unwrap();
        newer_editor_record.title = "编辑器中的较新标题".to_string();
        newer_editor_record.revision += 1;
        newer_editor_record.updated_at += 1;
        newer_editor_record.group_id = None;
        assert!(window.apply_cached_book_update(newer_editor_record, None));

        window.merge_cached_projection(service_projection);
        let merged = &window.books()[0];
        assert_eq!(merged.title, "编辑器中的较新标题");
        assert_eq!(merged.group_id.as_deref(), Some(group.id.as_str()));
        assert_eq!(window.groups(), &[group]);

        canonical.remove_book(&book.id).unwrap();
        window.merge_cached_projection(canonical);
        assert!(window.books().is_empty());
    }

    #[test]
    fn source_edit_is_cleaned_before_revision_is_published() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = library.create_book("安全编辑", "作者").unwrap();
        let unit_id = library.document(&book.id).unwrap().units[0].id.clone();

        let updated = library
            .update_content_unit_source(
                &book.id,
                &unit_id,
                "<h1>新标题</h1><p>正文<script>alert(1)</script></p>",
            )
            .unwrap();

        assert_eq!(updated.revision, book.revision + 1);
        let unit = library.document(&book.id).unwrap().units.remove(0);
        assert!(!unit.source.contains("script"));
        assert!(unit.source.contains("正文"));
    }

    #[test]
    fn media_insert_replace_delete_uses_owned_immutable_assets() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = library.create_book("媒体图书", "作者").unwrap();
        let unit_id = library.document(&book.id).unwrap().units[0].id.clone();
        let audio_bytes = b"ID3-test-audio".to_vec();
        let audio = MediaDraft::from_bytes(
            MediaKind::Audio,
            "audio/mpeg",
            Some("voice.mp3".into()),
            audio_bytes.clone(),
        )
        .unwrap()
        .with_title("朗读")
        .with_description("人工说明");
        library
            .insert_media_block(&book.id, &unit_id, 1, audio)
            .unwrap();
        let document = library.document(&book.id).unwrap();
        let (block_id, audio_asset_id) = match &document.units[0].document.blocks[1] {
            Block::Audio { id, asset_id, .. } => (id.clone(), asset_id.clone()),
            other => panic!("expected audio block, got {other:?}"),
        };
        assert_eq!(
            library.asset_bytes(&book.id, &audio_asset_id).unwrap(),
            audio_bytes
        );

        let video_bytes = b"test-video-container".to_vec();
        let video = MediaDraft::from_bytes(
            MediaKind::Video,
            "video/mp4",
            Some("clip.mp4".into()),
            video_bytes.clone(),
        )
        .unwrap();
        library
            .replace_media_block(&book.id, &unit_id, &block_id, video)
            .unwrap();
        assert!(library.asset_bytes(&book.id, &audio_asset_id).is_err());
        let document = library.document(&book.id).unwrap();
        let video_asset_id = match &document.units[0].document.blocks[1] {
            Block::Video { asset_id, .. } => asset_id.clone(),
            other => panic!("expected video block, got {other:?}"),
        };
        assert_eq!(
            library.asset_bytes(&book.id, &video_asset_id).unwrap(),
            video_bytes
        );

        library
            .delete_media_block(&book.id, &unit_id, &block_id)
            .unwrap();
        assert!(library.asset_bytes(&book.id, &video_asset_id).is_err());
        assert_eq!(
            library.document(&book.id).unwrap().units[0]
                .document
                .blocks
                .len(),
            2
        );
    }

    #[test]
    fn canonical_cover_is_stored_outside_sqlite() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 0, 0]),
        ))
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = library.create_book("封面图书", "作者").unwrap();
        let cover = CoverDraft::from_bytes(png.clone()).unwrap();
        library.set_cover(&book.id, cover).unwrap();
        assert_eq!(
            library.cover_bytes(&book.id).unwrap().unwrap().as_ref(),
            &png
        );
        library.clear_cover(&book.id).unwrap();
        assert!(library.cover_bytes(&book.id).unwrap().is_none());
    }

    #[test]
    fn committed_import_succeeds_when_cover_cache_hydration_fails() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 0, 0]),
        ))
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = library.create_book("提交后封面失败", "作者").unwrap();
        let record = library
            .set_cover(&book.id, CoverDraft::from_bytes(png).unwrap())
            .unwrap();
        let cover_key = BlobKey::parse(record.cover_object_key.as_deref().unwrap()).unwrap();
        assert!(library.cover_bytes_cached(&record.id).is_some());

        // Simulate corruption discovered only after the SQLite import
        // transaction has committed. The database row and derivative jobs are
        // already durable at this point, so finalizing the import may only
        // degrade the optional in-memory cover projection.
        library
            .io
            .block_on(library.blob_store.delete(&cover_key))
            .unwrap();
        library.books.retain(|candidate| candidate.id != record.id);

        let outcome = library.finish_committed_import(record.clone());

        assert!(matches!(outcome, ImportOutcome::Added(added) if added.id == record.id));
        assert!(library.cover_bytes_cached(&record.id).is_none());
        assert_eq!(library.book_record(&record.id).unwrap().id, record.id);
        let conn = db::open_conn(library.database_path()).unwrap();
        assert!(
            !db::index_jobs::list_for_book(&conn, &record.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn committed_editor_save_succeeds_when_cover_cache_hydration_fails() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 0, 0]),
        ))
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        let mut library = LibraryStore::load_from(data_dir.clone()).unwrap();
        let created = library.create_book("提交后保存", "作者").unwrap();
        let covered = library
            .set_cover(&created.id, CoverDraft::from_bytes(png.clone()).unwrap())
            .unwrap();
        let unit_id = library.document(&created.id).unwrap().units[0].id.clone();
        let cover_key = BlobKey::parse(covered.cover_object_key.as_deref().unwrap()).unwrap();

        // The normalized source, canonical document and cover object are
        // already durable when apply_document_with_assets refreshes this
        // optional UI cache. Simulate a transient read failure at exactly that
        // post-commit boundary.
        library.fail_next_cover_cache_hydration = true;
        let updated = library
            .update_content_unit_source(
                &created.id,
                &unit_id,
                "<h1>已保存标题</h1><p>已提交的正文</p>",
            )
            .expect("a cover cache failure must not report the committed save as failed");

        assert_eq!(updated.revision, covered.revision + 1);
        assert_eq!(
            library.book_record(&created.id).unwrap().revision,
            updated.revision
        );
        assert_eq!(
            library
                .books()
                .iter()
                .find(|record| record.id == created.id)
                .unwrap()
                .revision,
            updated.revision
        );
        assert!(library.cover_bytes_cached(&created.id).is_none());
        assert!(
            library
                .io
                .block_on(library.blob_store.exists(&cover_key))
                .unwrap(),
            "the cache failure must not damage the durable cover object"
        );
        assert!(
            library.document(&created.id).unwrap().units[0]
                .source
                .contains("已提交的正文")
        );

        drop(library);
        let reopened = LibraryStore::load_from(data_dir).unwrap();
        assert_eq!(
            reopened.book_record(&created.id).unwrap().revision,
            updated.revision
        );
        assert!(
            reopened.document(&created.id).unwrap().units[0]
                .source
                .contains("已提交的正文")
        );
        assert_eq!(
            reopened.cover_bytes(&created.id).unwrap().unwrap().as_ref(),
            &png
        );
    }

    #[test]
    fn cover_file_is_rejected_by_metadata_before_full_allocation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized-cover.png");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_COVER_BYTES + 1).unwrap();
        drop(file);

        let error = CoverDraft::read(&path).unwrap_err();
        assert!(error.to_string().contains("32 MiB 安全上限"));
    }

    #[test]
    fn startup_gc_collects_an_object_left_before_database_commit() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        let library = LibraryStore::load_from(data_dir.clone()).unwrap();
        let key = library
            .io
            .block_on(library.blob_store.put(b"uncommitted-object"))
            .unwrap();
        assert!(
            library
                .io
                .block_on(library.blob_store.exists(&key))
                .unwrap()
        );
        drop(library);

        let reopened = LibraryStore::load_from(data_dir).unwrap();
        assert!(
            !reopened
                .io
                .block_on(reopened.blob_store.exists(&key))
                .unwrap()
        );
    }

    #[test]
    fn removing_a_book_unlinks_first_and_reclaims_objects_in_background() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        let mut library = LibraryStore::load_from(data_dir.clone()).unwrap();
        let book = library.create_book("待删除图书", "作者").unwrap();
        let source_key = BlobKey::parse(&book.source_object_key).unwrap();

        library.remove_book(&book.id).unwrap();
        assert!(library.book_record(&book.id).is_err());
        drop(library);

        let reopened = LibraryStore::load_from(data_dir).unwrap();
        assert!(
            !reopened
                .io
                .block_on(reopened.blob_store.exists(&source_key))
                .unwrap()
        );
        let conn = db::open_conn(reopened.database_path()).unwrap();
        assert!(
            db::blobs::get(&conn, source_key.as_str())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn concurrent_reuse_cannot_be_deleted_by_a_stale_gc_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        let mut library = LibraryStore::load_from(data_dir).unwrap();
        let book = library.create_book("对象复用", "作者").unwrap();
        let source_key = BlobKey::parse(&book.source_object_key).unwrap();
        let source_bytes = library.source_bytes(&book.id).unwrap();
        let imported = ImportedBook {
            document: library.document(&book.id).unwrap(),
            assets: Vec::new(),
        };

        // Capture exactly the stale snapshot an unlinking transaction hands to
        // the asynchronous collector, without starting that collector yet.
        let candidates = db::transactions::delete_document(
            &mut db::open_conn(library.database_path()).unwrap(),
            &book.id,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].object_key, source_key.as_str());

        let publication = library.blob_publication_lock();
        let publication_guard = library.io.block_on(publication.acquire());
        let publisher = library.clone();
        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        publication.observe_next_acquire(attempted_tx);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let publish_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = publisher.persist_document(
                imported,
                &source_bytes,
                "application/epub+zip",
                None,
                "created",
                None,
            );
            finished_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        attempted_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("publisher reached the publication gate");
        assert!(
            matches!(
                finished_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ),
            "publisher must wait while reclamation owns the publication gate"
        );

        // Delete the old unreferenced instance while publication is excluded.
        // Once the gate is released, the publisher must recreate the same hash
        // before committing its new SQLite reference.
        library.io.block_on(reclaim_unreferenced_blob_while_locked(
            library.database_path(),
            &library.blob_store,
            &candidates[0],
            "测试对象",
        ));
        assert!(
            !library
                .io
                .block_on(library.blob_store.exists(&source_key))
                .unwrap()
        );
        drop(publication_guard);
        let republished = finished_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        publish_thread.join().unwrap();
        assert_eq!(republished.source_object_key, source_key.as_str());
        assert!(
            library
                .io
                .block_on(library.blob_store.exists(&source_key))
                .unwrap()
        );
        assert!(
            !db::blobs::is_unreferenced(
                &db::open_conn(library.database_path()).unwrap(),
                source_key.as_str(),
            )
            .unwrap()
        );

        // A delayed collector may still hold the pre-publication candidate.
        // Its fresh database check must retain both the bytes and metadata.
        library.io.block_on(reclaim_unreferenced_blobs(
            library.database_path(),
            &library.blob_store,
            &publication,
            candidates,
            "测试对象",
        ));
        assert!(
            library
                .io
                .block_on(library.blob_store.exists(&source_key))
                .unwrap()
        );
        assert!(
            db::blobs::get(
                &db::open_conn(library.database_path()).unwrap(),
                source_key.as_str(),
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn stale_reader_clone_persists_unit_progress_against_the_current_revision() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        let mut writer = LibraryStore::load_from(data_dir).unwrap();
        let book = writer.create_book("并发阅读进度", "作者").unwrap();
        let unit_id = writer.document(&book.id).unwrap().units[0].id.clone();
        let mut stale_unit_reader = writer.clone();

        let updated = writer
            .update_content_unit_source(&book.id, &unit_id, "<h1>新版本</h1><p>正文</p>")
            .unwrap();
        assert!(updated.revision > book.revision);

        stale_unit_reader
            .update_progress_at(&book.id, 0, &unit_id)
            .unwrap();
        let conn = db::open_conn(writer.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(progress.source_revision, updated.revision);
        assert_eq!(progress.content_unit_id.as_deref(), Some(unit_id.as_str()));
    }

    #[test]
    fn stale_reader_unit_progress_resolves_the_current_ordinal_after_reorder() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let (book, first_unit_id, _) = create_two_unit_book(&mut library);
        let incarnation = library.progress_incarnation(&book.id).unwrap();

        let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
        editor.move_unit(&first_unit_id, 1).unwrap();
        library.apply_document(editor.into_document()).unwrap();

        assert!(
            library
                .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 10)
                .unwrap()
        );
        let conn = db::open_conn(library.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(
            progress.content_unit_id.as_deref(),
            Some(first_unit_id.as_str())
        );
        assert_eq!(progress.spine_index, 1);
        let locator: DocumentLocator = serde_json::from_str(&progress.locator_json).unwrap();
        assert_eq!(locator.unit_id, first_unit_id);
        assert_eq!(library.book_record(&book.id).unwrap().last_spine, 1);
    }

    #[test]
    fn document_reorder_preserves_stable_progress_before_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("library");
        let mut library = LibraryStore::load_from(data_dir.clone()).unwrap();
        let (book, first_unit_id, _) = create_two_unit_book(&mut library);
        library
            .update_progress_at(&book.id, 0, &first_unit_id)
            .unwrap();

        let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
        editor.move_unit(&first_unit_id, 1).unwrap();
        library.apply_document(editor.into_document()).unwrap();

        let conn = db::open_conn(library.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(
            progress.content_unit_id.as_deref(),
            Some(first_unit_id.as_str())
        );
        assert_eq!(progress.spine_index, 1);
        drop(conn);
        drop(library);

        let reopened = LibraryStore::load_from(data_dir).unwrap();
        assert_eq!(reopened.book_record(&book.id).unwrap().last_spine, 1);
    }

    #[test]
    fn deleted_progress_unit_falls_back_to_a_valid_current_unit() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
        library
            .update_progress_at(&book.id, 1, &second_unit_id)
            .unwrap();

        let mut editor = DocumentEditor::new(library.document(&book.id).unwrap()).unwrap();
        editor.remove_unit(&second_unit_id).unwrap();
        library.apply_document(editor.into_document()).unwrap();

        let conn = db::open_conn(library.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(
            progress.content_unit_id.as_deref(),
            Some(first_unit_id.as_str())
        );
        assert_eq!(progress.spine_index, 0);
        assert_eq!(library.book_record(&book.id).unwrap().last_spine, 0);
    }

    #[test]
    fn older_cross_window_progress_action_cannot_overwrite_a_newer_one() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
        let incarnation = library.progress_incarnation(&book.id).unwrap();

        assert!(
            library
                .update_progress_at_ordered(&book.id, 0, &second_unit_id, incarnation, 10)
                .unwrap()
        );
        // Repeating the same position is still a newer action and must advance
        // the guard; otherwise an in-flight action from another window can
        // arrive between the two sequence values and overwrite it.
        assert!(
            library
                .update_progress_at_ordered(&book.id, 0, &second_unit_id, incarnation, 30)
                .unwrap()
        );
        assert!(
            !library
                .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 20)
                .unwrap()
        );
        assert!(
            !library
                .update_progress_at_ordered(&book.id, 0, "missing-unit", incarnation, 50)
                .unwrap()
        );
        assert!(
            !library
                .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 40)
                .unwrap()
        );

        let conn = db::open_conn(library.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(
            progress.content_unit_id.as_deref(),
            Some(second_unit_id.as_str())
        );
        assert_eq!(progress.spine_index, 1);
        assert_eq!(library.book_record(&book.id).unwrap().last_spine, 1);
    }

    #[test]
    fn failed_newer_progress_does_not_block_an_older_success_or_a_later_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
        let incarnation = library.progress_incarnation(&book.id).unwrap();

        assert!(
            library
                .update_progress_at_ordered(&book.id, 1, &second_unit_id, incarnation, 10)
                .unwrap()
        );
        let conn = db::open_conn(library.database_path()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_progress_update
             BEFORE UPDATE ON progress
             BEGIN
                 SELECT RAISE(ABORT, 'test progress update failure');
             END;",
        )
        .unwrap();
        drop(conn);

        assert!(
            library
                .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 30)
                .is_err()
        );
        assert_eq!(
            library
                .progress_write_sequences
                .get(&(book.id.clone(), incarnation)),
            Some(&10)
        );

        let conn = db::open_conn(library.database_path()).unwrap();
        conn.execute_batch("DROP TRIGGER fail_progress_update")
            .unwrap();
        drop(conn);
        assert!(
            library
                .update_progress_at_ordered(&book.id, 0, &first_unit_id, incarnation, 20)
                .unwrap()
        );
        assert!(
            library
                .update_progress_at_ordered(&book.id, 1, &second_unit_id, incarnation, 40)
                .unwrap()
        );

        let conn = db::open_conn(library.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(
            progress.content_unit_id.as_deref(),
            Some(second_unit_id.as_str())
        );
        assert_eq!(progress.spine_index, 1);
        assert_eq!(
            library
                .progress_write_sequences
                .get(&(book.id.clone(), incarnation)),
            Some(&40)
        );
    }

    #[test]
    fn deleted_and_reimported_book_rejects_the_old_reader_incarnation() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let (book, first_unit_id, second_unit_id) = create_two_unit_book(&mut library);
        let old_incarnation = library.progress_incarnation(&book.id).unwrap();
        let document = library.document(&book.id).unwrap();
        let source_bytes = library.source_bytes(&book.id).unwrap();

        library.remove_book(&book.id).unwrap();
        let record = library
            .persist_document(
                ImportedBook {
                    document,
                    assets: Vec::new(),
                },
                &source_bytes,
                "application/epub+zip",
                Some("reimported.epub".to_string()),
                "normalized",
                None,
            )
            .unwrap();
        assert!(matches!(
            library.finish_committed_import(record),
            ImportOutcome::Added(_)
        ));
        let new_incarnation = library.progress_incarnation(&book.id).unwrap();
        assert_ne!(new_incarnation, old_incarnation);

        assert!(
            !library
                .update_progress_at_ordered(&book.id, 1, &second_unit_id, old_incarnation, 100,)
                .unwrap()
        );
        let conn = db::open_conn(library.database_path()).unwrap();
        let progress = db::progress::get(&conn, &book.id).unwrap().unwrap();
        assert_eq!(
            progress.content_unit_id.as_deref(),
            Some(first_unit_id.as_str())
        );
        assert_eq!(progress.spine_index, 0);
        drop(conn);

        // Sequence watermarks are scoped by incarnation, so the new reader is
        // not blocked even by a numerically lower action in this direct test.
        assert!(
            library
                .update_progress_at_ordered(&book.id, 1, &second_unit_id, new_incarnation, 1,)
                .unwrap()
        );
        assert_eq!(library.book_record(&book.id).unwrap().last_spine, 1);
    }

    #[test]
    fn created_books_keep_only_the_current_normalized_source() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
        let book = library.create_book("版本收敛", "作者").unwrap();
        let unit_id = library.document(&book.id).unwrap().units[0].id.clone();

        library
            .update_content_unit_source(&book.id, &unit_id, "<h1>第二版</h1><p>正文二</p>")
            .unwrap();
        let latest = library
            .update_content_unit_source(&book.id, &unit_id, "<h1>第三版</h1><p>正文三</p>")
            .unwrap();

        let conn = db::open_conn(library.database_path()).unwrap();
        let sources = db::book_sources::list_for_book(&conn, &book.id).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].revision, latest.revision);
        assert_eq!(sources[0].source_kind, "normalized");
    }
}
