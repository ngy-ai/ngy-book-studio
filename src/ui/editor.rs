use super::*;
use super::{
    ai_controller::AiSidebarController,
    ai_sidebar::{
        AiCitationNavigationTarget, AiCitationTextFocus, AiQuestionRequest, AiSidebarEvent,
        AiSourceLink, citation_dom_navigation_script,
    },
};
use gpui::{Pixels, PromptButton, PromptLevel};
use moye_epub_editor::{
    chat::ChatWindowKind,
    document::{
        AssetRef, AssetRole, Block, BookDocument, BookFormat, BookSource, ContentUnitKind, Inline,
        Revision, SourceKind, TocNode, deterministic_id,
    },
    editing::{DocumentEditor, NewContentUnit},
    library::{MediaDraft, MediaKind},
    markup::{ParsedSource, parse_source_for_unit, serialize_source, serialize_xhtml},
    media::{MediaBackend, MediaMetadata, MediaResponse, MediaService},
    services::{AppServices, LibraryMutation},
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

mod navigation;
use navigation::{editor_toc_rows, toc_indent_destination, toc_outdent_destination};

const EDITOR_SHELL_HOST: &str = "shell";
const EDITOR_CONTENT_HOST: &str = "content";
#[cfg(target_os = "windows")]
const EDITOR_SHELL_ORIGIN: &str = "http://epubeditor.shell";
#[cfg(not(target_os = "windows"))]
const EDITOR_SHELL_ORIGIN: &str = "epubeditor://shell";
const EDITOR_SHELL_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src http://epubeditor.content epubeditor://content data:; media-src http://epubeditor.content epubeditor://content data:; connect-src http://epubeditor.content epubeditor://content; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'";
const EDITOR_CONTENT_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; media-src 'self' data:; connect-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'";
const EDITOR_TRUSTED_SHELL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\" lang=\"zh-CN\"><head><meta charset=\"utf-8\"/><title>墨页富文本编辑器</title></head><body><main data-moye-editor-shell=\"trusted\"></main></body></html>";
const EDITOR_ASSET_PATH_PREFIX: &str = ".moye/assets/";
const MAX_EDITOR_ASSET_ID_BYTES: usize = 256;
const MAX_EDITOR_SELECTION_BYTES: usize = 32 * 1024;
const MAX_EDITOR_MEDIA_TITLE_CHARS: usize = 512;
const MAX_EDITOR_MEDIA_DESCRIPTION_CHARS: usize = 16 * 1024;
const MAX_EDITOR_ID_BYTES: usize = 4 * 1024;
const MAX_EDITOR_REQUEST_ID: u64 = (1_u64 << 53) - 1;
const EDITOR_LEFT_SIDEBAR_DEFAULT_WIDTH: f32 = 252.;
const EDITOR_LEFT_SIDEBAR_MIN_WIDTH: f32 = 200.;
const EDITOR_LEFT_SIDEBAR_MAX_WIDTH: f32 = 480.;
const EDITOR_CONTENT_MIN_WIDTH: f32 = 360.;
const EDITOR_RESIZE_HANDLE_WIDTH: f32 = 7.;

static NEXT_EDITOR_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorResizablePane {
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct EditorPaneResizeDrag(EditorResizablePane);

struct EditorPaneResizePreview;

impl Render for EditorPaneResizePreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn constrained_editor_pane_width(
    requested: Pixels,
    minimum: Pixels,
    maximum: Pixels,
    viewport_width: Pixels,
    opposite_width: Pixels,
) -> Pixels {
    let reserved =
        opposite_width + px(EDITOR_CONTENT_MIN_WIDTH) + px(EDITOR_RESIZE_HANDLE_WIDTH * 2.);
    let available_maximum = (viewport_width - reserved).max(minimum);
    requested.clamp(minimum, maximum.min(available_maximum))
}

fn editor_pane_width_from_pointer(
    pane: EditorResizablePane,
    pointer_x: Pixels,
    viewport_width: Pixels,
) -> Pixels {
    let half_handle = px(EDITOR_RESIZE_HANDLE_WIDTH / 2.);
    match pane {
        EditorResizablePane::Left => pointer_x - half_handle,
        EditorResizablePane::Right => viewport_width - pointer_x - half_handle,
    }
}

// The editor bundle is built and pinned under `web/editor`. Cargo consumes the
// checked-in artifact so normal Rust builds never require Node.js.
const EDITOR_INITIALIZATION_SCRIPT: &str = include_str!("../../assets/editor/prosemirror.js");

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorSearchHit {
    chapter_index: usize,
    chapter_title: String,
    snippet: String,
}

/// A session-local content-unit projection used by the editor WebView.
/// `spine_index == None` marks a newly added unit that has not been saved yet.
#[derive(Clone, Debug)]
pub struct EditorChapter {
    pub title: String,
    pub href: String,
    pub spine_index: Option<usize>,
    pub html: String,
}

/// Builds the editor's session-local view projection directly from the
/// canonical block model. Original EPUB/Office/Kindle containers are never
/// hydrated just to open an editor window, and their styles or scripts cannot
/// become an implicit rendering authority.
pub(super) fn editor_chapters_from_document(document: &BookDocument) -> Result<Vec<EditorChapter>> {
    document.validate().context("编辑器文档无效")?;
    document
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            let body = serialize_xhtml(&unit.document)
                .with_context(|| format!("无法生成内容单元预览：{}", unit.title))?;
            Ok(EditorChapter {
                title: unit.title.clone(),
                // This URL is only a versioned WebView identity. Persistence,
                // TOC and citations continue to use the stable content-unit ID.
                href: format!("chapter-{}.xhtml", index + 1),
                spine_index: Some(index),
                html: editor_document_shell(&unit.title, &body),
            })
        })
        .collect()
}

#[derive(Clone, Debug)]
struct EditorUnitState {
    id: String,
    kind: ContentUnitKind,
    source_kind: SourceKind,
    source: String,
}

impl EditorUnitState {
    fn from_document(document: &BookDocument) -> Vec<Self> {
        document
            .units
            .iter()
            .map(|unit| Self {
                id: unit.id.clone(),
                kind: unit.kind,
                source_kind: unit.source_kind,
                source: unit.source.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorMediaBlock {
    id: String,
    kind: MediaKind,
    label: String,
    title: Option<String>,
    description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorMediaMetadataDraft {
    title: Option<String>,
    description: Option<String>,
}

impl EditorMediaMetadataDraft {
    fn from_inputs(title: &str, description: &str) -> Result<Self> {
        Ok(Self {
            title: normalize_editor_media_text(
                title,
                "媒体标题",
                MAX_EDITOR_MEDIA_TITLE_CHARS,
                false,
            )?,
            description: normalize_editor_media_text(
                description,
                "媒体说明",
                MAX_EDITOR_MEDIA_DESCRIPTION_CHARS,
                true,
            )?,
        })
    }
}

fn normalize_editor_media_text(
    value: &str,
    label: &str,
    max_chars: usize,
    multiline: bool,
) -> Result<Option<String>> {
    let value = value.trim();
    anyhow::ensure!(
        value.chars().count() <= max_chars,
        "{label}超过 {max_chars} 个字符"
    );
    anyhow::ensure!(
        !value.chars().any(|character| {
            character.is_control() && !(multiline && matches!(character, '\n' | '\r' | '\t'))
        }),
        "{label}包含不允许的控制字符"
    );
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn read_editor_media_file(path: &std::path::Path, kind: MediaKind) -> Result<Vec<u8>> {
    use std::io::Read as _;

    let metadata = std::fs::metadata(path)
        .with_context(|| format!("无法读取媒体文件信息：{}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "所选媒体不是普通文件");
    anyhow::ensure!(metadata.len() > 0, "媒体文件不能为空");
    anyhow::ensure!(
        metadata.len() <= kind.max_bytes(),
        "媒体文件超过 {} 字节安全上限",
        kind.max_bytes()
    );

    // Re-apply the limit while reading so a file that grows after the metadata
    // check still cannot make the process allocate beyond the accepted bound.
    let file = std::fs::File::open(path)
        .with_context(|| format!("无法打开媒体文件：{}", path.display()))?;
    let initial_capacity = usize::try_from(metadata.len().min(8 * 1024 * 1024)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(kind.max_bytes() + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("无法读取媒体文件：{}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= kind.max_bytes(),
        "媒体文件超过 {} 字节安全上限",
        kind.max_bytes()
    );
    Ok(bytes)
}

#[derive(Clone)]
struct PreparedEditorMedia {
    draft: MediaDraft,
    asset: AssetRef,
}

impl PreparedEditorMedia {
    fn new(draft: MediaDraft) -> Self {
        let role = match draft.kind {
            MediaKind::Image => AssetRole::ContentImage,
            MediaKind::Audio => AssetRole::Audio,
            MediaKind::Video => AssetRole::Video,
        };
        let asset = AssetRef::from_bytes(
            role,
            draft.media_type.clone(),
            draft.original_file_name.clone(),
            draft.bytes().as_slice(),
        );
        Self { draft, asset }
    }
}

#[derive(Clone)]
enum EditorMediaModalAction {
    InsertOrReplace {
        media: PreparedEditorMedia,
        replace_block_id: Option<String>,
    },
    Edit {
        target: EditorMediaBlock,
    },
}

#[derive(Clone)]
struct EditorMediaModal {
    action: EditorMediaModalAction,
    kind: MediaKind,
    title_input: Entity<InputState>,
    description_input: Entity<InputState>,
}

/// Which editor view is active for the current chapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EditorTab {
    /// Raw XHTML source in a plain text editor.
    #[default]
    Source,
    /// Read-only rendered view of the chapter.
    Preview,
    /// WYSIWYG editing via a contenteditable webview.
    RichText,
}

impl EditorTab {
    fn uses_webview(self) -> bool {
        self != Self::Source
    }
}

/// Body snapshot streamed back from the rich-text WebView via `window.ipc`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorIpcMessage {
    session_id: String,
    chapter_id: String,
    href: String,
    revision: u64,
    #[serde(default)]
    request_id: Option<u64>,
    #[serde(default)]
    body: Option<String>,
    selected_text: String,
    #[serde(default)]
    too_large: bool,
    #[serde(default)]
    ready: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorCitationNavigationResult {
    #[serde(rename = "type")]
    kind: String,
    session_id: String,
    chapter_id: String,
    href: String,
    revision: u64,
    request_id: u64,
    found: bool,
    reason: String,
}

#[derive(Clone, Debug)]
pub(super) struct EditorIpcUpdate {
    session_id: String,
    chapter_id: String,
    href: String,
    revision: u64,
    request_id: Option<u64>,
    html: String,
    selected_text: Option<String>,
    too_large: bool,
    ready: bool,
    /// `true` when the rich-text page reported an actual document change.
    /// Used to distinguish real edits from "no change" snapshots that just
    /// echo the current page body.
    edited: bool,
}

#[derive(Clone, Debug)]
pub(super) enum EditorWebEvent {
    Ipc(EditorIpcUpdate),
    PageLoaded(String),
    CitationNavigationResult {
        session_id: String,
        chapter_id: String,
        href: String,
        revision: u64,
        request_id: u64,
        found: bool,
        reason: String,
    },
}

#[derive(Clone, Debug)]
struct EditorWebPage {
    chapter_id: String,
    href: String,
    html: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveEditorPage {
    session_id: String,
    chapter_id: String,
    href: String,
    revision: u64,
    ready: bool,
}

impl ActiveEditorPage {
    fn is_unready_match(
        &self,
        session_id: &str,
        chapter_id: &str,
        revision: u64,
        href: &str,
    ) -> bool {
        !self.ready
            && self.session_id == session_id
            && self.chapter_id == chapter_id
            && self.revision == revision
            && editor_hrefs_match(&self.href, href)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingEditorAction {
    SwitchTab(EditorTab),
    SelectChapter(usize),
    SelectToc,
    AddChapter,
    RemoveChapter,
    MoveChapterUp,
    MoveChapterDown,
    IndentToc,
    OutdentToc,
    CycleUnitKind,
    SetSourceKind(SourceKind),
    InsertMedia(MediaKind),
    ReplaceMedia,
    EditMediaMetadata,
    DeleteMedia,
    Save,
    Export,
    AskAi,
    OpenCitation(usize),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingEditorCitationNavigation {
    session_id: String,
    chapter_id: String,
    href: String,
    revision: u64,
    request_id: u64,
    focus: AiCitationTextFocus,
}

fn merge_pending_editor_action(
    current: PendingEditorAction,
    next: PendingEditorAction,
    pending_export_path: &mut Option<PathBuf>,
) -> PendingEditorAction {
    let merged = if current == PendingEditorAction::Close || next == PendingEditorAction::Close {
        PendingEditorAction::Close
    } else if current == PendingEditorAction::AskAi || next == PendingEditorAction::AskAi {
        PendingEditorAction::AskAi
    } else {
        next
    };
    if merged != PendingEditorAction::Export {
        *pending_export_path = None;
    }
    merged
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingEditorSnapshot {
    session_id: String,
    chapter_id: String,
    href: String,
    revision: u64,
    request_id: u64,
    action: PendingEditorAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorWriteIntent {
    Save,
    Export,
    Close,
}

#[derive(Clone, Debug)]
struct ActiveEditorWrite {
    id: u64,
    intent: EditorWriteIntent,
    draft_generation: u64,
    title: String,
    target_display: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorWriteFollowUp {
    StayOpen,
    Close,
    SaveThenClose,
}

fn editor_write_follow_up(
    operation: &ActiveEditorWrite,
    close_requested: bool,
    current_generation: u64,
    save_succeeded: bool,
    export_succeeded: bool,
) -> EditorWriteFollowUp {
    if !save_succeeded
        || (operation.intent == EditorWriteIntent::Export && !export_succeeded)
        || !(close_requested || operation.intent == EditorWriteIntent::Close)
    {
        return EditorWriteFollowUp::StayOpen;
    }
    if current_generation == operation.draft_generation {
        EditorWriteFollowUp::Close
    } else {
        EditorWriteFollowUp::SaveThenClose
    }
}

struct EditorWriteJob {
    document: BookDocument,
    new_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    export_target: Option<PathBuf>,
}

enum EditorExportOutcome {
    NotRequested,
    Exported(u64),
    Failed(String),
}

impl EditorExportOutcome {
    fn succeeded(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }
}

struct EditorWriteWorkerResult {
    document: BookDocument,
    cover_bytes: Option<Arc<Vec<u8>>>,
    export: EditorExportOutcome,
    #[cfg(test)]
    worker_thread_id: std::thread::ThreadId,
}

fn spawn_editor_write(
    services: &Arc<AppServices>,
    job: EditorWriteJob,
) -> tokio::task::JoinHandle<Result<LibraryMutation<EditorWriteWorkerResult>>> {
    services.spawn_library_projected(move |library| {
        let EditorWriteJob {
            document,
            new_asset_bytes,
            export_target,
        } = job;
        let book_id = document.id.clone();
        // Keep the authoritative UI projection from the exact validated draft.
        // `apply_document_with_assets` only advances document/unit revisions;
        // reconstructing that small change avoids a fallible post-commit read
        // being misreported as a failed save.
        let mut saved_document = document.clone();
        let record = library.apply_document_with_assets(document, new_asset_bytes)?;
        saved_document.revision = Revision::new(record.revision);
        for unit in &mut saved_document.units {
            unit.revision = saved_document.revision;
        }
        let cover_bytes = library.cover_bytes_cached(&book_id);
        let export = match export_target {
            Some(target) => match library.export_epub(&book_id, &target) {
                Ok(bytes_written) => EditorExportOutcome::Exported(bytes_written),
                Err(error) => EditorExportOutcome::Failed(format!("{error:#}")),
            },
            None => EditorExportOutcome::NotRequested,
        };
        Ok(EditorWriteWorkerResult {
            document: saved_document,
            cover_bytes,
            export,
            #[cfg(test)]
            worker_thread_id: std::thread::current().id(),
        })
    })
}

fn apply_editor_library_projection(
    library: &mut LibraryStore,
    applied_generation: &mut u64,
    generation: u64,
    snapshot: LibraryStore,
) -> bool {
    if generation < *applied_generation {
        return false;
    }
    library.merge_cached_projection(snapshot);
    *applied_generation = generation;
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditorWebViewBuildGate {
    building: bool,
    close_requested: bool,
}

impl EditorWebViewBuildGate {
    fn new(building: bool) -> Self {
        Self {
            building,
            close_requested: false,
        }
    }

    /// Returns true when native close must be vetoed until the child WebView
    /// builder has stopped using the copied parent HWND.
    fn request_close(&mut self) -> bool {
        if self.building {
            self.close_requested = true;
            true
        } else {
            false
        }
    }

    /// Marks the asynchronous build settled and consumes a deferred close.
    fn finish(&mut self) -> bool {
        self.building = false;
        std::mem::take(&mut self.close_requested)
    }
}

#[derive(Default)]
struct EditorWebPages {
    pages: BTreeMap<u64, EditorWebPage>,
}

#[derive(Clone, Debug)]
struct EditorMediaAsset {
    media_type: String,
    byte_len: u64,
    content_hash: String,
    /// Only unsaved assets live in memory. Persisted assets retain their
    /// authorization metadata here and are streamed through AppServices.
    bytes: Option<Arc<Vec<u8>>>,
}

impl EditorMediaAsset {
    #[cfg(test)]
    fn checked(metadata: &AssetRef, bytes: Arc<Vec<u8>>) -> Result<Self> {
        metadata.validate().context("编辑器媒体元数据无效")?;
        ensure_editor_asset_id(&metadata.id)?;
        anyhow::ensure!(
            metadata.byte_len == bytes.len() as u64,
            "编辑器媒体长度与元数据不匹配"
        );
        anyhow::ensure!(
            metadata.content_hash == blake3::hash(bytes.as_slice()).to_hex().to_string(),
            "编辑器媒体内容哈希与元数据不匹配"
        );
        Ok(Self::prevalidated(metadata, bytes))
    }

    fn authorized(metadata: &AssetRef) -> Result<Self> {
        metadata.validate().context("编辑器媒体元数据无效")?;
        ensure_editor_asset_id(&metadata.id)?;
        Ok(Self {
            media_type: metadata.media_type.clone(),
            byte_len: metadata.byte_len,
            content_hash: metadata.content_hash.clone(),
            bytes: None,
        })
    }

    /// The caller created `metadata` from these exact bytes on a background
    /// task. Keep the cheap structural checks here so installing a freshly
    /// selected video never hashes a potentially large object on the GPUI
    /// thread for a second time.
    fn prevalidated(metadata: &AssetRef, bytes: Arc<Vec<u8>>) -> Self {
        debug_assert_eq!(metadata.byte_len, bytes.len() as u64);
        Self {
            media_type: metadata.media_type.clone(),
            byte_len: metadata.byte_len,
            content_hash: metadata.content_hash.clone(),
            bytes: Some(bytes),
        }
    }

    fn matches(&self, metadata: &AssetRef) -> bool {
        self.media_type == metadata.media_type
            && self.byte_len == metadata.byte_len
            && self.content_hash == metadata.content_hash
    }
}

#[derive(Clone)]
struct EditorMediaSnapshot {
    book_id: Arc<str>,
    assets: Arc<HashMap<String, EditorMediaAsset>>,
}

impl MediaBackend for EditorMediaSnapshot {
    fn metadata(&self, book_id: &str, asset_id: &str) -> Result<MediaMetadata> {
        anyhow::ensure!(book_id == self.book_id.as_ref(), "资源不属于指定图书");
        let asset = self.assets.get(asset_id).context("资源不存在")?;
        Ok(MediaMetadata {
            media_type: asset.media_type.clone(),
            byte_len: asset.byte_len,
        })
    }

    fn read(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
        anyhow::ensure!(book_id == self.book_id.as_ref(), "资源不属于指定图书");
        Ok(self
            .assets
            .get(asset_id)
            .context("资源不存在")?
            .bytes
            .as_ref()
            .context("持久媒体必须通过异步对象存储读取")?
            .as_ref()
            .clone())
    }

    fn read_range(
        &self,
        book_id: &str,
        asset_id: &str,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>> {
        anyhow::ensure!(book_id == self.book_id.as_ref(), "资源不属于指定图书");
        let bytes = self
            .assets
            .get(asset_id)
            .context("资源不存在")?
            .bytes
            .as_ref()
            .context("持久媒体必须通过异步对象存储读取")?;
        let start = usize::try_from(range.start).context("媒体范围起点过大")?;
        let end = usize::try_from(range.end).context("媒体范围终点过大")?;
        Ok(bytes.get(start..end).context("媒体范围越界")?.to_vec())
    }
}

/// Versioned chapter snapshots served by the editor custom protocol. Keeping
/// a short history prevents an in-flight navigation from receiving another
/// chapter's HTML when users switch tabs or chapters quickly.
#[derive(Clone)]
pub struct EditorWebState {
    book_id: Arc<str>,
    session_id: Arc<str>,
    pages: Arc<Mutex<EditorWebPages>>,
    media: Arc<Mutex<Arc<HashMap<String, EditorMediaAsset>>>>,
    /// Held across `responder.respond` and across the close transition. This
    /// makes the final open check atomic with respect to WebView release.
    protocol_open: Arc<Mutex<bool>>,
}

impl EditorWebState {
    pub(super) fn new(book_id: String, href: String, html: String) -> Self {
        let session_sequence = NEXT_EDITOR_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("editor-session-{}-{session_sequence}", std::process::id());
        // `EditorApp::new` replaces this bootstrap identity with the stable
        // content-unit ID before the WebView is built. Keeping a valid,
        // deterministic placeholder also makes the protocol state safe to use
        // in focused tests without weakening the four-part IPC contract.
        let bootstrap_chapter_id = deterministic_id(
            "editor-bootstrap-unit",
            format!("{book_id}\0{href}").as_bytes(),
        );
        let state = Self {
            book_id: Arc::from(book_id),
            session_id: Arc::from(session_id),
            pages: Arc::new(Mutex::new(EditorWebPages::default())),
            media: Arc::new(Mutex::new(Arc::new(HashMap::new()))),
            protocol_open: Arc::new(Mutex::new(true)),
        };
        state.set(0, bootstrap_chapter_id, href, html);
        state
    }

    fn session_id(&self) -> &str {
        self.session_id.as_ref()
    }

    /// Installs only the immutable authorization metadata referenced by the
    /// canonical document. Persisted bytes remain in object storage until the
    /// asynchronous protocol handler requests a full object or exact range.
    pub(super) fn authorize_media(&self, document: &BookDocument) -> Result<()> {
        self.ensure_document_owner(document)?;
        document.validate().context("编辑器文档无效")?;
        let mut assets = HashMap::new();
        for asset_id in editor_media_asset_ids(document) {
            let metadata = document
                .find_asset(&asset_id)
                .with_context(|| format!("编辑器媒体元数据不存在：{asset_id}"))?;
            assets.insert(asset_id, EditorMediaAsset::authorized(metadata)?);
        }
        *self.media.lock().unwrap() = Arc::new(assets);
        Ok(())
    }

    /// Atomically installs a freshly selected, already hashed object and drops
    /// every object no longer referenced by the current canonical document.
    /// This is memory-only and is safe to call while updating GPUI state.
    fn sync_media(
        &self,
        document: &BookDocument,
        prepared: Option<(&AssetRef, Arc<Vec<u8>>)>,
    ) -> Result<()> {
        self.ensure_document_owner(document)?;
        let current = self.media_snapshot().assets;
        let mut candidates = current.as_ref().clone();
        if let Some((metadata, bytes)) = prepared {
            metadata.validate().context("编辑器媒体元数据无效")?;
            ensure_editor_asset_id(&metadata.id)?;
            anyhow::ensure!(
                metadata.byte_len == bytes.len() as u64,
                "编辑器媒体长度与元数据不匹配"
            );
            let canonical = document
                .find_asset(&metadata.id)
                .context("新媒体不在当前图书文档中")?;
            anyhow::ensure!(
                canonical.media_type == metadata.media_type
                    && canonical.byte_len == metadata.byte_len
                    && canonical.content_hash == metadata.content_hash,
                "新媒体与当前图书文档不匹配"
            );
            candidates.insert(
                metadata.id.clone(),
                EditorMediaAsset::prevalidated(canonical, bytes),
            );
        }

        let mut retained = HashMap::new();
        for asset_id in editor_media_asset_ids(document) {
            ensure_editor_asset_id(&asset_id)?;
            let metadata = document
                .find_asset(&asset_id)
                .with_context(|| format!("编辑器媒体元数据不存在：{asset_id}"))?;
            metadata.validate().context("编辑器媒体元数据无效")?;
            let asset = match candidates.get(&asset_id) {
                Some(asset) if asset.matches(metadata) => asset.clone(),
                Some(_) => anyhow::bail!("编辑器媒体快照与文档不匹配"),
                None => EditorMediaAsset::authorized(metadata)?,
            };
            retained.insert(asset_id, asset);
        }
        *self.media.lock().unwrap() = Arc::new(retained);
        Ok(())
    }

    fn ensure_document_owner(&self, document: &BookDocument) -> Result<()> {
        anyhow::ensure!(
            document.id == self.book_id.as_ref(),
            "编辑器媒体快照不属于当前图书"
        );
        Ok(())
    }

    fn media_snapshot(&self) -> EditorMediaSnapshot {
        EditorMediaSnapshot {
            book_id: Arc::clone(&self.book_id),
            assets: Arc::clone(&self.media.lock().unwrap()),
        }
    }

    fn protocol_is_open(&self) -> bool {
        *self.protocol_open.lock().unwrap()
    }

    fn close_protocol(&self) {
        *self.protocol_open.lock().unwrap() = false;
    }

    fn set(&self, revision: u64, chapter_id: String, href: String, html: String) {
        let mut state = self.pages.lock().unwrap();
        state.pages.insert(
            revision,
            EditorWebPage {
                chapter_id,
                href,
                html,
            },
        );
        while state.pages.len() > EDITOR_PAGE_HISTORY {
            let Some(oldest) = state.pages.keys().next().copied() else {
                break;
            };
            state.pages.remove(&oldest);
        }
    }

    fn page(&self, revision: u64) -> Option<EditorWebPage> {
        self.pages.lock().unwrap().pages.get(&revision).cloned()
    }

    fn apply_message(&self, message: EditorIpcMessage) -> Option<EditorIpcUpdate> {
        if message.session_id != self.session_id.as_ref()
            || !valid_editor_ipc_id(&message.chapter_id)
        {
            return None;
        }
        let selected_text = normalize_editor_selection(&message.selected_text)?;
        let media = self.media_snapshot();
        let mut state = self.pages.lock().unwrap();
        let page = state.pages.get_mut(&message.revision)?;
        if page.chapter_id != message.chapter_id || !editor_hrefs_match(&page.href, &message.href) {
            return None;
        }
        let href = page.href.clone();
        if message.ready {
            return Some(EditorIpcUpdate {
                session_id: message.session_id,
                chapter_id: message.chapter_id,
                href,
                revision: message.revision,
                request_id: message.request_id,
                html: page.html.clone(),
                selected_text,
                too_large: false,
                ready: true,
                edited: false,
            });
        }
        if message.too_large
            || message
                .body
                .as_ref()
                .is_some_and(|body| body.len() > MAX_EDITOR_BODY_BYTES)
        {
            return Some(EditorIpcUpdate {
                session_id: message.session_id,
                chapter_id: message.chapter_id,
                href,
                revision: message.revision,
                request_id: message.request_id,
                html: page.html.clone(),
                selected_text,
                too_large: true,
                ready: false,
                edited: false,
            });
        }
        let body_was_some = message.body.is_some();
        let html = match message.body {
            Some(body) => {
                let body = canonicalize_editor_asset_urls(&body, &media.assets);
                replace_body_element(&page.html, &body)?
            }
            None => page.html.clone(),
        };
        page.html = html.clone();
        Some(EditorIpcUpdate {
            session_id: message.session_id,
            chapter_id: message.chapter_id,
            href,
            revision: message.revision,
            request_id: message.request_id,
            html,
            selected_text,
            too_large: false,
            ready: false,
            edited: body_was_some,
        })
    }
}

fn normalize_editor_selection(value: &str) -> Option<Option<String>> {
    if value.len() > MAX_EDITOR_SELECTION_BYTES {
        return None;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    Some((!normalized.is_empty()).then_some(normalized))
}

fn valid_editor_ipc_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EDITOR_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn parse_editor_ipc_message(body: &str) -> Option<EditorIpcMessage> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let object = value.as_object()?;
    // `Option<u64>` deliberately distinguishes explicit `null` (automatic
    // snapshot/Ready) from an exact host request, but the key itself is part
    // of the versioned contract and cannot be omitted by legacy senders.
    if !object.contains_key("request_id") {
        return None;
    }
    let message = serde_json::from_value::<EditorIpcMessage>(value).ok()?;
    let request_id_valid = message
        .request_id
        .is_none_or(|request_id| (1..=MAX_EDITOR_REQUEST_ID).contains(&request_id));
    let shape_valid = if message.ready {
        message.request_id.is_none() && message.body.is_none() && !message.too_large
    } else {
        !message.too_large || message.body.is_none()
    };
    (valid_editor_ipc_id(&message.session_id)
        && valid_editor_ipc_id(&message.chapter_id)
        && request_id_valid
        && shape_valid)
        .then_some(message)
}

fn parse_editor_citation_navigation_result(body: &str) -> Option<EditorCitationNavigationResult> {
    let message = serde_json::from_str::<EditorCitationNavigationResult>(body).ok()?;
    (message.kind == "citation_navigation_result"
        && valid_editor_ipc_id(&message.session_id)
        && valid_editor_ipc_id(&message.chapter_id)
        && (1..=MAX_EDITOR_REQUEST_ID).contains(&message.request_id)
        && message.reason.len() <= 64)
        .then_some(message)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorDocumentRequest {
    kind: EditorDocumentKind,
    session_id: String,
    chapter_id: String,
    revision: u64,
    href: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorDocumentKind {
    TrustedShell,
    Preview,
    RichSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorOrigin {
    Shell,
    Content,
}

fn editor_query_value<'a>(uri: &'a gpui_component::wry::http::Uri, key: &str) -> Option<&'a str> {
    let mut matches = uri.query()?.split('&').filter_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then_some(value)
    });
    let value = matches.next()?;
    matches.next().is_none().then_some(value)
}

fn editor_query_id(uri: &gpui_component::wry::http::Uri, key: &str) -> Option<String> {
    let decoded = urlencoding::decode(editor_query_value(uri, key)?).ok()?;
    valid_editor_ipc_id(&decoded).then(|| decoded.into_owned())
}

fn editor_origin(uri: &gpui_component::wry::http::Uri) -> Option<EditorOrigin> {
    let custom_origin = match (uri.scheme_str(), uri.host()) {
        (Some("epubeditor"), Some(EDITOR_SHELL_HOST)) => Some(EditorOrigin::Shell),
        (Some("epubeditor"), Some(EDITOR_CONTENT_HOST)) => Some(EditorOrigin::Content),
        _ => None,
    };
    if custom_origin.is_some() {
        return custom_origin;
    }
    #[cfg(target_os = "windows")]
    {
        match (uri.scheme_str(), uri.host()) {
            (Some("http"), Some("epubeditor.shell")) => Some(EditorOrigin::Shell),
            (Some("http"), Some("epubeditor.content")) => Some(EditorOrigin::Content),
            _ => None,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

fn normalized_editor_path(path: &str) -> Option<String> {
    let decoded = urlencoding::decode(path).ok()?;
    if decoded.contains('\\') || decoded.as_bytes().contains(&0) {
        return None;
    }
    Some(decoded.trim_start_matches('/').to_string())
}

fn ensure_editor_asset_id(asset_id: &str) -> Result<()> {
    anyhow::ensure!(
        !asset_id.is_empty()
            && asset_id.len() <= MAX_EDITOR_ASSET_ID_BYTES
            && asset_id != "."
            && asset_id != ".."
            && asset_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
            }),
        "编辑器媒体 ID 无效"
    );
    Ok(())
}

fn editor_media_asset_ids(document: &BookDocument) -> HashSet<String> {
    document
        .assets
        .iter()
        .filter(|asset| {
            [
                AssetRole::Cover,
                AssetRole::ContentImage,
                AssetRole::Poster,
                AssetRole::Audio,
                AssetRole::Video,
            ]
            .iter()
            .any(|role| asset.has_role(*role))
        })
        .map(|asset| asset.id.clone())
        .collect()
}

fn editor_asset_path(asset_id: &str) -> String {
    format!("/{EDITOR_ASSET_PATH_PREFIX}{asset_id}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EditorAssetRequest {
    NotAsset,
    Invalid,
    Asset(String),
}

fn editor_asset_request(uri: &gpui_component::wry::http::Uri) -> EditorAssetRequest {
    let Some(path) = normalized_editor_path(uri.path()) else {
        return if uri.path().contains(EDITOR_ASSET_PATH_PREFIX) {
            EditorAssetRequest::Invalid
        } else {
            EditorAssetRequest::NotAsset
        };
    };
    let Some(asset_id) = path.strip_prefix(EDITOR_ASSET_PATH_PREFIX) else {
        return EditorAssetRequest::NotAsset;
    };
    if uri.query().is_some() || ensure_editor_asset_id(asset_id).is_err() {
        return EditorAssetRequest::Invalid;
    }
    EditorAssetRequest::Asset(asset_id.to_string())
}

/// Rewrites only quoted `src` and `poster` attribute values while they are
/// inside a tag. Text nodes, other attributes, and unknown URLs are preserved
/// byte-for-byte.
fn rewrite_editor_resource_attributes(
    input: &str,
    mut replacement: impl FnMut(&str) -> Option<String>,
) -> String {
    fn attribute_at(bytes: &[u8], start: usize, name: &[u8]) -> bool {
        bytes
            .get(start..start + name.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
            && bytes
                .get(start + name.len())
                .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'=')
    }

    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied_until = 0usize;
    let mut cursor = 0usize;
    let mut in_tag = false;
    let mut quoted = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if !in_tag {
            if byte == b'<' {
                in_tag = true;
            }
            cursor += 1;
            continue;
        }
        if let Some(quote) = quoted {
            if byte == quote {
                quoted = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'>' {
            in_tag = false;
            cursor += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quoted = Some(byte);
            cursor += 1;
            continue;
        }

        let name_len = if attribute_at(bytes, cursor, b"src") {
            3
        } else if attribute_at(bytes, cursor, b"poster") {
            6
        } else {
            cursor += 1;
            continue;
        };
        let boundary = cursor == 0
            || bytes[cursor - 1].is_ascii_whitespace()
            || matches!(bytes[cursor - 1], b'<' | b'/');
        if !boundary {
            cursor += name_len;
            continue;
        }
        let mut value_start = cursor + name_len;
        while bytes
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        if bytes.get(value_start) != Some(&b'=') {
            cursor += name_len;
            continue;
        }
        value_start += 1;
        while bytes
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        let Some(quote @ (b'\'' | b'"')) = bytes.get(value_start).copied() else {
            cursor += name_len;
            continue;
        };
        value_start += 1;
        let Some(relative_end) = bytes[value_start..].iter().position(|byte| *byte == quote) else {
            break;
        };
        let value_end = value_start + relative_end;
        if let Some(value) = input.get(value_start..value_end)
            && let Some(rewritten) = replacement(value)
        {
            output.push_str(&input[copied_until..value_start]);
            output.push_str(&rewritten);
            copied_until = value_end;
        }
        cursor = value_end + 1;
    }
    output.push_str(&input[copied_until..]);
    output
}

fn rewrite_editor_asset_urls(html: &str, assets: &HashMap<String, EditorMediaAsset>) -> String {
    rewrite_editor_resource_attributes(html, |value| {
        let asset_id = value.strip_prefix("moye-asset:")?;
        if ensure_editor_asset_id(asset_id).is_err() || !assets.contains_key(asset_id) {
            return None;
        }
        Some(editor_asset_path(asset_id))
    })
}

fn canonicalize_editor_asset_urls(
    html: &str,
    assets: &HashMap<String, EditorMediaAsset>,
) -> String {
    rewrite_editor_resource_attributes(html, |value| {
        let relative_prefix = format!("/{EDITOR_ASSET_PATH_PREFIX}");
        let asset_id = if let Some(asset_id) = value.strip_prefix(&relative_prefix) {
            asset_id.to_string()
        } else {
            let uri = value.parse::<gpui_component::wry::http::Uri>().ok()?;
            if editor_origin(&uri) != Some(EditorOrigin::Content) || uri.query().is_some() {
                return None;
            }
            normalized_editor_path(uri.path())?
                .strip_prefix(EDITOR_ASSET_PATH_PREFIX)?
                .to_string()
        };
        if ensure_editor_asset_id(&asset_id).is_err() || !assets.contains_key(&asset_id) {
            return None;
        }
        Some(format!("moye-asset:{asset_id}"))
    })
}

fn editor_hrefs_match(left: &str, right: &str) -> bool {
    match (normalized_editor_path(left), normalized_editor_path(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn editor_ipc_matches_document_request(
    document: &EditorDocumentRequest,
    message: &EditorIpcMessage,
) -> bool {
    document.kind == EditorDocumentKind::TrustedShell
        && message.session_id == document.session_id
        && message.chapter_id == document.chapter_id
        && message.revision == document.revision
        && normalized_editor_path(&message.href).as_deref() == Some(document.href.as_str())
}

fn has_trusted_editor_shell_origin<B>(request: &gpui_component::wry::http::Request<B>) -> bool {
    request
        .headers()
        .get("Origin")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == EDITOR_SHELL_ORIGIN)
}

fn editor_document_request(uri: &gpui_component::wry::http::Uri) -> Option<EditorDocumentRequest> {
    let kind = match (editor_origin(uri)?, editor_query_value(uri, "mode")?) {
        (EditorOrigin::Shell, "edit") => EditorDocumentKind::TrustedShell,
        (EditorOrigin::Content, "preview") => EditorDocumentKind::Preview,
        (EditorOrigin::Content, "source") => EditorDocumentKind::RichSource,
        _ => return None,
    };
    let session_id = editor_query_id(uri, "session_id")?;
    let chapter_id = editor_query_id(uri, "chapter_id")?;
    let revision = editor_query_value(uri, "rev")?.parse().ok()?;
    let href = normalized_editor_path(uri.path())?;
    (!href.is_empty()).then_some(EditorDocumentRequest {
        kind,
        session_id,
        chapter_id,
        revision,
        href,
    })
}

fn editor_custom_url(
    tab: EditorTab,
    session_id: &str,
    chapter_id: &str,
    revision: u64,
    href: &str,
) -> String {
    let (host, mode) = match tab {
        EditorTab::RichText => (EDITOR_SHELL_HOST, "edit"),
        EditorTab::Preview => (EDITOR_CONTENT_HOST, "preview"),
        EditorTab::Source => unreachable!("source mode does not use the WebView"),
    };
    format!(
        "epubeditor://{host}/{}?mode={mode}&session_id={}&chapter_id={}&rev={revision}",
        href.trim_start_matches('/'),
        urlencoding::encode(session_id),
        urlencoding::encode(chapter_id),
    )
}

fn editor_navigation_url(
    tab: EditorTab,
    session_id: &str,
    chapter_id: &str,
    revision: u64,
    href: &str,
) -> String {
    let url = editor_custom_url(tab, session_id, chapter_id, revision, href);
    #[cfg(target_os = "windows")]
    {
        url.replacen("epubeditor://", "http://epubeditor.", 1)
    }
    #[cfg(not(target_os = "windows"))]
    {
        url
    }
}

fn is_editor_navigation_url(url: &str) -> bool {
    url.parse()
        .ok()
        .and_then(|uri| editor_document_request(&uri))
        .is_some_and(|request| {
            matches!(
                request.kind,
                EditorDocumentKind::TrustedShell | EditorDocumentKind::Preview
            )
        })
}

fn editor_error_response(
    status: u16,
    message: impl Into<String>,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    gpui_component::wry::http::Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Security-Policy", "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(message.into().into_bytes()))
        .expect("valid editor error response")
}

fn editor_media_response(
    response: MediaResponse,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    let mut builder = gpui_component::wry::http::Response::builder()
        .status(response.status)
        .header("Content-Type", response.media_type)
        .header("Accept-Ranges", response.accept_ranges)
        .header("Content-Length", response.content_length.to_string())
        .header("Content-Security-Policy", "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store");
    if let Some(content_range) = response.content_range {
        builder = builder.header("Content-Range", content_range);
    }
    builder
        .body(Cow::Owned(response.body))
        .expect("valid editor media response")
}

fn editor_owned_response(
    response: gpui_component::wry::http::Response<Cow<'static, [u8]>>,
) -> gpui_component::wry::http::Response<Vec<u8>> {
    let (parts, body) = response.into_parts();
    gpui_component::wry::http::Response::from_parts(parts, body.into_owned())
}

fn spawn_persisted_editor_media(
    services: &Arc<AppServices>,
    book_id: String,
    asset_id: String,
    expected: EditorMediaAsset,
    range_header: Option<String>,
) -> tokio::task::JoinHandle<Result<MediaResponse>> {
    services.spawn_library_read(move |library| {
        anyhow::ensure!(expected.bytes.is_none(), "未保存媒体应从内存快照读取");
        let (media_type, byte_len) = library.asset_metadata(&book_id, &asset_id)?;
        anyhow::ensure!(
            media_type == expected.media_type && byte_len == expected.byte_len,
            "持久媒体与编辑器授权快照不匹配"
        );
        MediaService::new((*library).clone()).serve(&book_id, &asset_id, range_header.as_deref())
    })
}

fn spawn_editor_media(
    services: &Arc<AppServices>,
    snapshot: EditorMediaSnapshot,
    book_id: String,
    asset_id: String,
    expected: EditorMediaAsset,
    range_header: Option<String>,
) -> tokio::task::JoinHandle<Result<MediaResponse>> {
    if expected.bytes.is_none() {
        return spawn_persisted_editor_media(services, book_id, asset_id, expected, range_header);
    }

    services.runtime().handle().spawn_blocking(move || {
        let authorized = snapshot
            .assets
            .get(&asset_id)
            .context("编辑器媒体授权已撤销")?;
        anyhow::ensure!(
            authorized.media_type == expected.media_type
                && authorized.byte_len == expected.byte_len
                && authorized.content_hash == expected.content_hash
                && authorized.bytes.is_some(),
            "未保存媒体与编辑器授权快照不匹配"
        );
        MediaService::new(snapshot).serve(&book_id, &asset_id, range_header.as_deref())
    })
}

fn editor_protocol_response<B>(
    state: &EditorWebState,
    request: &gpui_component::wry::http::Request<B>,
) -> gpui_component::wry::http::Response<Cow<'static, [u8]>> {
    let uri = request.uri();
    let Some(origin) = editor_origin(uri) else {
        return editor_error_response(403, "拒绝非编辑器来源");
    };

    if let Some(document) = editor_document_request(uri) {
        if document.kind == EditorDocumentKind::RichSource
            && !has_trusted_editor_shell_origin(request)
        {
            return editor_error_response(403, "正文快照只允许受信编辑器读取");
        }
        if document.session_id != state.session_id() {
            return editor_error_response(404, "编辑会话已过期");
        }
        let Some(page) = state.page(document.revision) else {
            return editor_error_response(404, "编辑页面已过期");
        };
        if page.chapter_id != document.chapter_id
            || normalized_editor_path(&page.href).as_deref() != Some(document.href.as_str())
        {
            return editor_error_response(404, "编辑页面与章节不匹配");
        }
        let (html, csp) = match document.kind {
            EditorDocumentKind::TrustedShell => {
                (EDITOR_TRUSTED_SHELL.to_string(), EDITOR_SHELL_CSP)
            }
            EditorDocumentKind::Preview | EditorDocumentKind::RichSource => {
                let media = state.media_snapshot();
                (
                    rewrite_editor_asset_urls(&page.html, &media.assets),
                    EDITOR_CONTENT_CSP,
                )
            }
        };
        let content_length = html.len().to_string();
        let mut response = gpui_component::wry::http::Response::builder()
            .status(200)
            .header("Content-Type", "application/xhtml+xml; charset=utf-8")
            .header("Content-Length", content_length)
            .header("Content-Security-Policy", csp)
            .header("X-Content-Type-Options", "nosniff")
            .header("Cache-Control", "no-store")
            .header("Referrer-Policy", "no-referrer");
        if document.kind == EditorDocumentKind::RichSource {
            response = response
                .header("Access-Control-Allow-Origin", EDITOR_SHELL_ORIGIN)
                .header("Cross-Origin-Resource-Policy", "cross-origin")
                .header("Vary", "Origin");
        } else {
            response = response.header("Cross-Origin-Resource-Policy", "same-origin");
        }
        return response
            .body(Cow::Owned(html.into_bytes()))
            .expect("valid editor document response");
    }

    if origin != EditorOrigin::Content {
        return editor_error_response(404, "编辑器资源不存在");
    }
    match editor_asset_request(uri) {
        EditorAssetRequest::Asset(asset_id) => {
            let range_header = request
                .headers()
                .get("Range")
                .map(|value| value.to_str().unwrap_or("invalid"));
            let snapshot = state.media_snapshot();
            let response = match MediaService::new(snapshot.clone()).serve(
                snapshot.book_id.as_ref(),
                &asset_id,
                range_header,
            ) {
                Ok(response) => response,
                Err(_) => return editor_error_response(404, "编辑器媒体不存在"),
            };
            return editor_media_response(response);
        }
        EditorAssetRequest::Invalid => {
            return editor_error_response(404, "编辑器媒体不存在");
        }
        EditorAssetRequest::NotAsset => {}
    }

    editor_error_response(404, "编辑器资源不存在")
}

fn find_xml_sequence(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn xml_tag_end(bytes: &[u8], start: usize, track_brackets: bool) -> Option<usize> {
    let mut quote = None;
    let mut bracket_depth = 0usize;
    for (offset, byte) in bytes.get(start..)?.iter().copied().enumerate() {
        let index = start + offset;
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'[' if track_brackets => bracket_depth += 1,
            b']' if track_brackets => bracket_depth = bracket_depth.saturating_sub(1),
            b'>' if bracket_depth == 0 => return Some(index + 1),
            _ => {}
        }
    }
    None
}

fn xml_body_element_range(xml: &str) -> Option<(usize, usize)> {
    let bytes = xml.as_bytes();
    let mut cursor = 0usize;
    let mut body_start = None;
    let mut body_depth = 0usize;

    while cursor < bytes.len() {
        let start = cursor + bytes.get(cursor..)?.iter().position(|byte| *byte == b'<')?;
        if bytes.get(start..)?.starts_with(b"<!--") {
            cursor = find_xml_sequence(bytes, start + 4, b"-->")? + 3;
            continue;
        }
        if bytes.get(start..)?.starts_with(b"<![CDATA[") {
            cursor = find_xml_sequence(bytes, start + 9, b"]]>")? + 3;
            continue;
        }
        if bytes.get(start..)?.starts_with(b"<?") {
            cursor = find_xml_sequence(bytes, start + 2, b"?>")? + 2;
            continue;
        }
        if bytes.get(start..)?.starts_with(b"<!") {
            cursor = xml_tag_end(bytes, start + 2, true)?;
            continue;
        }

        let mut name_cursor = start + 1;
        let closing = bytes.get(name_cursor) == Some(&b'/');
        if closing {
            name_cursor += 1;
        }
        while bytes.get(name_cursor).is_some_and(u8::is_ascii_whitespace) {
            name_cursor += 1;
        }
        let name_start = name_cursor;
        while bytes
            .get(name_cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>'))
        {
            name_cursor += 1;
        }
        if name_cursor == name_start {
            cursor = start + 1;
            continue;
        }
        let end = xml_tag_end(bytes, name_cursor, false)?;
        let name = &bytes[name_start..name_cursor];
        let local_name = name.rsplit(|byte| *byte == b':').next().unwrap_or(name);
        let is_body = local_name.eq_ignore_ascii_case(b"body");
        let mut before_end = end.saturating_sub(2);
        while before_end > start && bytes[before_end].is_ascii_whitespace() {
            before_end -= 1;
        }
        let self_closing = bytes.get(before_end) == Some(&b'/');

        if is_body {
            if closing {
                if body_depth == 0 {
                    return None;
                }
                body_depth -= 1;
                if body_depth == 0 {
                    return Some((body_start?, end));
                }
            } else if self_closing {
                if body_depth == 0 {
                    return Some((start, end));
                }
            } else {
                if body_depth == 0 {
                    body_start = Some(start);
                }
                body_depth += 1;
            }
        }
        cursor = end;
    }
    None
}

fn replace_body_element(document: &str, replacement: &str) -> Option<String> {
    let (replacement_start, replacement_end) = xml_body_element_range(replacement)?;
    let replacement_bytes = replacement.as_bytes();
    let trimmed_start = replacement_bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())?;
    let trimmed_end = replacement_bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())?
        + 1;
    if (replacement_start, replacement_end) != (trimmed_start, trimmed_end) {
        return None;
    }

    let (body_start, close_end) = xml_body_element_range(document)?;
    let mut html = String::with_capacity(document.len() + replacement.len());
    html.push_str(&document[..body_start]);
    html.push_str(&replacement[replacement_start..replacement_end]);
    html.push_str(&document[close_end..]);
    Some(html)
}

fn parse_rich_text_snapshot(document: &str, unit_id: &str) -> Result<ParsedSource> {
    let (body_start, body_end) =
        xml_body_element_range(document).context("富文本快照缺少正文元素")?;
    // The WebView snapshot is a complete trusted XHTML shell. Parsing that
    // whole shell through an HTML fragment sanitizer can preserve the removed
    // <title> text as visible body content, duplicating the chapter title on
    // every rich-text save. Only the serialized body is editable content.
    parse_source_for_unit(SourceKind::Html, &document[body_start..body_end], unit_id)
}

fn preview_document_from_source(
    template: &str,
    title: &str,
    unit_id: &str,
    source_kind: SourceKind,
    source: &str,
) -> Result<(String, String, moye_epub_editor::document::BlockDocument)> {
    let parsed = parse_source_for_unit(source_kind, source, unit_id)?;
    let html = serialize_xhtml(&parsed.document)?;
    let fallback = editor_document_shell(title, &html);
    let replacement = if xml_body_element_range(&html).is_some() {
        html
    } else {
        format!("<body><article>{html}</article></body>")
    };
    let preview = replace_body_element(template, &replacement).unwrap_or(fallback);
    Ok((preview, parsed.canonical_source, parsed.document))
}

fn editor_document_shell(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><meta charset=\"utf-8\"/><title>{}</title></head><body><article>{body}</article></body></html>",
        escape_editor_xml(title)
    )
}

fn escape_editor_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn editor_caption_text(caption: &[Inline]) -> Option<String> {
    let text = caption.iter().map(Inline::plain_text).collect::<String>();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn editor_media_label(kind: MediaKind, title: Option<&str>, description: Option<&str>) -> String {
    title
        .filter(|title| !title.trim().is_empty())
        .or_else(|| description.filter(|description| !description.trim().is_empty()))
        .unwrap_or(match kind {
            MediaKind::Image => "图片",
            MediaKind::Audio => "音频",
            MediaKind::Video => "视频",
        })
        .to_string()
}

fn media_blocks(blocks: &[Block]) -> Vec<EditorMediaBlock> {
    fn collect_inlines(inlines: &[Inline], output: &mut Vec<EditorMediaBlock>) {
        for inline in inlines {
            match inline {
                Inline::Image {
                    asset_id,
                    alt,
                    title,
                } => output.push(EditorMediaBlock {
                    id: format!("inline-image:{asset_id}"),
                    kind: MediaKind::Image,
                    label: editor_media_label(MediaKind::Image, title.as_deref(), Some(alt)),
                    title: title.clone(),
                    description: (!alt.trim().is_empty()).then(|| alt.trim().to_string()),
                }),
                Inline::Emphasis { content }
                | Inline::Strong { content }
                | Inline::Strikethrough { content }
                | Inline::Link { content, .. } => collect_inlines(content, output),
                _ => {}
            }
        }
    }

    fn collect(blocks: &[Block], output: &mut Vec<EditorMediaBlock>) {
        for block in blocks {
            match block {
                Block::Image {
                    id,
                    alt,
                    title,
                    caption,
                    ..
                } => {
                    let description = editor_caption_text(caption)
                        .or_else(|| (!alt.trim().is_empty()).then(|| alt.trim().to_string()));
                    output.push(EditorMediaBlock {
                        id: id.clone(),
                        kind: MediaKind::Image,
                        label: editor_media_label(
                            MediaKind::Image,
                            title.as_deref(),
                            description.as_deref(),
                        ),
                        title: title.clone(),
                        description,
                    });
                }
                Block::Audio {
                    id, title, caption, ..
                } => {
                    let description = editor_caption_text(caption);
                    output.push(EditorMediaBlock {
                        id: id.clone(),
                        kind: MediaKind::Audio,
                        label: editor_media_label(
                            MediaKind::Audio,
                            title.as_deref(),
                            description.as_deref(),
                        ),
                        title: title.clone(),
                        description,
                    });
                }
                Block::Video {
                    id, title, caption, ..
                } => {
                    let description = editor_caption_text(caption);
                    output.push(EditorMediaBlock {
                        id: id.clone(),
                        kind: MediaKind::Video,
                        label: editor_media_label(
                            MediaKind::Video,
                            title.as_deref(),
                            description.as_deref(),
                        ),
                        title: title.clone(),
                        description,
                    });
                }
                Block::BlockQuote { blocks, .. } => collect(blocks, output),
                Block::BulletList { items, .. } | Block::OrderedList { items, .. } => {
                    for item in items {
                        collect(&item.blocks, output);
                    }
                }
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    collect_inlines(content, output)
                }
                Block::Table { header, rows, .. } => {
                    for row in header.iter().chain(rows) {
                        for cell in &row.cells {
                            collect_inlines(&cell.content, output);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut output = Vec::new();
    collect(blocks, &mut output);
    output
}

fn editor_caption_from_description(description: Option<&str>) -> Vec<Inline> {
    description
        .map(|description| vec![Inline::text(description)])
        .unwrap_or_default()
}

fn update_media_metadata(
    blocks: &mut [Block],
    block_id: &str,
    metadata: &EditorMediaMetadataDraft,
) -> bool {
    fn update_inline(
        inlines: &mut [Inline],
        asset_id: &str,
        metadata: &EditorMediaMetadataDraft,
    ) -> bool {
        for inline in inlines {
            match inline {
                Inline::Image {
                    asset_id: current,
                    alt,
                    title,
                } if current == asset_id => {
                    *alt = metadata.description.clone().unwrap_or_default();
                    *title = metadata.title.clone();
                    return true;
                }
                Inline::Emphasis { content }
                | Inline::Strong { content }
                | Inline::Strikethrough { content }
                | Inline::Link { content, .. } => {
                    if update_inline(content, asset_id, metadata) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if let Some(asset_id) = block_id.strip_prefix("inline-image:") {
        for block in blocks {
            let updated = match block {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    update_inline(content, asset_id, metadata)
                }
                Block::Table { header, rows, .. } => header
                    .iter_mut()
                    .chain(rows.iter_mut())
                    .flat_map(|row| row.cells.iter_mut())
                    .any(|cell| update_inline(&mut cell.content, asset_id, metadata)),
                Block::BlockQuote { blocks, .. } => {
                    update_media_metadata(blocks, block_id, metadata)
                }
                Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                    .iter_mut()
                    .any(|item| update_media_metadata(&mut item.blocks, block_id, metadata)),
                _ => false,
            };
            if updated {
                return true;
            }
        }
        return false;
    }

    for block in blocks {
        if block.id() == block_id {
            match block {
                Block::Image {
                    alt,
                    title,
                    caption,
                    ..
                } => {
                    *alt = metadata.description.clone().unwrap_or_default();
                    *title = metadata.title.clone();
                    *caption = editor_caption_from_description(metadata.description.as_deref());
                    return true;
                }
                Block::Audio { title, caption, .. } | Block::Video { title, caption, .. } => {
                    *title = metadata.title.clone();
                    *caption = editor_caption_from_description(metadata.description.as_deref());
                    return true;
                }
                _ => return false,
            }
        }
        let updated = match block {
            Block::BlockQuote { blocks, .. } => update_media_metadata(blocks, block_id, metadata),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| update_media_metadata(&mut item.blocks, block_id, metadata)),
            _ => false,
        };
        if updated {
            return true;
        }
    }
    false
}

fn replace_media_asset(blocks: &mut [Block], block_id: &str, asset_id: &str) -> bool {
    fn replace_inline(inlines: &mut [Inline], old_asset_id: &str, asset_id: &str) -> bool {
        for inline in inlines {
            match inline {
                Inline::Image {
                    asset_id: current, ..
                } if current == old_asset_id => {
                    *current = asset_id.to_string();
                    return true;
                }
                Inline::Emphasis { content }
                | Inline::Strong { content }
                | Inline::Strikethrough { content }
                | Inline::Link { content, .. } => {
                    if replace_inline(content, old_asset_id, asset_id) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if let Some(old_asset_id) = block_id.strip_prefix("inline-image:") {
        for block in blocks {
            let replaced = match block {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    replace_inline(content, old_asset_id, asset_id)
                }
                Block::Table { header, rows, .. } => header
                    .iter_mut()
                    .chain(rows.iter_mut())
                    .flat_map(|row| row.cells.iter_mut())
                    .any(|cell| replace_inline(&mut cell.content, old_asset_id, asset_id)),
                Block::BlockQuote { blocks, .. } => replace_media_asset(blocks, block_id, asset_id),
                Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                    .iter_mut()
                    .any(|item| replace_media_asset(&mut item.blocks, block_id, asset_id)),
                _ => false,
            };
            if replaced {
                return true;
            }
        }
        return false;
    }
    for block in blocks {
        if block.id() == block_id {
            match block {
                Block::Image {
                    asset_id: current, ..
                }
                | Block::Audio {
                    asset_id: current, ..
                }
                | Block::Video {
                    asset_id: current, ..
                } => {
                    *current = asset_id.to_string();
                    return true;
                }
                _ => return false,
            }
        }
        let replaced = match block {
            Block::BlockQuote { blocks, .. } => replace_media_asset(blocks, block_id, asset_id),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| replace_media_asset(&mut item.blocks, block_id, asset_id)),
            _ => false,
        };
        if replaced {
            return true;
        }
    }
    false
}

fn delete_media(blocks: &mut Vec<Block>, block_id: &str) -> bool {
    fn delete_inline(inlines: &mut Vec<Inline>, asset_id: &str) -> bool {
        if let Some(position) = inlines.iter().position(
            |inline| matches!(inline, Inline::Image { asset_id: current, .. } if current == asset_id),
        ) {
            inlines.remove(position);
            return true;
        }
        inlines.iter_mut().any(|inline| match inline {
            Inline::Emphasis { content }
            | Inline::Strong { content }
            | Inline::Strikethrough { content }
            | Inline::Link { content, .. } => delete_inline(content, asset_id),
            _ => false,
        })
    }

    if let Some(asset_id) = block_id.strip_prefix("inline-image:") {
        return blocks.iter_mut().any(|block| match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                delete_inline(content, asset_id)
            }
            Block::Table { header, rows, .. } => header
                .iter_mut()
                .chain(rows.iter_mut())
                .flat_map(|row| row.cells.iter_mut())
                .any(|cell| delete_inline(&mut cell.content, asset_id)),
            Block::BlockQuote { blocks, .. } => delete_media(blocks, block_id),
            Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
                .iter_mut()
                .any(|item| delete_media(&mut item.blocks, block_id)),
            _ => false,
        });
    }
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
    blocks.iter_mut().any(|block| match block {
        Block::BlockQuote { blocks, .. } => delete_media(blocks, block_id),
        Block::BulletList { items, .. } | Block::OrderedList { items, .. } => items
            .iter_mut()
            .any(|item| delete_media(&mut item.blocks, block_id)),
        _ => false,
    })
}

fn prune_editor_assets(document: &mut BookDocument) {
    let referenced = document
        .units
        .iter()
        .flat_map(|unit| unit.document.referenced_asset_ids())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let cover = document.cover_asset_id.clone();
    let original = match &document.source {
        BookSource::Imported {
            original_asset_id, ..
        } => Some(original_asset_id.clone()),
        BookSource::Created => None,
    };
    document.assets.retain(|asset| {
        referenced.contains(&asset.id)
            || cover.as_deref() == Some(asset.id.as_str())
            || original.as_deref() == Some(asset.id.as_str())
    });
}

fn clear_editor_cover_assignment(document: &mut BookDocument) {
    document.cover_asset_id = None;
    for asset in &mut document.assets {
        asset.roles.retain(|role| *role != AssetRole::Cover);
    }
}

fn retain_referenced_pending_assets(
    pending: &mut HashMap<String, Arc<Vec<u8>>>,
    document: &BookDocument,
) {
    let retained = document
        .assets
        .iter()
        .map(|asset| asset.id.as_str())
        .collect::<HashSet<_>>();
    pending.retain(|asset_id, _| retained.contains(asset_id.as_str()));
}

fn source_kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Markdown => "Markdown",
        SourceKind::Html => "HTML",
    }
}

fn editor_book_format_label(document: &BookDocument) -> &'static str {
    match &document.source {
        // Newly created books use the normalized EPUB publishing path.
        BookSource::Created => "EPUB",
        BookSource::Imported { format, .. } => match format {
            BookFormat::Epub => "EPUB",
            BookFormat::Pdf => "PDF",
            BookFormat::Doc => "DOC",
            BookFormat::Docx => "DOCX",
            BookFormat::Pptx => "PPTX",
            BookFormat::Xlsx => "XLSX",
            BookFormat::Mobi => "MOBI",
            BookFormat::Azw => "AZW",
            BookFormat::Azw3 => "AZW3",
        },
    }
}

fn editor_clear_cover_enabled(has_cover: bool, controls_disabled: bool) -> bool {
    has_cover && !controls_disabled
}

fn unit_kind_label(kind: ContentUnitKind) -> &'static str {
    match kind {
        ContentUnitKind::Chapter => "章节",
        ContentUnitKind::Section => "小节",
        ContentUnitKind::Page => "页面",
        ContentUnitKind::Slide => "幻灯片",
        ContentUnitKind::Worksheet => "工作表",
    }
}

fn next_unit_kind(kind: ContentUnitKind) -> ContentUnitKind {
    match kind {
        ContentUnitKind::Chapter => ContentUnitKind::Section,
        ContentUnitKind::Section => ContentUnitKind::Page,
        ContentUnitKind::Page => ContentUnitKind::Slide,
        ContentUnitKind::Slide => ContentUnitKind::Worksheet,
        ContentUnitKind::Worksheet => ContentUnitKind::Chapter,
    }
}

#[cfg(test)]
fn toc_depth_for_unit(nodes: &[TocNode], unit_id: &str) -> usize {
    fn find(nodes: &[TocNode], unit_id: &str, depth: usize) -> Option<usize> {
        nodes.iter().find_map(|node| {
            (node.target.unit_id() == unit_id)
                .then_some(depth)
                .or_else(|| find(&node.children, unit_id, depth + 1))
        })
    }
    find(nodes, unit_id, 0).unwrap_or(0)
}

pub(super) fn suggested_epub_filename(title: &str) -> String {
    let mut stem = title
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();

    let trimmed_len = stem.trim_end_matches([' ', '.']).len();
    stem.truncate(trimmed_len);
    if stem.to_ascii_lowercase().ends_with(".epub") {
        stem.truncate(stem.len() - ".epub".len());
    }
    let trimmed_len = stem.trim_end_matches([' ', '.']).len();
    stem.truncate(trimmed_len);
    stem = stem.chars().take(120).collect();
    let trimmed_len = stem.trim_end_matches([' ', '.']).len();
    stem.truncate(trimmed_len);
    if stem.is_empty() {
        stem.push_str("book");
    }

    let device_component_len = stem.split('.').next().unwrap_or_default().len();
    let device_name = stem[..device_component_len].to_ascii_uppercase();
    let reserved = matches!(device_name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device_name
            .strip_prefix("COM")
            .or_else(|| device_name.strip_prefix("LPT"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        stem.insert(device_component_len, '_');
    }

    format!("{stem}.epub")
}

/// Converts editable XHTML into compact visible text for in-memory editor
/// searches. The persisted library index performs the same job while importing
/// and saving; this local path is needed so unsaved source/rich-text snapshots
/// are searchable before they reach SQLite.
fn searchable_text_from_xhtml(source: &str) -> String {
    fn push_text_char(output: &mut String, ch: char) {
        if ch.is_whitespace() {
            if !output.ends_with(' ') && !output.is_empty() {
                output.push(' ');
            }
        } else {
            output.push(ch);
        }
    }

    fn decode_entity(entity: &str) -> Option<char> {
        match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        }
    }

    fn is_text_boundary(name: &str) -> bool {
        matches!(
            name,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "body"
                | "br"
                | "dd"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "hr"
                | "li"
                | "main"
                | "nav"
                | "ol"
                | "p"
                | "pre"
                | "section"
                | "table"
                | "tbody"
                | "td"
                | "tfoot"
                | "th"
                | "thead"
                | "tr"
                | "ul"
        )
    }

    let mut output = String::with_capacity(source.len().min(64 * 1024));
    let mut chars = source.chars().peekable();
    let mut hidden_tags = Vec::<String>::new();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut tag = String::new();
            let mut quote = None;
            for next in chars.by_ref() {
                match next {
                    '\'' | '"' if quote.is_none() => quote = Some(next),
                    next if quote == Some(next) => quote = None,
                    '>' if quote.is_none() => break,
                    _ => {}
                }
                tag.push(next);
            }
            let tag = tag.trim();
            let closing = tag.starts_with('/');
            let self_closing = tag.ends_with('/');
            let name = tag
                .trim_start_matches('/')
                .split(|ch: char| ch.is_whitespace() || ch == '/')
                .next()
                .unwrap_or_default()
                .rsplit(':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if closing {
                if let Some(position) = hidden_tags.iter().rposition(|hidden| hidden == &name) {
                    hidden_tags.truncate(position);
                } else if hidden_tags.is_empty() && is_text_boundary(&name) {
                    push_text_char(&mut output, ' ');
                }
            } else if matches!(
                name.as_str(),
                "head" | "script" | "style" | "template" | "noscript"
            ) && !self_closing
            {
                hidden_tags.push(name);
            } else if hidden_tags.is_empty() && is_text_boundary(&name) {
                push_text_char(&mut output, ' ');
            }
            continue;
        }
        if !hidden_tags.is_empty() {
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            let mut terminated = false;
            while entity.len() <= 12 {
                let Some(next) = chars.peek().copied() else {
                    break;
                };
                if next == ';' {
                    chars.next();
                    terminated = true;
                    break;
                }
                if next.is_whitespace() || matches!(next, '<' | '&') {
                    break;
                }
                entity.push(next);
                chars.next();
            }
            if terminated {
                if let Some(decoded) = decode_entity(&entity) {
                    push_text_char(&mut output, decoded);
                } else {
                    output.push('&');
                    output.push_str(&entity);
                    output.push(';');
                }
            } else {
                output.push('&');
                output.push_str(&entity);
            }
            continue;
        }
        push_text_char(&mut output, ch);
    }
    output.trim().to_string()
}

fn search_snippet(text: &str, query: &str) -> Option<String> {
    let folded = text.to_lowercase();
    let query = query.to_lowercase();
    let byte_start = folded.find(&query)?;
    let match_start = folded[..byte_start].chars().count();
    let match_len = query.chars().count();
    let chars = text.chars().collect::<Vec<_>>();
    let snippet_start = match_start.saturating_sub(36);
    let snippet_end = (match_start + match_len + 56).min(chars.len());
    let mut snippet = chars[snippet_start..snippet_end].iter().collect::<String>();
    if snippet_start > 0 {
        snippet.insert(0, '…');
    }
    if snippet_end < chars.len() {
        snippet.push('…');
    }
    Some(snippet)
}

fn search_editor_chapters(chapters: &[EditorChapter], query: &str) -> Vec<EditorSearchHit> {
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Vec::new();
    }
    chapters
        .iter()
        .enumerate()
        .filter_map(|(chapter_index, chapter)| {
            let body = searchable_text_from_xhtml(&chapter.html);
            let matches_all = terms.iter().all(|term| {
                search_snippet(&body, term).is_some()
                    || search_snippet(&chapter.title, term).is_some()
            });
            if !matches_all {
                return None;
            }
            let snippet = terms
                .iter()
                .find_map(|term| search_snippet(&body, term))
                .or_else(|| {
                    terms.iter().find_map(|term| {
                        search_snippet(&chapter.title, term)
                            .map(|title| format!("章节标题：{title}"))
                    })
                })
                .unwrap_or_else(|| chapter.title.clone());
            Some(EditorSearchHit {
                chapter_index,
                chapter_title: chapter.title.clone(),
                snippet,
            })
        })
        .take(200)
        .collect()
}

/// Builds the WebView used for the editor's preview and WYSIWYG tabs. The
/// trusted ProseMirror shell and canonical chapter projection use distinct
/// private origins; only the content origin can serve authorized book media.
pub(super) async fn build_editor_webview(
    parent: &ParentWindowHandle,
    state: EditorWebState,
    services: Arc<AppServices>,
) -> Result<(
    gpui_component::wry::WebView,
    async_channel::Receiver<EditorWebEvent>,
)> {
    let initial_page = state.page(0).context("编辑器缺少初始章节")?;
    let initial_url = editor_custom_url(
        EditorTab::Preview,
        state.session_id(),
        &initial_page.chapter_id,
        0,
        &initial_page.href,
    );
    let (sender, receiver) = async_channel::unbounded::<EditorWebEvent>();
    let page_sender = sender.clone();
    let ipc_state = state.clone();
    let protocol_state = state.clone();
    let protocol_runtime = services.runtime();
    let raw_webview = gpui_component::wry::WebViewBuilder::new()
        .with_ipc_handler(move |request| {
            if request.body().len() > MAX_EDITOR_IPC_BYTES {
                return;
            }
            let Some(document) = editor_document_request(request.uri()) else {
                return;
            };
            if let Some(message) = parse_editor_citation_navigation_result(request.body()) {
                if document.kind != EditorDocumentKind::Preview
                    || message.session_id != document.session_id
                    || message.chapter_id != document.chapter_id
                    || message.revision != document.revision
                    || normalized_editor_path(&message.href).as_deref()
                        != Some(document.href.as_str())
                {
                    return;
                }
                let _ = sender.try_send(EditorWebEvent::CitationNavigationResult {
                    session_id: message.session_id,
                    chapter_id: message.chapter_id,
                    href: message.href,
                    revision: message.revision,
                    request_id: message.request_id,
                    found: message.found,
                    reason: message.reason,
                });
                return;
            }
            let Some(message) = parse_editor_ipc_message(request.body()) else {
                return;
            };
            if !editor_ipc_matches_document_request(&document, &message) {
                return;
            }
            if let Some(update) = ipc_state.apply_message(message) {
                let _ = sender.try_send(EditorWebEvent::Ipc(update));
            }
        })
        .with_asynchronous_custom_protocol(
            "epubeditor".to_string(),
            move |_, request, responder| {
                if !protocol_state.protocol_is_open() {
                    return;
                }
                let protocol_asset = if editor_origin(request.uri()) == Some(EditorOrigin::Content)
                {
                    match editor_asset_request(request.uri()) {
                        EditorAssetRequest::Asset(asset_id) => {
                            let snapshot = protocol_state.media_snapshot();
                            snapshot
                                .assets
                                .get(&asset_id)
                                .cloned()
                                .map(|asset| (asset_id, asset, snapshot))
                        }
                        EditorAssetRequest::Invalid | EditorAssetRequest::NotAsset => None,
                    }
                } else {
                    None
                };
                let Some((asset_id, asset, snapshot)) = protocol_asset else {
                    let response = editor_owned_response(editor_protocol_response(
                        &protocol_state,
                        &request,
                    ));
                    let open = protocol_state.protocol_open.lock().unwrap();
                    if *open {
                        responder.respond(response);
                    }
                    return;
                };
                let range_header = request
                    .headers()
                    .get("Range")
                    .map(|value| value.to_str().unwrap_or("invalid").to_string());
                let book_id = protocol_state.book_id.to_string();
                let task = spawn_editor_media(
                    &services,
                    snapshot,
                    book_id.clone(),
                    asset_id.clone(),
                    asset,
                    range_header,
                );
                let response_state = protocol_state.clone();
                protocol_runtime.spawn(async move {
                    let response = match task.await {
                        Ok(Ok(response)) => editor_media_response(response),
                        Ok(Err(error)) => {
                            tracing::warn!(%book_id, %asset_id, %error, "cannot serve editor media");
                            editor_error_response(404, "编辑器媒体不存在")
                        }
                        Err(error) => {
                            tracing::warn!(%book_id, %asset_id, %error, "editor media task stopped");
                            editor_error_response(404, "编辑器媒体不存在")
                        }
                    };
                    let open = response_state.protocol_open.lock().unwrap();
                    if *open {
                        responder.respond(editor_owned_response(response));
                    }
                });
            },
        )
        .with_initialization_script(EDITOR_INITIALIZATION_SCRIPT)
        .with_navigation_handler(|url| is_editor_navigation_url(&url))
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, gpui_component::wry::PageLoadEvent::Finished) {
                let _ = page_sender.try_send(EditorWebEvent::PageLoaded(url));
            }
        })
        .with_new_window_req_handler(|_, _| gpui_component::wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_background_color((251, 250, 247, 255))
        .with_hotkeys_zoom(false)
        .with_incognito(true)
        .with_url(initial_url)
        .build_as_child_async(parent)
        .await
        .context("无法创建编辑器视图")?;

    Ok((raw_webview, receiver))
}

/// Editor window root: lets the user rewrite the title, author, chapter titles
/// and chapter XHTML of a book, and add new chapters.
pub struct EditorApp {
    book_id: String,
    library: LibraryStore,
    library_projection_generation: u64,
    services: Arc<AppServices>,
    library_view: WeakEntity<EpubReaderApp>,
    ai_sidebar: Entity<AiSidebar>,
    ai_sidebar_collapsed: bool,
    left_sidebar_width: Pixels,
    right_sidebar_width: Pixels,
    pane_viewport_width: Pixels,
    ai_controller: AiSidebarController,
    pending_ai_request: Option<AiQuestionRequest>,
    pending_citation_source: Option<AiSourceLink>,
    pending_citation_navigation: Option<PendingEditorCitationNavigation>,
    cover: Option<CoverDraft>,
    cover_preview: Option<Arc<Image>>,
    cover_dirty: bool,
    cover_loading: bool,
    pending_cover_action: Option<PendingEditorAction>,
    search_input: Entity<InputState>,
    search_query: String,
    search_results: Vec<EditorSearchHit>,
    _search_subscriptions: Vec<Subscription>,
    title_input: Entity<InputState>,
    author_input: Entity<InputState>,
    chapter_title_input: Entity<InputState>,
    body_input: Entity<InputState>,
    chapters: Vec<EditorChapter>,
    canonical_document: Option<BookDocument>,
    unit_states: Vec<EditorUnitState>,
    /// Chapter IDs whose source has been edited (source tab or rich text)
    /// since the last load or successful save. Used to skip pointless
    /// re-serialization when toggling the source kind on a pristine chapter.
    modified_chapter_ids: HashSet<String>,
    pending_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    media_loading: bool,
    media_modal: Option<EditorMediaModal>,
    media_target: Option<EditorMediaBlock>,
    pending_media_action: Option<PendingEditorAction>,
    selected: usize,
    selected_toc_id: Option<String>,
    pending_toc_id: Option<String>,
    tab: EditorTab,
    editor_webview: Option<Entity<WebView>>,
    web_state: EditorWebState,
    web_revision: u64,
    active_web_page: Option<ActiveEditorPage>,
    ai_selected_text: Option<String>,
    snapshot_request: u64,
    pending_snapshot: Option<PendingEditorSnapshot>,
    pending_ready_action: Option<PendingEditorAction>,
    export_dialog_open: bool,
    pending_export_path: Option<PathBuf>,
    active_write: Option<ActiveEditorWrite>,
    write_sequence: u64,
    close_after_write: bool,
    close_prompt_open: bool,
    close_confirmation_pending: bool,
    close_without_saving_pending: bool,
    draft_generation: u64,
    webview_build_gate: EditorWebViewBuildGate,
    pub(super) ipc_sync_task: Option<Task<()>>,
    ready_timeout_task: Option<Task<()>>,
    closing_webview: Option<WeakEntity<WebView>>,
    closing: bool,
    /// Set when the window closes because its book left the library: unsaved
    /// edits belong to a document that no longer exists.
    closing_for_removed_book: bool,
    removal_scheduled: bool,
    pub(super) notice: Option<Notice>,
}

impl EditorApp {
    pub fn new(
        book_id: String,
        library: LibraryStore,
        library_projection_generation: u64,
        services: Arc<AppServices>,
        library_view: WeakEntity<EpubReaderApp>,
        cover: Option<CoverDraft>,
        title_input: Entity<InputState>,
        author_input: Entity<InputState>,
        chapter_title_input: Entity<InputState>,
        body_input: Entity<InputState>,
        chapters: Vec<EditorChapter>,
        document: BookDocument,
        web_state: EditorWebState,
        webview_building: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前图书正文…"));
        let mut _search_subscriptions = vec![
            cx.subscribe_in(&search_input, window, Self::on_search_input_event),
            cx.subscribe_in(&title_input, window, Self::on_book_title_input_event),
            cx.subscribe_in(&author_input, window, Self::on_author_input_event),
            cx.subscribe_in(
                &chapter_title_input,
                window,
                Self::on_chapter_title_input_event,
            ),
            cx.subscribe_in(&body_input, window, Self::on_body_input_event),
        ];
        let cover_preview = cover.as_ref().and_then(|cover| {
            image_format_from_mime(cover.mime())
                .map(|format| Arc::new(Image::from_bytes(format, (**cover.bytes()).clone())))
        });
        let canonical_document = if document.id == book_id && document.units.len() == chapters.len()
        {
            Some(document)
        } else {
            tracing::warn!(
                book_id,
                canonical_book_id = document.id,
                canonical_units = document.units.len(),
                editor_chapters = chapters.len(),
                "canonical document and editor projection are not aligned"
            );
            None
        };
        let unit_states = canonical_document
            .as_ref()
            .map(EditorUnitState::from_document)
            .unwrap_or_default();
        if let (Some(chapter), Some(unit)) = (chapters.first(), unit_states.first()) {
            web_state.set(
                0,
                unit.id.clone(),
                chapter.href.clone(),
                chapter.html.clone(),
            );
        }
        if let Some(source) = unit_states.first().map(|unit| unit.source.clone()) {
            body_input.update(cx, |state, cx| {
                state.set_value(source, window, cx);
            });
        }
        let current_book =
            AiBookOption::new(book_id.clone(), title_input.read(cx).value().to_string());
        let available_books = library
            .books()
            .iter()
            .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
            .collect();
        let ai_sidebar = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(current_book, available_books),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        let references =
            editor_reference_hints(&book_id, &chapters, &unit_states, 0, None, None, 0);
        ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
        let mut ai_controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Editor,
            Some(book_id.clone()),
        )
        .expect("editor AI scope is valid");
        _search_subscriptions.push(cx.subscribe_in(&ai_sidebar, window, Self::on_ai_sidebar_event));
        let ai_sidebar_collapsed = ai_sidebar.read(cx).is_collapsed();
        _search_subscriptions.push(cx.observe(&ai_sidebar, |this, sidebar, cx| {
            let collapsed = sidebar.read(cx).is_collapsed();
            if this.ai_sidebar_collapsed != collapsed {
                this.ai_sidebar_collapsed = collapsed;
                this.constrain_pane_widths(this.pane_viewport_width, cx);
                cx.notify();
            }
        }));
        _search_subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.pane_viewport_width = window.viewport_size().width;
            this.constrain_pane_widths(this.pane_viewport_width, cx);
        }));
        ai_controller.restore(ai_sidebar.clone(), cx);
        Self {
            book_id,
            library,
            library_projection_generation,
            services,
            library_view,
            ai_sidebar,
            ai_sidebar_collapsed,
            left_sidebar_width: px(EDITOR_LEFT_SIDEBAR_DEFAULT_WIDTH),
            right_sidebar_width: px(AI_SIDEBAR_WIDTH),
            pane_viewport_width: window.viewport_size().width,
            ai_controller,
            pending_ai_request: None,
            pending_citation_source: None,
            pending_citation_navigation: None,
            cover,
            cover_preview,
            cover_dirty: false,
            cover_loading: false,
            pending_cover_action: None,
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            _search_subscriptions,
            title_input,
            author_input,
            chapter_title_input,
            body_input,
            chapters,
            canonical_document,
            unit_states,
            modified_chapter_ids: HashSet::new(),
            pending_asset_bytes: HashMap::new(),
            media_loading: false,
            media_modal: None,
            media_target: None,
            pending_media_action: None,
            selected: 0,
            selected_toc_id: None,
            pending_toc_id: None,
            tab: EditorTab::Source,
            editor_webview: None,
            web_state,
            web_revision: 0,
            active_web_page: None,
            ai_selected_text: None,
            snapshot_request: 0,
            pending_snapshot: None,
            pending_ready_action: None,
            export_dialog_open: false,
            pending_export_path: None,
            active_write: None,
            write_sequence: 0,
            close_after_write: false,
            close_prompt_open: false,
            close_confirmation_pending: false,
            close_without_saving_pending: false,
            draft_generation: 0,
            webview_build_gate: EditorWebViewBuildGate::new(webview_building),
            ipc_sync_task: None,
            ready_timeout_task: None,
            closing_webview: None,
            closing: false,
            closing_for_removed_book: false,
            removal_scheduled: false,
            notice: None,
        }
    }

    fn resize_pane_from_pointer(
        &mut self,
        pane: EditorResizablePane,
        pointer_x: Pixels,
        viewport_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        match pane {
            EditorResizablePane::Left => {
                let right_width = if self.ai_sidebar_collapsed {
                    px(AI_SIDEBAR_COLLAPSED_WIDTH)
                } else {
                    self.right_sidebar_width
                };
                let width = constrained_editor_pane_width(
                    editor_pane_width_from_pointer(pane, pointer_x, viewport_width),
                    px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
                    px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
                    viewport_width,
                    right_width,
                );
                if self.left_sidebar_width != width {
                    self.left_sidebar_width = width;
                    cx.notify();
                }
            }
            EditorResizablePane::Right => {
                let width = constrained_editor_pane_width(
                    editor_pane_width_from_pointer(pane, pointer_x, viewport_width),
                    px(AI_SIDEBAR_MIN_WIDTH),
                    px(AI_SIDEBAR_MAX_WIDTH),
                    viewport_width,
                    self.left_sidebar_width,
                );
                if self.right_sidebar_width != width {
                    self.right_sidebar_width = width;
                    self.ai_sidebar.update(cx, |sidebar, cx| {
                        sidebar.set_expanded_width(width, cx);
                    });
                    cx.notify();
                }
            }
        }
    }

    fn constrain_pane_widths(&mut self, viewport_width: Pixels, cx: &mut Context<Self>) {
        let collapsed = self.ai_sidebar_collapsed;
        let occupied_right_width = if collapsed {
            px(AI_SIDEBAR_COLLAPSED_WIDTH)
        } else {
            self.right_sidebar_width
        };
        let left_width = constrained_editor_pane_width(
            self.left_sidebar_width,
            px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
            viewport_width,
            occupied_right_width,
        );
        let right_width = if collapsed {
            self.right_sidebar_width
        } else {
            constrained_editor_pane_width(
                self.right_sidebar_width,
                px(AI_SIDEBAR_MIN_WIDTH),
                px(AI_SIDEBAR_MAX_WIDTH),
                viewport_width,
                left_width,
            )
        };
        let left_changed = self.left_sidebar_width != left_width;
        let right_changed = self.right_sidebar_width != right_width;
        if left_changed {
            self.left_sidebar_width = left_width;
        }
        if right_changed {
            self.right_sidebar_width = right_width;
            self.ai_sidebar.update(cx, |sidebar, cx| {
                sidebar.set_expanded_width(right_width, cx);
            });
        }
        if left_changed || right_changed {
            cx.notify();
        }
    }

    fn on_ai_sidebar_event(
        &mut self,
        _sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                self.pending_ai_request = Some(request.clone());
                self.begin_action(PendingEditorAction::AskAi, window, cx);
            }
            AiSidebarEvent::Cancel { request_id } => {
                if self
                    .pending_ai_request
                    .as_ref()
                    .is_some_and(|request| request.request_id == *request_id)
                {
                    self.pending_ai_request = None;
                    if self
                        .pending_snapshot
                        .as_ref()
                        .is_some_and(|pending| pending.action == PendingEditorAction::Close)
                        || self.pending_ready_action == Some(PendingEditorAction::Close)
                    {
                        cancel_application_exit(cx);
                    }
                    self.pending_snapshot = None;
                    self.pending_ready_action = None;
                }
                self.ai_controller.cancel(*request_id);
            }
            AiSidebarEvent::NewSession => {
                self.ai_controller.new_session(self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::SwitchSession { thread_id } => {
                self.ai_controller
                    .switch_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::DeleteSession { thread_id } => {
                self.ai_controller
                    .delete_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::ScopeChanged { book_ids } => {
                self.ai_controller
                    .reconcile_scope(book_ids.clone(), self.ai_sidebar.clone(), cx)
            }
            AiSidebarEvent::OpenSource(source) => self.open_ai_source(source.clone(), window, cx),
        }
    }

    fn perform_ai_question(&mut self, cx: &mut Context<Self>) {
        let Some(mut request) = self.pending_ai_request.take() else {
            return;
        };
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            self.ai_sidebar.update(cx, |sidebar, cx| {
                sidebar.fail_answer(
                    request.request_id,
                    "正文源码尚未通过解析和清洗，未发送可能过期的 AI 引用。",
                    cx,
                );
            });
            return;
        }
        // Rich text reaches this point only after the exact
        // session/chapter/revision/request-id snapshot acknowledgement. Source
        // mode was flushed above. Freeze every selected chapter from this editor
        // session so unsaved edits in previously visited chapters cannot be
        // replaced by older SQLite content while the model is answering.
        freeze_editor_reference_hints(
            &self.book_id,
            &self.chapters,
            &self.unit_states,
            self.selected,
            self.ai_selected_text.as_deref(),
            self.web_revision,
            &mut request.reference_hints,
        );
        let current_unit_id = self
            .unit_states
            .get(self.selected)
            .map(|unit| unit.id.as_str())
            .or_else(|| {
                self.chapters
                    .get(self.selected)
                    .map(|chapter| chapter.href.as_str())
            });
        request.reference = current_unit_id.and_then(|unit_id| {
            request
                .reference_hints
                .iter()
                .find(|reference| reference.book_id == self.book_id && reference.unit_id == unit_id)
                .cloned()
        });
        self.ai_controller
            .submit(request, self.ai_sidebar.clone(), cx);
    }

    fn fail_pending_ai(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let Some(request) = self.pending_ai_request.take() else {
            return;
        };
        let message = message.into();
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.fail_answer(request.request_id, message, cx);
        });
    }

    fn set_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: message.into(),
            error: true,
        });
        cx.notify();
    }

    fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            lookup
                .current_unit_index(&document)
                .context("引用对应的文档或内容版本已失效")
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) if source.book_id == this.book_id => {
                        this.pending_citation_source = Some(source);
                        this.begin_action(PendingEditorAction::OpenCitation(index), window, cx)
                    }
                    Ok(Ok(index)) => {
                        if let Some(library_view) = this.library_view.upgrade() {
                            library_view.update(cx, |library, cx| {
                                library.open_book_at_source(source, Some(index), window, cx);
                            });
                        }
                    }
                    Ok(Err(error)) => this.set_error(format!("引用已失效：{error:#}"), cx),
                    Err(error) => this.set_error(format!("引用定位任务已停止：{error}"), cx),
                });
            });
        })
        .detach();
    }

    fn on_book_title_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.draft_generation = self.draft_generation.wrapping_add(1);
            self.sync_ai_scope(cx);
        }
    }

    fn on_author_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.draft_generation = self.draft_generation.wrapping_add(1);
            cx.notify();
        }
    }

    fn on_search_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                // Source mode has no IPC bridge, so copy both editable fields
                // into the in-memory chapter before searching. Rich text emits
                // its latest body on blur; `apply_ipc_message` refreshes active
                // results when that snapshot arrives.
                self.flush_chapter_title(cx);
                if self.tab == EditorTab::Source {
                    let _ = self.flush_body(cx);
                }
                self.search_query = input.read(cx).value().trim().to_string();
                self.refresh_editor_search(cx);
            }
            InputEvent::PressEnter { .. } => {
                if let Some(chapter_index) =
                    self.search_results.first().map(|hit| hit.chapter_index)
                {
                    self.select_chapter(chapter_index, window, cx);
                }
            }
            _ => {}
        }
    }

    fn on_chapter_title_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.draft_generation = self.draft_generation.wrapping_add(1);
            self.sync_ai_reference(cx);
            if self.search_query.is_empty() {
                // The status bar mirrors the live chapter-title input.
                cx.notify();
            } else {
                self.flush_chapter_title(cx);
                self.refresh_editor_search(cx);
            }
        }
    }

    fn on_body_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            // A programmatic `sync_body_input` sets the value to exactly the
            // stored unit source, so the equality check below ignores those
            // reloads and only flags genuine user edits.
            let value = self.body_input.read(cx).value();
            if let Some(state) = self.unit_states.get(self.selected) {
                if value.as_ref() != state.source.as_str() {
                    self.modified_chapter_ids.insert(state.id.clone());
                }
            }
            self.draft_generation = self.draft_generation.wrapping_add(1);
            if self.tab == EditorTab::Source && !self.search_query.is_empty() {
                let _ = self.flush_body(cx);
                self.refresh_editor_search(cx);
            } else {
                cx.notify();
            }
        }
    }

    fn refresh_editor_search(&mut self, cx: &mut Context<Self>) {
        self.search_results = search_editor_chapters(&self.chapters, &self.search_query);
        cx.notify();
    }

    pub(super) fn attach_webview(
        &mut self,
        webview: Entity<WebView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor_webview = Some(webview);
        let close_after_build = self.webview_build_gate.finish();
        // Building is asynchronous, so honor whichever tab is active when the
        // native child WebView actually becomes ready.
        if self.tab.uses_webview() {
            self.load_webview(self.tab, window, cx);
        } else if let Some(webview) = &self.editor_webview {
            webview.update(cx, |webview, _| webview.hide());
        }
        if close_after_build {
            self.resume_deferred_close(window, cx);
        }
        cx.notify();
        true
    }

    /// Resumes a close that was vetoed while the child WebView was building.
    fn resume_deferred_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing_for_removed_book {
            self.finish_removal_close(window, cx);
        } else {
            self.handle_window_close(window, cx);
        }
    }

    pub(super) fn fail_webview_build(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let close_after_build = self.webview_build_gate.finish();
        self.notice = Some(Notice {
            text: format!("无法创建预览视图：{error}"),
            error: true,
        });
        if close_after_build {
            self.resume_deferred_close(window, cx);
            return;
        }
        cx.notify();
    }

    pub(super) fn apply_web_event(
        &mut self,
        event: EditorWebEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorWebEvent::Ipc(message) => self.apply_ipc_message(message, window, cx),
            EditorWebEvent::PageLoaded(url) => self.run_pending_citation_navigation(&url, cx),
            EditorWebEvent::CitationNavigationResult {
                session_id,
                chapter_id,
                href,
                revision,
                request_id,
                found,
                reason,
            } => self.finish_citation_navigation(
                &session_id,
                &chapter_id,
                &href,
                revision,
                request_id,
                found,
                &reason,
                cx,
            ),
        }
    }

    fn run_pending_citation_navigation(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(document) = url
            .parse()
            .ok()
            .and_then(|uri| editor_document_request(&uri))
        else {
            return;
        };
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if document.kind != EditorDocumentKind::Preview
            || pending.session_id != document.session_id
            || pending.chapter_id != document.chapter_id
            || pending.revision != document.revision
            || normalized_editor_path(&pending.href).as_deref() != Some(document.href.as_str())
        {
            return;
        }
        let Some(webview) = self.editor_webview.as_ref() else {
            self.pending_citation_navigation = None;
            self.set_error("预览视图尚未就绪，无法定位引用", cx);
            return;
        };
        let script = match citation_dom_navigation_script(
            &pending.focus,
            serde_json::json!({
                "type": "citation_navigation_result",
                "session_id": pending.session_id,
                "chapter_id": pending.chapter_id,
                "href": pending.href,
                "revision": pending.revision,
                "request_id": pending.request_id,
            }),
        ) {
            Ok(script) => script,
            Err(error) => {
                self.pending_citation_navigation = None;
                self.set_error(error, cx);
                return;
            }
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.pending_citation_navigation = None;
            self.set_error(format!("无法执行引用定位：{error}"), cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_citation_navigation(
        &mut self,
        session_id: &str,
        chapter_id: &str,
        href: &str,
        revision: u64,
        request_id: u64,
        found: bool,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if pending.session_id != session_id
            || pending.chapter_id != chapter_id
            || !editor_hrefs_match(&pending.href, href)
            || pending.revision != revision
            || pending.request_id != request_id
        {
            return;
        }
        self.pending_citation_navigation = None;
        if found {
            self.notice = None;
            cx.notify();
        } else {
            let detail = match reason {
                "ambiguous" => "预览中存在多个相同片段",
                "not_found" | "missing_root" | "empty_text" => "预览中找不到该片段",
                _ => "预览无法映射该文字范围",
            };
            self.set_error(
                format!("引用定位失败：{detail}；未跳转到章节中的其它位置。"),
                cx,
            );
        }
    }

    /// Applies only snapshots from the currently authoritative rich-text page.
    /// Explicit snapshots carry a request id; the queued action runs only after
    /// that exact acknowledgement has updated the chapter.
    pub(super) fn apply_ipc_message(
        &mut self,
        message: EditorIpcUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let Some(active) = self.active_web_page.as_ref() else {
            return;
        };
        if self.tab != EditorTab::RichText
            || message.session_id != active.session_id
            || message.chapter_id != active.chapter_id
            || message.revision != active.revision
            || !editor_hrefs_match(&message.href, &active.href)
        {
            return;
        }

        // The session/chapter/revision identity above binds this transient
        // selection to the active page. An Ask-AI action consumes the selection
        // carried by its exact request-id acknowledgement below.
        self.ai_selected_text = message.selected_text.clone();

        if message.ready {
            self.ready_timeout_task.take();
            if let Some(active) = self.active_web_page.as_mut() {
                active.ready = true;
            }
            if let Some(action) = self.pending_ready_action.take() {
                self.begin_action(action, window, cx);
            } else {
                cx.notify();
            }
            return;
        }

        let acknowledges_pending = self.pending_snapshot.as_ref().is_some_and(|pending| {
            message.request_id == Some(pending.request_id)
                && message.session_id == pending.session_id
                && message.chapter_id == pending.chapter_id
                && message.revision == pending.revision
                && editor_hrefs_match(&message.href, &pending.href)
        });
        if message.too_large {
            if acknowledges_pending {
                let action = self.pending_snapshot.take().map(|pending| pending.action);
                self.pending_export_path = None;
                if action == Some(PendingEditorAction::Close) {
                    cancel_application_exit(cx);
                }
                if action == Some(PendingEditorAction::AskAi) {
                    self.fail_pending_ai("所选章节过大，无法安全冻结为 AI 引用。", cx);
                }
            }
            self.notice = Some(Notice {
                text: "富文本正文超过 8 MiB，请先在富文本中精简内容后再保存或切换".to_string(),
                error: true,
            });
            cx.notify();
            return;
        }
        let Some(chapter_index) = self
            .chapters
            .iter()
            .position(|chapter| editor_hrefs_match(&chapter.href, &message.href))
        else {
            return;
        };
        if message.edited || self.chapters[chapter_index].html != message.html {
            if let Err(error) = self.update_unit_from_rich_text(chapter_index, &message.html) {
                self.notice = Some(Notice {
                    text: format!("无法解析富文本编辑结果：{error:#}"),
                    error: true,
                });
                if self
                    .pending_snapshot
                    .take()
                    .is_some_and(|pending| pending.action == PendingEditorAction::Close)
                {
                    cancel_application_exit(cx);
                }
                self.pending_export_path = None;
                cx.notify();
                return;
            }
            if self.chapters[chapter_index].html != message.html {
                self.draft_generation = self.draft_generation.wrapping_add(1);
            }
            self.chapters[chapter_index].html = message.html.clone();
            if let Some(state) = self.unit_states.get(chapter_index) {
                self.modified_chapter_ids.insert(state.id.clone());
            }
        }
        // An unchanged acknowledgement carries the original display shell,
        // including its article wrapper. It is not a new editable snapshot:
        // reparsing it would collapse typed blocks and lose media references.
        // A previously rejected snapshot differs from the accepted chapter and
        // must still pass parsing, even if the web page now reports unchanged.
        // Only an accepted snapshot releases the exact pending action below.
        if !self.search_query.is_empty() {
            self.search_results = search_editor_chapters(&self.chapters, &self.search_query);
        }
        self.sync_ai_reference(cx);

        if acknowledges_pending {
            let action = self.pending_snapshot.take().unwrap().action;
            self.perform_action(action, window, cx);
        } else {
            cx.notify();
        }
    }

    fn handle_editor_ready_timeout(
        &mut self,
        session_id: &str,
        chapter_id: &str,
        revision: u64,
        href: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let still_waiting = self
            .active_web_page
            .as_ref()
            .is_some_and(|active| active.is_unready_match(session_id, chapter_id, revision, href));
        if self.tab != EditorTab::RichText || !still_waiting {
            return;
        }

        // A page that never sends Ready was never confirmed editable. Drop its
        // page identity instead of waiting forever for a bridge that does not
        // exist. Export must not fall back to potentially stale Rust state.
        self.active_web_page = None;
        let pending_action = self.pending_ready_action.take();
        if let Some(action) = pending_action {
            if action == PendingEditorAction::Export {
                self.pending_export_path = None;
            } else if action == PendingEditorAction::AskAi {
                self.fail_pending_ai("富文本编辑器尚未就绪，未发送可能过期的章节内容。", cx);
            } else {
                self.perform_action(action, window, cx);
                if action == PendingEditorAction::Close {
                    return;
                }
            }
        }
        self.notice = Some(Notice {
            text: "富文本编辑器初始化失败，已安全退回；请使用源码模式检查本章 XHTML".to_string(),
            error: true,
        });
        cx.notify();
    }

    fn begin_action(
        &mut self,
        action: PendingEditorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(operation) = self.active_write.as_ref() {
            if action == PendingEditorAction::Close {
                self.close_after_write = true;
                self.notice = Some(Notice {
                    text: "正在后台保存；保存成功后会安全关闭编辑器。".to_string(),
                    error: false,
                });
            } else {
                if action == PendingEditorAction::Export {
                    self.pending_export_path = None;
                }
                let operation_name = match operation.intent {
                    EditorWriteIntent::Save => "保存",
                    EditorWriteIntent::Export => "导出",
                    EditorWriteIntent::Close => "关闭保存",
                };
                self.notice = Some(Notice {
                    text: format!("正在后台{operation_name}，请等待当前操作完成。"),
                    error: false,
                });
            }
            cx.notify();
            return;
        }
        if self.media_modal.is_some()
            && matches!(
                action,
                PendingEditorAction::Save
                    | PendingEditorAction::Export
                    | PendingEditorAction::Close
            )
        {
            self.pending_media_action = Some(match self.pending_media_action {
                Some(pending) => {
                    merge_pending_editor_action(pending, action, &mut self.pending_export_path)
                }
                None => action,
            });
            self.notice = Some(Notice {
                text: "请先保存或取消媒体信息，随后会继续当前操作。".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        if self.media_loading
            && matches!(
                action,
                PendingEditorAction::Save
                    | PendingEditorAction::Export
                    | PendingEditorAction::Close
            )
        {
            self.pending_media_action = Some(match self.pending_media_action {
                Some(pending) => {
                    merge_pending_editor_action(pending, action, &mut self.pending_export_path)
                }
                None => action,
            });
            self.notice = Some(Notice {
                text: "正在读取媒体文件，完成后会继续操作。".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        if self.cover_loading
            && matches!(
                action,
                PendingEditorAction::Save
                    | PendingEditorAction::Export
                    | PendingEditorAction::Close
            )
        {
            self.pending_cover_action = Some(match self.pending_cover_action {
                Some(pending) => {
                    merge_pending_editor_action(pending, action, &mut self.pending_export_path)
                }
                None => action,
            });
            self.notice = Some(Notice {
                text: "正在读取封面图片，完成后会继续操作…".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        if let Some(pending) = self.pending_snapshot.as_mut() {
            pending.action =
                merge_pending_editor_action(pending.action, action, &mut self.pending_export_path);
            return;
        }
        if let Some(pending) = self.pending_ready_action.as_mut() {
            *pending = merge_pending_editor_action(*pending, action, &mut self.pending_export_path);
            return;
        }
        if action != PendingEditorAction::Export {
            self.pending_export_path = None;
        }
        if self.tab == EditorTab::RichText
            && let Some(active) = self.active_web_page.clone()
        {
            if !active.ready {
                self.pending_ready_action = Some(action);
                self.notice = Some(Notice {
                    text: "富文本编辑器正在加载，请稍候…".to_string(),
                    error: false,
                });
                cx.notify();
                return;
            }
            let Some(webview) = self.editor_webview.as_ref() else {
                self.perform_action(action, window, cx);
                return;
            };
            self.snapshot_request = if self.snapshot_request >= MAX_EDITOR_REQUEST_ID {
                1
            } else {
                self.snapshot_request + 1
            };
            let request_id = self.snapshot_request;
            self.pending_snapshot = Some(PendingEditorSnapshot {
                session_id: active.session_id,
                chapter_id: active.chapter_id,
                href: active.href,
                revision: active.revision,
                request_id,
                action,
            });
            let script =
                format!("window.__moyeEditorSend && window.__moyeEditorSend({request_id});");
            if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
                self.pending_snapshot = None;
                self.pending_export_path = None;
                if action == PendingEditorAction::Close {
                    cancel_application_exit(cx);
                }
                if action == PendingEditorAction::AskAi {
                    self.fail_pending_ai(format!("无法冻结当前章节供 AI 引用：{error}"), cx);
                }
                self.notice = Some(Notice {
                    text: format!("无法同步富文本内容：{error}"),
                    error: true,
                });
                cx.notify();
            } else {
                // Expose the pending sync/save/export state immediately instead
                // of waiting for the WebView's snapshot acknowledgement.
                cx.notify();
            }
            return;
        }
        self.perform_action(action, window, cx);
    }

    fn perform_action(
        &mut self,
        action: PendingEditorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action != PendingEditorAction::SelectToc {
            self.pending_toc_id = None;
        }
        match action {
            PendingEditorAction::SwitchTab(tab) => self.perform_switch_tab(tab, window, cx),
            PendingEditorAction::SelectChapter(index) => {
                self.perform_select_chapter(index, window, cx)
            }
            PendingEditorAction::SelectToc => self.perform_select_toc(window, cx),
            PendingEditorAction::AddChapter => self.perform_add_chapter(window, cx),
            PendingEditorAction::RemoveChapter => self.perform_remove_chapter(window, cx),
            PendingEditorAction::MoveChapterUp => self.perform_move_chapter(-1, window, cx),
            PendingEditorAction::MoveChapterDown => self.perform_move_chapter(1, window, cx),
            PendingEditorAction::IndentToc => self.perform_indent_toc(cx),
            PendingEditorAction::OutdentToc => self.perform_outdent_toc(cx),
            PendingEditorAction::CycleUnitKind => self.perform_cycle_unit_kind(cx),
            PendingEditorAction::SetSourceKind(kind) => {
                self.perform_set_source_kind(kind, window, cx)
            }
            PendingEditorAction::InsertMedia(kind) => {
                self.choose_media_file(kind, None, window, cx)
            }
            PendingEditorAction::ReplaceMedia => {
                if let Some(target) = self.media_target.take() {
                    self.choose_media_file(target.kind, Some(target), window, cx);
                }
            }
            PendingEditorAction::EditMediaMetadata => {
                if let Some(target) = self.media_target.take() {
                    self.open_media_metadata_editor(target, window, cx);
                }
            }
            PendingEditorAction::DeleteMedia => self.perform_delete_media(window, cx),
            PendingEditorAction::Save => {
                self.perform_save_draft(window, cx);
            }
            PendingEditorAction::Export => self.perform_export(window, cx),
            PendingEditorAction::AskAi => self.perform_ai_question(cx),
            PendingEditorAction::OpenCitation(index) => {
                self.perform_open_citation(index, window, cx)
            }
            PendingEditorAction::Close => {
                self.start_editor_write(EditorWriteIntent::Close, None, window, cx);
            }
        }
    }

    fn switch_tab(&mut self, tab: EditorTab, window: &mut Window, cx: &mut Context<Self>) {
        let pending_tab = self
            .pending_snapshot
            .as_ref()
            .map(|pending| pending.action)
            .or(self.pending_ready_action)
            .and_then(|action| match action {
                PendingEditorAction::SwitchTab(tab) => Some(tab),
                _ => None,
            });
        if self.tab != tab || pending_tab.is_some_and(|pending| pending != tab) {
            self.begin_action(PendingEditorAction::SwitchTab(tab), window, cx);
        }
    }

    fn perform_switch_tab(&mut self, tab: EditorTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.ready_timeout_task.take();
        if self.tab == EditorTab::Source {
            if !self.flush_body(cx) {
                return;
            }
        }
        let converted_to_html = tab == EditorTab::RichText && self.convert_selected_to_html();
        if tab == EditorTab::RichText
            && self
                .chapters
                .get(self.selected)
                .is_some_and(|chapter| chapter.html.len() > MAX_EDITOR_BODY_BYTES)
        {
            self.notice = Some(Notice {
                text: "本章超过 8 MiB，无法进入富文本模式；仍可使用源码和预览".to_string(),
                error: true,
            });
            cx.notify();
            return;
        }
        self.ai_selected_text = None;
        self.tab = tab;
        match tab {
            EditorTab::Source => {
                // Removing a native child HWND from the GPUI element tree does
                // not hide it; left visible, it keeps covering the source view.
                self.active_web_page = None;
                if let Some(webview) = &self.editor_webview {
                    webview.update(cx, |webview, _| webview.hide());
                }
                self.sync_body_input(self.selected, window, cx);
            }
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.notice = converted_to_html.then(|| Notice {
            text: "进入富文本模式后，本章源码已规范化为 HTML；保存前仍可切回 Markdown。"
                .to_string(),
            error: false,
        });
        cx.notify();
    }

    /// Reloads the editor webview for the preview or rich-text tab using a
    /// fresh `epubeditor://` URL. A new URL every time guarantees the webview
    /// performs a real navigation and re-queries the custom protocol.
    fn load_webview(&mut self, tab: EditorTab, window: &mut Window, cx: &mut Context<Self>) {
        self.load_webview_with_focus(tab, None, false, window, cx);
    }

    fn load_webview_for_citation(
        &mut self,
        target: AiCitationNavigationTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_webview.is_none() {
            self.set_error("预览视图尚未就绪，无法打开引用", cx);
            return;
        }
        self.load_webview_with_focus(EditorTab::Preview, target.focus, true, window, cx);
    }

    fn load_webview_with_focus(
        &mut self,
        tab: EditorTab,
        citation_focus: Option<AiCitationTextFocus>,
        citation_navigation_requested: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ready_timeout_task.take();
        self.pending_citation_navigation = None;
        self.ai_selected_text = None;
        let Some(webview) = &self.editor_webview else {
            self.active_web_page = None;
            return;
        };
        webview.update(cx, |webview, _| webview.show());
        let Some(chapter) = self.chapters.get(self.selected) else {
            return;
        };
        let Some(unit) = self.unit_states.get(self.selected) else {
            self.active_web_page = None;
            return;
        };
        self.web_revision += 1;
        let revision = self.web_revision;
        let session_id = self.web_state.session_id().to_string();
        let chapter_id = unit.id.clone();
        self.web_state.set(
            revision,
            chapter_id.clone(),
            chapter.href.clone(),
            chapter.html.clone(),
        );
        let url = editor_navigation_url(tab, &session_id, &chapter_id, revision, &chapter.href);
        self.pending_citation_navigation =
            citation_focus.map(|focus| PendingEditorCitationNavigation {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: chapter.href.clone(),
                revision,
                request_id: revision,
                focus,
            });
        self.active_web_page = if tab == EditorTab::RichText {
            Some(ActiveEditorPage {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: chapter.href.clone(),
                revision,
                ready: false,
            })
        } else {
            None
        };
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.active_web_page = None;
            self.pending_citation_navigation = None;
            tracing::warn!(%error, "failed to load editor page");
            if citation_navigation_requested {
                self.set_error(format!("无法打开引用预览：{error}"), cx);
            }
        } else if tab == EditorTab::RichText {
            let href = chapter.href.clone();
            self.ready_timeout_task = Some(cx.spawn_in(window, async move |view, cx| {
                Timer::after(EDITOR_READY_TIMEOUT).await;
                let _ = view.update_in(cx, |editor, window, cx| {
                    editor.handle_editor_ready_timeout(
                        &session_id,
                        &chapter_id,
                        revision,
                        &href,
                        window,
                        cx,
                    )
                });
            }));
        }
        self.sync_ai_reference(cx);
    }

    /// Pushes the title of `index` into the chapter-title input.
    fn sync_chapter_title_input(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chapter) = self.chapters.get(index) {
            self.chapter_title_input.update(cx, |state, cx| {
                state.set_value(chapter.title.clone(), window, cx);
            });
        }
    }

    /// Pushes the canonical Markdown/HTML source into the source editor.
    fn sync_body_input(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(source) = self
            .unit_states
            .get(index)
            .map(|unit| unit.source.clone())
            .or_else(|| self.chapters.get(index).map(|chapter| chapter.html.clone()))
        {
            self.body_input.update(cx, |state, cx| {
                state.set_value(source, window, cx);
            });
        }
    }

    fn sync_ai_reference(&mut self, cx: &mut Context<Self>) {
        let live_title = self.chapter_title_input.read(cx).value().trim().to_string();
        let references = editor_reference_hints(
            &self.book_id,
            &self.chapters,
            &self.unit_states,
            self.selected,
            (!live_title.is_empty()).then_some(live_title.as_str()),
            self.ai_selected_text.as_deref(),
            self.web_revision,
        );
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }

    fn sync_ai_scope(&mut self, cx: &mut Context<Self>) {
        let current = AiBookOption::new(
            self.book_id.clone(),
            self.title_input.read(cx).value().to_string(),
        );
        let available = self
            .library
            .books()
            .iter()
            .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
            .collect();
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_scope(AiSidebarScope::book(current, available), cx);
        });
    }

    fn select_chapter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index == self.selected || index >= self.chapters.len() {
            return;
        }
        self.begin_action(PendingEditorAction::SelectChapter(index), window, cx);
    }

    fn select_toc(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_toc_id = Some(id.to_string());
        self.begin_action(PendingEditorAction::SelectToc, window, cx);
    }

    fn current_toc_id(&self) -> Option<String> {
        let document = self.canonical_document.as_ref()?;
        let unit = self.unit_states.get(self.selected)?;
        let rows = editor_toc_rows(&document.toc);
        self.selected_toc_id
            .as_ref()
            .and_then(|id| {
                rows.iter()
                    .find(|row| row.id == *id && row.unit_id == unit.id)
            })
            .or_else(|| rows.iter().find(|row| row.unit_id == unit.id))
            .map(|row| row.id.clone())
    }

    fn perform_select_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.pending_toc_id.take() else {
            return;
        };
        let target = self.canonical_document.as_ref().and_then(|document| {
            editor_toc_rows(&document.toc)
                .into_iter()
                .find(|row| row.id == id)
        });
        let Some(target) = target else {
            self.set_error("目录项已不存在，请重新选择", cx);
            return;
        };
        let Some(index) = self
            .unit_states
            .iter()
            .position(|unit| unit.id == target.unit_id)
        else {
            self.set_error("目录对应的章节已不存在，请重新打开编辑窗口", cx);
            return;
        };
        self.perform_select_chapter(index, window, cx);
        // Source validation may veto switching. Never highlight a destination
        // whose content did not become the current editable unit.
        if self.selected == index {
            self.selected_toc_id = Some(id);
            cx.notify();
        }
    }

    fn perform_select_chapter(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index == self.selected || index >= self.chapters.len() {
            return;
        }
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source {
            if !self.flush_body(cx) {
                return;
            }
        }
        self.selected = index;
        self.selected_toc_id = None;
        self.ai_selected_text = None;
        self.sync_chapter_title_input(index, window, cx);
        match self.tab {
            EditorTab::Source => self.sync_body_input(index, window, cx),
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.sync_ai_reference(cx);
        self.notice = None;
        cx.notify();
    }

    fn perform_open_citation(
        &mut self,
        requested_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.pending_citation_source.take() else {
            self.set_error("引用定位状态已失效，请重新点击来源", cx);
            return;
        };
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(document) = self.canonical_document.as_ref() else {
            self.set_error("编辑器统一文档已失配，无法安全定位引用", cx);
            return;
        };
        let target = match source.current_navigation_target(document) {
            Ok(target) if target.unit_index == requested_index => target,
            Ok(_) => {
                self.set_error("引用内容单元与编辑器请求位置不匹配", cx);
                return;
            }
            Err(error) => {
                self.set_error(format!("引用已失效：{error}"), cx);
                return;
            }
        };
        if target.unit_index >= self.chapters.len() {
            self.set_error("引用对应的编辑章节已不存在", cx);
            return;
        }
        self.selected = target.unit_index;
        self.ai_selected_text = None;
        self.sync_chapter_title_input(target.unit_index, window, cx);
        self.tab = EditorTab::Preview;
        self.load_webview_for_citation(target, window, cx);
        self.sync_ai_reference(cx);
        cx.notify();
    }

    /// Copies the chapter-title input into the selected chapter.
    fn flush_chapter_title(&mut self, cx: &mut Context<Self>) {
        let value = self.chapter_title_input.read(cx).value().to_string();
        if let Some(chapter) = self.chapters.get_mut(self.selected) {
            chapter.title = value;
        }
    }

    /// Parses, cleans and copies the source editor into the canonical model.
    /// Invalid source never replaces the last valid AST or preview.
    fn flush_body(&mut self, cx: &mut Context<Self>) -> bool {
        let value = self.body_input.read(cx).value().to_string();
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            if let Some(chapter) = self.chapters.get_mut(self.selected) {
                chapter.html = value;
            }
            return true;
        };
        // Structured media blocks deliberately remain authoritative when the
        // source text has not changed. Re-parsing an identical Markdown source
        // would otherwise degrade allowlisted audio/video HTML to RawHtml.
        if value == state.source {
            return true;
        }
        let template = self
            .chapters
            .get(self.selected)
            .map(|chapter| chapter.html.as_str())
            .unwrap_or(DEFAULT_CHAPTER_HTML);
        let title = self
            .chapters
            .get(self.selected)
            .map(|chapter| chapter.title.as_str())
            .unwrap_or("章节");
        let (preview, source, blocks) = match preview_document_from_source(
            template,
            title,
            &state.id,
            state.source_kind,
            &value,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.notice = Some(Notice {
                    text: format!("正文源码无效，尚未保存：{error:#}"),
                    error: true,
                });
                cx.notify();
                return false;
            }
        };
        if let Some(unit) = self
            .canonical_document
            .as_mut()
            .and_then(|document| document.units.iter_mut().find(|unit| unit.id == state.id))
        {
            unit.source_kind = state.source_kind;
            unit.source = source.clone();
            unit.document = blocks;
        }
        if let Some(unit) = self.unit_states.get_mut(self.selected) {
            unit.source = source;
        }
        if let Some(chapter) = self.chapters.get_mut(self.selected) {
            chapter.html = preview;
        }
        true
    }

    fn update_unit_from_rich_text(&mut self, index: usize, html: &str) -> Result<()> {
        let state = self
            .unit_states
            .get(index)
            .cloned()
            .context("当前内容单元没有稳定 ID")?;
        let parsed = parse_rich_text_snapshot(html, &state.id)?;
        if let Some(unit) = self
            .canonical_document
            .as_mut()
            .and_then(|document| document.units.iter_mut().find(|unit| unit.id == state.id))
        {
            unit.source_kind = SourceKind::Html;
            unit.source = parsed.canonical_source.clone();
            unit.document = parsed.document;
        }
        if let Some(unit) = self.unit_states.get_mut(index) {
            unit.source_kind = SourceKind::Html;
            unit.source = parsed.canonical_source;
        }
        Ok(())
    }

    fn convert_selected_to_html(&mut self) -> bool {
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return false;
        };
        if state.source_kind == SourceKind::Html {
            return false;
        }
        let Some(document) = self.canonical_document.as_mut() else {
            return false;
        };
        let Some(unit) = document.units.iter_mut().find(|unit| unit.id == state.id) else {
            return false;
        };
        let Ok(source) = serialize_source(&unit.document, SourceKind::Html) else {
            return false;
        };
        unit.source_kind = SourceKind::Html;
        unit.source = source.clone();
        if let Some(editor_unit) = self.unit_states.get_mut(self.selected) {
            editor_unit.source_kind = SourceKind::Html;
            editor_unit.source = source;
        }
        true
    }

    fn add_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::AddChapter, window, cx);
    }

    fn perform_add_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(document) = self.canonical_document.clone() else {
            self.set_error("当前图书缺少统一文档模型，无法安全新增章节", cx);
            return;
        };
        let new_position = document.units.len();
        let title = format!("新章节 {}", new_position + 1);
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑图书结构：{error:#}"), cx);
                return;
            }
        };
        let unit_id = match editor.add_unit(NewContentUnit::markdown_chapter(
            title.clone(),
            new_position,
        )) {
            Ok(unit_id) => unit_id,
            Err(error) => {
                self.set_error(format!("无法新增章节：{error:#}"), cx);
                return;
            }
        };
        let next_document = editor.into_document();
        let Some(unit) = next_document.units.iter().find(|unit| unit.id == unit_id) else {
            self.set_error("新增章节没有生成稳定内容单元", cx);
            return;
        };

        // Pick a URL that does not collide with the protocol resources. This
        // href is only a view identity; persistence uses the stable unit ID.
        let mut index = self.chapters.len() + 1;
        let mut href = format!("chapter-{index}.xhtml");
        while self
            .chapters
            .iter()
            .any(|chapter| editor_hrefs_match(&chapter.href, &href))
        {
            index += 1;
            href = format!("chapter-{index}.xhtml");
        }
        let preview = preview_document_from_source(
            DEFAULT_CHAPTER_HTML,
            &title,
            &unit.id,
            unit.source_kind,
            &unit.source,
        )
        .map(|(preview, _, _)| preview)
        .unwrap_or_else(|_| DEFAULT_CHAPTER_HTML.to_string());
        self.chapters.push(EditorChapter {
            title,
            href,
            spine_index: None,
            html: preview,
        });
        self.unit_states.push(EditorUnitState {
            id: unit.id.clone(),
            kind: unit.kind,
            source_kind: unit.source_kind,
            source: unit.source.clone(),
        });
        self.canonical_document = Some(next_document);
        let new_index = self.chapters.len() - 1;
        self.selected = new_index;
        self.sync_chapter_title_input(new_index, window, cx);
        match self.tab {
            EditorTab::Source => self.sync_body_input(new_index, window, cx),
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.sync_ai_reference(cx);
        self.notice = None;
        if self.search_query.is_empty() {
            cx.notify();
        } else {
            self.refresh_editor_search(cx);
        }
    }

    fn remove_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chapters.is_empty() {
            return;
        }
        self.begin_action(PendingEditorAction::RemoveChapter, window, cx);
    }

    fn perform_remove_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            self.set_error("当前章节缺少稳定内容单元，无法安全删除", cx);
            return;
        };
        let Some(document) = self.canonical_document.clone() else {
            self.set_error("当前图书缺少统一文档模型，无法安全删除章节", cx);
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑图书结构：{error:#}"), cx);
                return;
            }
        };
        if let Err(error) = editor.remove_unit(&state.id) {
            self.set_error(format!("无法删除章节：{error:#}"), cx);
            return;
        }
        self.canonical_document = Some(editor.into_document());
        self.chapters.remove(self.selected);
        self.unit_states.remove(self.selected);
        self.modified_chapter_ids.remove(&state.id);
        if self.selected >= self.chapters.len() {
            self.selected = self.chapters.len().saturating_sub(1);
        }
        self.sync_chapter_title_input(self.selected, window, cx);
        match self.tab {
            EditorTab::Source => self.sync_body_input(self.selected, window, cx),
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.sync_ai_reference(cx);
        if self.search_query.is_empty() {
            cx.notify();
        } else {
            self.refresh_editor_search(cx);
        }
    }

    fn move_chapter(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        if (offset < 0 && self.selected == 0)
            || (offset > 0 && self.selected + 1 >= self.chapters.len())
        {
            return;
        }
        self.begin_action(
            if offset < 0 {
                PendingEditorAction::MoveChapterUp
            } else {
                PendingEditorAction::MoveChapterDown
            },
            window,
            cx,
        );
    }

    fn perform_move_chapter(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let destination = self.selected.saturating_add_signed(offset);
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑图书结构：{error:#}"), cx);
                return;
            }
        };
        if let Err(error) = editor.move_unit(&state.id, destination) {
            self.set_error(format!("无法移动内容单元：{error:#}"), cx);
            return;
        }
        self.canonical_document = Some(editor.into_document());
        let chapter = self.chapters.remove(self.selected);
        self.chapters.insert(destination, chapter);
        let state = self.unit_states.remove(self.selected);
        self.unit_states.insert(destination, state);
        self.selected = destination;
        self.sync_chapter_title_input(destination, window, cx);
        if self.tab == EditorTab::Source {
            self.sync_body_input(destination, window, cx);
        } else {
            self.load_webview(self.tab, window, cx);
        }
        self.sync_ai_reference(cx);
        self.notice = Some(Notice {
            text: "已调整线性阅读顺序，保存后生效。".to_string(),
            error: false,
        });
        cx.notify();
    }

    fn indent_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::IndentToc, window, cx);
    }

    fn perform_indent_toc(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let Some(toc_id) = self.current_toc_id() else {
            self.set_error("请先选择要调整的目录项", cx);
            return;
        };
        let Some((parent_id, position)) = toc_indent_destination(&document.toc, &toc_id) else {
            self.set_error("当前目录项没有可作为父目录的前一项", cx);
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑目录：{error:#}"), cx);
                return;
            }
        };
        match editor.move_toc_node(&toc_id, Some(&parent_id), position) {
            Ok(()) => {
                self.canonical_document = Some(editor.into_document());
                self.selected_toc_id = Some(toc_id);
                self.notice = Some(Notice {
                    text: "已将本项缩进为上一项的子目录，保存后生效。".to_string(),
                    error: false,
                });
                cx.notify();
            }
            Err(error) => self.set_error(format!("无法缩进目录：{error:#}"), cx),
        }
    }

    fn outdent_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::OutdentToc, window, cx);
    }

    fn perform_outdent_toc(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let Some(toc_id) = self.current_toc_id() else {
            return;
        };
        let Some((parent_id, position)) = toc_outdent_destination(&document.toc, &toc_id) else {
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑目录：{error:#}"), cx);
                return;
            }
        };
        match editor.move_toc_node(&toc_id, parent_id.as_deref(), position) {
            Ok(()) => {
                self.canonical_document = Some(editor.into_document());
                self.selected_toc_id = Some(toc_id);
                self.notice = Some(Notice {
                    text: "已将当前目录项提升一级，保存后生效。".to_string(),
                    error: false,
                });
                cx.notify();
            }
            Err(error) => self.set_error(format!("无法提升目录：{error:#}"), cx),
        }
    }

    fn cycle_unit_kind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::CycleUnitKind, window, cx);
    }

    fn perform_cycle_unit_kind(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        let kind = next_unit_kind(state.kind);
        let title = self.chapter_title_input.read(cx).value().trim().to_string();
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑内容单元：{error:#}"), cx);
                return;
            }
        };
        if let Err(error) = editor.update_unit_identity(&state.id, title, kind) {
            self.set_error(format!("无法修改内容类型：{error:#}"), cx);
            return;
        }
        self.canonical_document = Some(editor.into_document());
        if let Some(unit) = self.unit_states.get_mut(self.selected) {
            unit.kind = kind;
        }
        self.notice = Some(Notice {
            text: format!("内容类型已改为{}，保存后生效。", unit_kind_label(kind)),
            error: false,
        });
        cx.notify();
    }

    fn set_source_kind(
        &mut self,
        source_kind: SourceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .unit_states
            .get(self.selected)
            .is_none_or(|unit| unit.source_kind == source_kind)
        {
            return;
        }
        self.begin_action(PendingEditorAction::SetSourceKind(source_kind), window, cx);
    }

    fn perform_set_source_kind(
        &mut self,
        source_kind: SourceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        // Toggling the source kind on a chapter that has not been edited only
        // re-serialises the original content into a different representation,
        // which the user perceives as the source "being converted" for no
        // reason. Leave the pristine source untouched.
        if !self.modified_chapter_ids.contains(&state.id) {
            return;
        }
        let Some(document) = self.canonical_document.as_mut() else {
            return;
        };
        let Some(unit) = document.units.iter_mut().find(|unit| unit.id == state.id) else {
            return;
        };
        let source = match serialize_source(&unit.document, source_kind) {
            Ok(source) => source,
            Err(error) => {
                self.set_error(format!("无法转换正文源码：{error:#}"), cx);
                return;
            }
        };
        unit.source_kind = source_kind;
        unit.source = source.clone();
        if let Some(editor_unit) = self.unit_states.get_mut(self.selected) {
            editor_unit.source_kind = source_kind;
            editor_unit.source = source;
        }
        if source_kind == SourceKind::Markdown && self.tab == EditorTab::RichText {
            self.ready_timeout_task.take();
            self.active_web_page = None;
            self.ai_selected_text = None;
            self.tab = EditorTab::Source;
            if let Some(webview) = &self.editor_webview {
                webview.update(cx, |webview, _| webview.hide());
            }
            self.sync_body_input(self.selected, window, cx);
        } else if self.tab == EditorTab::Source {
            self.sync_body_input(self.selected, window, cx);
        }
        self.notice = Some(Notice {
            text: format!(
                "本章源码已规范化为 {}，保存后生效。",
                source_kind_label(source_kind)
            ),
            error: false,
        });
        cx.notify();
    }

    fn insert_media(&mut self, kind: MediaKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = None;
        self.begin_action(PendingEditorAction::InsertMedia(kind), window, cx);
    }

    fn replace_media(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = Some(target);
        self.begin_action(PendingEditorAction::ReplaceMedia, window, cx);
    }

    fn edit_media_metadata(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = Some(target);
        self.begin_action(PendingEditorAction::EditMediaMetadata, window, cx);
    }

    fn request_delete_media(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = Some(target);
        self.begin_action(PendingEditorAction::DeleteMedia, window, cx);
    }

    fn choose_media_file(
        &mut self,
        kind: MediaKind,
        replace_target: Option<EditorMediaBlock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, label, extensions): (&str, &str, &[&str]) = match kind {
            MediaKind::Image => (
                "选择正文图片",
                "图片",
                &["jpg", "jpeg", "png", "gif", "webp"],
            ),
            MediaKind::Audio => (
                "选择音频",
                "音频",
                &["mp3", "m4a", "aac", "ogg", "wav", "flac"],
            ),
            MediaKind::Video => ("选择视频", "视频", &["mp4", "webm", "ogv", "mov"]),
        };
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(title)
            .add_filter(label, extensions)
            .open_single_file();
        self.media_loading = true;
        self.notice = Some(Notice {
            text: format!("正在读取{label}…"),
            error: false,
        });
        cx.spawn_in(window, async move |view, cx| {
            let path = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = view.update_in(cx, |this, window, cx| {
                        this.media_loading = false;
                        this.media_target = None;
                        if let Some(action) = this.pending_media_action.take() {
                            // A cancelled insertion has no new state to save,
                            // but a previously requested save/close still runs.
                            this.begin_action(action, window, cx);
                        } else {
                            cx.notify();
                        }
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update_in(cx, |this, window, cx| {
                        this.media_loading = false;
                        this.media_target = None;
                        this.set_error(format!("无法打开媒体文件选择器：{error}"), cx);
                        if let Some(action) = this.pending_media_action.take() {
                            this.begin_action(action, window, cx);
                        }
                    });
                    return;
                }
            };
            let task = cx.background_spawn(async move {
                let bytes = read_editor_media_file(&path, kind)?;
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
                let media_type = mime_guess::from_path(&path)
                    .first_raw()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let title = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or(label)
                    .to_string();
                MediaDraft::from_bytes(kind, media_type, file_name, bytes)
                    .map(|draft| PreparedEditorMedia::new(draft.with_title(title)))
            });
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.media_loading = false;
                this.media_target = None;
                match result {
                    Ok(media) => {
                        let initial_title = replace_target
                            .as_ref()
                            .and_then(|target| target.title.clone())
                            .or_else(|| media.draft.title.clone());
                        let initial_description = replace_target
                            .as_ref()
                            .and_then(|target| target.description.clone())
                            .or_else(|| media.draft.description.clone());
                        this.open_media_metadata_modal(
                            EditorMediaModalAction::InsertOrReplace {
                                media,
                                replace_block_id: replace_target
                                    .as_ref()
                                    .map(|target| target.id.clone()),
                            },
                            kind,
                            initial_title.as_deref().unwrap_or_default(),
                            initial_description.as_deref().unwrap_or_default(),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        this.set_error(format!("无法使用所选媒体：{error:#}"), cx);
                        if let Some(action) = this.pending_media_action.take() {
                            this.begin_action(action, window, cx);
                        }
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn open_media_metadata_editor(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = target.title.clone().unwrap_or_default();
        let description = target.description.clone().unwrap_or_default();
        let kind = target.kind;
        self.open_media_metadata_modal(
            EditorMediaModalAction::Edit { target },
            kind,
            &title,
            &description,
            window,
            cx,
        );
    }

    fn open_media_metadata_modal(
        &mut self,
        action: EditorMediaModalAction,
        kind: MediaKind,
        title: &str,
        description: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("可选，例如：人物关系图")
                .default_value(title.to_string())
        });
        let description_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder("可选；将进入正文纯文本、全文检索与 AI 引用")
                .default_value(description.to_string())
        });
        title_input.update(cx, |state, cx| state.focus(window, cx));
        self.media_modal = Some(EditorMediaModal {
            action,
            kind,
            title_input,
            description_input,
        });
        self.notice = Some(Notice {
            text: "请填写媒体标题与说明。".to_string(),
            error: false,
        });
        cx.notify();
    }

    fn close_media_metadata_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.media_modal = None;
        if let Some(action) = self.pending_media_action.take() {
            self.begin_action(action, window, cx);
        } else {
            cx.notify();
        }
    }

    fn confirm_media_metadata_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.media_modal.clone() else {
            return;
        };
        let metadata = match EditorMediaMetadataDraft::from_inputs(
            &modal.title_input.read(cx).value(),
            &modal.description_input.read(cx).value(),
        ) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.set_error(format!("媒体信息无效：{error}"), cx);
                return;
            }
        };
        let result = match modal.action {
            EditorMediaModalAction::InsertOrReplace {
                mut media,
                replace_block_id,
            } => {
                media.draft.title = metadata.title;
                media.draft.description = metadata.description;
                self.apply_media_draft(media, replace_block_id.as_deref(), window, cx)
            }
            EditorMediaModalAction::Edit { target } => {
                self.apply_media_metadata(&target.id, &metadata, window, cx)
            }
        };
        if let Err(error) = result {
            self.set_error(format!("无法更新媒体信息：{error:#}"), cx);
            return;
        }
        self.media_modal = None;
        if let Some(action) = self.pending_media_action.take() {
            self.begin_action(action, window, cx);
        } else {
            cx.notify();
        }
    }

    fn apply_media_draft(
        &mut self,
        media: PreparedEditorMedia,
        replace_block_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let PreparedEditorMedia { draft, asset } = media;
        let metadata = EditorMediaMetadataDraft {
            title: draft.title.clone(),
            description: draft.description.clone(),
        };
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            anyhow::bail!("正文源码尚未通过解析");
        }
        let state = self
            .unit_states
            .get(self.selected)
            .cloned()
            .context("当前内容单元不存在")?;
        let document = self
            .canonical_document
            .as_mut()
            .context("当前图书缺少统一文档模型")?;
        let asset_id = asset.id.clone();
        if let Some(existing) = document.assets.iter_mut().find(|item| item.id == asset.id) {
            for role in &asset.roles {
                existing.add_role(*role);
            }
        } else {
            document.assets.push(asset.clone());
        }
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == state.id)
            .context("当前内容单元不存在")?;
        if let Some(block_id) = replace_block_id {
            anyhow::ensure!(
                replace_media_asset(&mut unit.document.blocks, block_id, &asset_id),
                "媒体块不存在"
            );
            anyhow::ensure!(
                update_media_metadata(&mut unit.document.blocks, block_id, &metadata),
                "媒体块信息不存在"
            );
        } else {
            let block_id = deterministic_id(
                "block",
                format!(
                    "{}\0{}\0{}\0{}",
                    unit.id,
                    unit.revision.get(),
                    asset_id,
                    unit.document.blocks.len()
                )
                .as_bytes(),
            );
            let block = match draft.kind {
                MediaKind::Image => Block::Image {
                    id: block_id,
                    asset_id: asset_id.clone(),
                    alt: metadata.description.clone().unwrap_or_default(),
                    title: metadata.title.clone(),
                    caption: editor_caption_from_description(metadata.description.as_deref()),
                },
                MediaKind::Audio => Block::Audio {
                    id: block_id,
                    asset_id: asset_id.clone(),
                    title: metadata.title.clone(),
                    caption: editor_caption_from_description(metadata.description.as_deref()),
                },
                MediaKind::Video => Block::Video {
                    id: block_id,
                    asset_id: asset_id.clone(),
                    poster_asset_id: None,
                    title: metadata.title.clone(),
                    caption: editor_caption_from_description(metadata.description.as_deref()),
                },
            };
            unit.document.blocks.push(block);
        }
        unit.source = serialize_source(&unit.document, unit.source_kind)?;
        self.pending_asset_bytes
            .insert(asset_id, draft.bytes().clone());
        prune_editor_assets(document);
        retain_referenced_pending_assets(&mut self.pending_asset_bytes, document);
        document.validate()?;
        self.web_state
            .sync_media(document, Some((&asset, draft.bytes().clone())))?;
        self.sync_unit_state_from_document(self.selected);
        self.refresh_selected_editor_views(window, cx)?;
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.sync_ai_reference(cx);
        if !self.search_query.is_empty() {
            self.refresh_editor_search(cx);
        }
        self.notice = Some(Notice {
            text: if replace_block_id.is_some() {
                "已替换媒体，保存后写入对象存储。".to_string()
            } else {
                "已插入媒体，保存后写入对象存储。".to_string()
            },
            error: false,
        });
        cx.notify();
        Ok(())
    }

    fn apply_media_metadata(
        &mut self,
        block_id: &str,
        metadata: &EditorMediaMetadataDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            anyhow::bail!("正文源码尚未通过解析");
        }
        let state = self
            .unit_states
            .get(self.selected)
            .cloned()
            .context("当前内容单元不存在")?;
        let document = self
            .canonical_document
            .as_mut()
            .context("当前图书缺少统一文档模型")?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == state.id)
            .context("当前内容单元不存在")?;
        anyhow::ensure!(
            update_media_metadata(&mut unit.document.blocks, block_id, metadata),
            "媒体块不存在"
        );
        unit.source = serialize_source(&unit.document, unit.source_kind)?;
        document.validate()?;
        self.sync_unit_state_from_document(self.selected);
        self.refresh_selected_editor_views(window, cx)?;
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.sync_ai_reference(cx);
        if !self.search_query.is_empty() {
            self.refresh_editor_search(cx);
        }
        self.notice = Some(Notice {
            text: "已更新媒体标题与说明，保存后进入全文检索。".to_string(),
            error: false,
        });
        cx.notify();
        Ok(())
    }

    fn perform_delete_media(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.media_target.take() else {
            return;
        };
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        let Some(document) = self.canonical_document.as_mut() else {
            return;
        };
        let Some(unit) = document.units.iter_mut().find(|unit| unit.id == state.id) else {
            return;
        };
        if !delete_media(&mut unit.document.blocks, &target.id) {
            self.set_error("媒体块不存在", cx);
            return;
        }
        match serialize_source(&unit.document, unit.source_kind) {
            Ok(source) => unit.source = source,
            Err(error) => {
                self.set_error(format!("无法更新正文源码：{error:#}"), cx);
                return;
            }
        }
        prune_editor_assets(document);
        retain_referenced_pending_assets(&mut self.pending_asset_bytes, document);
        if let Err(error) = self.web_state.sync_media(document, None) {
            self.set_error(format!("无法更新媒体预览：{error:#}"), cx);
            return;
        }
        self.sync_unit_state_from_document(self.selected);
        if let Err(error) = self.refresh_selected_editor_views(window, cx) {
            self.set_error(format!("无法刷新编辑视图：{error:#}"), cx);
            return;
        }
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.sync_ai_reference(cx);
        if !self.search_query.is_empty() {
            self.refresh_editor_search(cx);
        }
        self.notice = Some(Notice {
            text: "已删除媒体引用，保存后异步回收无引用对象。".to_string(),
            error: false,
        });
        cx.notify();
    }

    fn sync_unit_state_from_document(&mut self, index: usize) {
        let Some(state_id) = self.unit_states.get(index).map(|state| state.id.clone()) else {
            return;
        };
        let Some(unit) = self
            .canonical_document
            .as_ref()
            .and_then(|document| document.units.iter().find(|unit| unit.id == state_id))
        else {
            return;
        };
        if let Some(state) = self.unit_states.get_mut(index) {
            state.kind = unit.kind;
            state.source_kind = unit.source_kind;
            state.source = unit.source.clone();
        }
    }

    fn refresh_selected_editor_views(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let state = self
            .unit_states
            .get(self.selected)
            .cloned()
            .context("当前内容单元不存在")?;
        let chapter = self
            .chapters
            .get(self.selected)
            .cloned()
            .context("当前章节不存在")?;
        let (preview, _, _) = preview_document_from_source(
            &chapter.html,
            &chapter.title,
            &state.id,
            state.source_kind,
            &state.source,
        )?;
        self.chapters[self.selected].html = preview;
        match self.tab {
            EditorTab::Source => self.sync_body_input(self.selected, window, cx),
            EditorTab::Preview | EditorTab::RichText => self.load_webview(self.tab, window, cx),
        }
        Ok(())
    }

    fn change_cover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cover_loading
            || self.canonical_document.is_none()
            || self.media_loading
            || self.media_modal.is_some()
            || self.active_write.is_some()
            || self.export_dialog_open
            || self.webview_build_gate.building
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing
        {
            return;
        }
        // Construct the native dialog while the GPUI window is available, but
        // defer `show` until this entity update has returned. On Windows the
        // dialog runs a nested message loop; showing it while GPUI holds its
        // app borrow makes every requested frame fail with a BorrowMutError.
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title("选择封面图片")
            .add_filter("封面图片", ["jpg", "jpeg", "png", "gif"])
            .open_single_file();

        self.cover_loading = true;
        cx.spawn_in(window, async move |view, cx| {
            let path = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = view.update_in(cx, |this, window, cx| {
                        this.cover_loading = false;
                        if let Some(action) = this.pending_cover_action.take() {
                            this.begin_action(action, window, cx);
                        } else {
                            cx.notify();
                        }
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.cover_loading = false;
                        let pending_action = this.pending_cover_action.take();
                        if pending_action == Some(PendingEditorAction::Close) {
                            cancel_application_exit(cx);
                        }
                        if pending_action == Some(PendingEditorAction::Export) {
                            this.pending_export_path = None;
                        }
                        this.notice = Some(Notice {
                            text: format!("无法打开封面文件选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    });
                    return;
                }
            };

            let task = cx.background_spawn(async move { CoverDraft::read(&path) });
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.cover_loading = false;
                let pending_action = this.pending_cover_action.take();
                match result {
                    Ok(cover) => {
                        this.cover_preview = image_format_from_mime(cover.mime()).map(|format| {
                            Arc::new(Image::from_bytes(format, (**cover.bytes()).clone()))
                        });
                        this.cover = Some(cover);
                        this.cover_dirty = true;
                        this.draft_generation = this.draft_generation.wrapping_add(1);
                        this.notice = Some(Notice {
                            text: "已选择新封面，保存后生效".to_string(),
                            error: false,
                        });
                        if let Some(action) = pending_action {
                            this.begin_action(action, window, cx);
                            return;
                        }
                    }
                    Err(error) => {
                        if pending_action == Some(PendingEditorAction::Close) {
                            cancel_application_exit(cx);
                        }
                        if pending_action == Some(PendingEditorAction::Export) {
                            this.pending_export_path = None;
                        }
                        this.notice = Some(Notice {
                            text: format!("无法使用所选封面：{error:#}"),
                            error: true,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn clear_cover(&mut self, cx: &mut Context<Self>) {
        if self.cover.is_none()
            || self.canonical_document.is_none()
            || self.cover_loading
            || self.media_loading
            || self.media_modal.is_some()
            || self.active_write.is_some()
            || self.export_dialog_open
            || self.webview_build_gate.building
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing
        {
            return;
        }
        self.cover = None;
        self.cover_preview = None;
        self.cover_dirty = true;
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.notice = Some(Notice {
            text: "已清除封面，保存后生效".to_string(),
            error: false,
        });
        cx.notify();
    }

    fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::Save, window, cx);
    }

    fn export_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_write.is_some() || self.export_dialog_open {
            self.notice = Some(Notice {
                text: "已有保存或导出操作正在后台执行，请稍候。".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        let title = self.title_input.read(cx).value().trim().to_string();
        // Do not call `show` while this entity update owns GPUI's App borrow.
        // The native save dialog pumps Windows messages and would otherwise
        // re-enter GPUI's frame callback with a conflicting mutable borrow.
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出《{title}》"))
            .set_filename(suggested_epub_filename(&title))
            .add_filter("EPUB 图书", ["epub"])
            .save_single_file();
        self.export_dialog_open = true;
        cx.spawn_in(window, async move |view, cx| {
            let target = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.export_dialog_open = false;
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.export_dialog_open = false;
                        this.notice = Some(Notice {
                            text: format!("无法打开保存文件选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.export_dialog_open = false;
                this.pending_export_path = Some(target);
                this.notice = Some(Notice {
                    text: "正在同步修改并导出 EPUB…".to_string(),
                    error: false,
                });
                this.begin_action(PendingEditorAction::Export, window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn perform_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_export_path.take() else {
            self.notice = Some(Notice {
                text: "导出失败：缺少目标文件路径".to_string(),
                error: true,
            });
            cx.notify();
            return;
        };
        self.start_editor_write(EditorWriteIntent::Export, Some(target), window, cx);
    }

    fn perform_save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_editor_write(EditorWriteIntent::Save, None, window, cx);
    }

    /// Freezes and validates the current UI draft. The expensive canonical EPUB
    /// generation, object-store writes, SQLite transaction and optional export
    /// are deliberately absent from this GPUI callback and run through
    /// `spawn_editor_write` on the application's dedicated runtime.
    fn freeze_editor_write(&mut self, cx: &mut Context<Self>) -> Option<(String, EditorWriteJob)> {
        let title = self.title_input.read(cx).value().trim().to_string();
        let author = self.author_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            self.notice = Some(Notice {
                text: "书名不能为空".to_string(),
                error: true,
            });
            cx.notify();
            return None;
        }
        // Refresh the selected chapter title. The body only needs flushing from
        // the source input: rich text has already crossed the exact
        // session/chapter/revision/request-id snapshot barrier above.
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return None;
        }
        let Some(document) = self.canonical_document.clone() else {
            self.notice = Some(Notice {
                text: "当前图书缺少统一文档模型，已拒绝使用旧 EPUB 草稿覆盖数据。".to_string(),
                error: true,
            });
            cx.notify();
            return None;
        };
        if document.units.len() != self.unit_states.len()
            || document.units.len() != self.chapters.len()
        {
            self.notice = Some(Notice {
                text: "编辑器内容单元与统一模型不一致，请重新打开编辑窗口。".to_string(),
                error: true,
            });
            cx.notify();
            return None;
        }

        let language = document.language.clone();
        let description = document.description.clone();
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法验证统一文档模型：{error:#}"), cx);
                return None;
            }
        };
        if let Err(error) = editor.set_metadata(
            title.clone(),
            (!author.is_empty()).then_some(author).into_iter().collect(),
            language,
            description,
        ) {
            self.set_error(format!("无法更新图书元数据：{error:#}"), cx);
            return None;
        }
        for (chapter, state) in self.chapters.iter().zip(&self.unit_states) {
            if let Err(error) =
                editor.update_unit_identity(&state.id, chapter.title.clone(), state.kind)
            {
                self.set_error(format!("无法更新内容单元：{error:#}"), cx);
                return None;
            }
        }
        let mut document = editor.into_document();
        let mut new_asset_bytes = self.pending_asset_bytes.clone();
        if self.cover_dirty {
            clear_editor_cover_assignment(&mut document);
            if let Some(cover) = self.cover.clone() {
                let mut asset = AssetRef::from_bytes(
                    AssetRole::Cover,
                    cover.mime(),
                    None,
                    cover.bytes().as_slice(),
                );
                asset.add_role(AssetRole::ContentImage);
                let asset_id = asset.id.clone();
                if let Some(existing) = document
                    .assets
                    .iter_mut()
                    .find(|existing| existing.id == asset.id)
                {
                    existing.add_role(AssetRole::Cover);
                    existing.add_role(AssetRole::ContentImage);
                } else {
                    document.assets.push(asset);
                }
                document.cover_asset_id = Some(asset_id.clone());
                new_asset_bytes.insert(asset_id, cover.bytes().clone());
            }
        }
        prune_editor_assets(&mut document);
        retain_referenced_pending_assets(&mut new_asset_bytes, &document);
        if let Err(error) = document.validate() {
            self.set_error(format!("编辑后的图书结构无效：{error}"), cx);
            return None;
        }
        Some((
            title,
            EditorWriteJob {
                document,
                new_asset_bytes,
                export_target: None,
            },
        ))
    }

    fn start_editor_write(
        &mut self,
        intent: EditorWriteIntent,
        export_target: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_write.is_some() {
            self.begin_action(
                if intent == EditorWriteIntent::Close {
                    PendingEditorAction::Close
                } else {
                    PendingEditorAction::Save
                },
                window,
                cx,
            );
            return;
        }
        let Some((title, mut job)) = self.freeze_editor_write(cx) else {
            if intent == EditorWriteIntent::Close {
                cancel_application_exit(cx);
            }
            return;
        };
        let target_display = export_target
            .as_ref()
            .map(|target| target.display().to_string());
        job.export_target = export_target;
        self.write_sequence = self.write_sequence.wrapping_add(1).max(1);
        let operation = ActiveEditorWrite {
            id: self.write_sequence,
            intent,
            draft_generation: self.draft_generation,
            title,
            target_display,
        };
        let operation_id = operation.id;
        self.active_write = Some(operation);
        self.close_after_write = intent == EditorWriteIntent::Close;
        self.set_rich_text_write_locked(true, cx);
        self.notice = Some(Notice {
            text: match intent {
                EditorWriteIntent::Save => "正在后台生成并保存 EPUB…",
                EditorWriteIntent::Export => "正在后台保存并导出 EPUB…",
                EditorWriteIntent::Close => "正在后台保存，成功后将安全关闭…",
            }
            .to_string(),
            error: false,
        });
        cx.notify();

        let task = spawn_editor_write(&self.services, job);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.finish_editor_write(operation_id, outcome, window, cx);
                });
            });
        })
        .detach();
    }

    fn finish_editor_write(
        &mut self,
        operation_id: u64,
        outcome: std::result::Result<
            Result<LibraryMutation<EditorWriteWorkerResult>>,
            tokio::task::JoinError,
        >,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_write.as_ref().map(|operation| operation.id) != Some(operation_id) {
            return;
        }
        let operation = self
            .active_write
            .take()
            .expect("matching editor write operation exists");
        let close_requested = std::mem::take(&mut self.close_after_write);
        match outcome {
            Ok(Ok(mutation)) => {
                let LibraryMutation {
                    value: result,
                    snapshot,
                    generation,
                } = mutation;
                let export_succeeded = result.export.succeeded();
                if !self.apply_persisted_editor_state(
                    result.document,
                    result.cover_bytes,
                    generation,
                    snapshot,
                    cx,
                ) {
                    if close_requested || operation.intent == EditorWriteIntent::Close {
                        cancel_application_exit(cx);
                    }
                    self.set_rich_text_write_locked(false, cx);
                    self.resume_close_confirmation(window, cx);
                    return;
                }
                // The persisted state is now authoritative for every chapter;
                // the in-memory dirty flags are reset in lockstep.
                self.modified_chapter_ids.clear();
                match &result.export {
                    EditorExportOutcome::NotRequested => {
                        self.notice = Some(Notice {
                            text: "已保存统一文档、目录和媒体修改。".to_string(),
                            error: false,
                        });
                    }
                    EditorExportOutcome::Exported(bytes_written) => {
                        let target = operation.target_display.as_deref().unwrap_or("目标文件");
                        self.notice = Some(Notice {
                            text: format!(
                                "《{}》已保存并导出到「{target}」（{} 字节）",
                                operation.title, bytes_written
                            ),
                            error: false,
                        });
                    }
                    EditorExportOutcome::Failed(error) => {
                        self.notice = Some(Notice {
                            text: format!("已保存修改，但导出《{}》失败：{error}", operation.title),
                            error: true,
                        });
                    }
                }
                match editor_write_follow_up(
                    &operation,
                    close_requested,
                    self.draft_generation,
                    true,
                    export_succeeded,
                ) {
                    EditorWriteFollowUp::StayOpen => {
                        if close_requested || operation.intent == EditorWriteIntent::Close {
                            cancel_application_exit(cx);
                        }
                        self.set_rich_text_write_locked(false, cx);
                        cx.notify();
                    }
                    EditorWriteFollowUp::Close => self.schedule_window_removal(window, cx),
                    EditorWriteFollowUp::SaveThenClose => {
                        self.set_rich_text_write_locked(false, cx);
                        self.notice = Some(Notice {
                            text: "保存期间草稿又发生变化，正在重新同步后关闭。".to_string(),
                            error: false,
                        });
                        self.begin_action(PendingEditorAction::Close, window, cx);
                    }
                }
            }
            Ok(Err(error)) => {
                if close_requested || operation.intent == EditorWriteIntent::Close {
                    cancel_application_exit(cx);
                }
                self.set_rich_text_write_locked(false, cx);
                self.notice = Some(Notice {
                    text: format!("保存失败，编辑器保持打开：{error:#}"),
                    error: true,
                });
                debug_assert_eq!(
                    editor_write_follow_up(
                        &operation,
                        close_requested,
                        self.draft_generation,
                        false,
                        false,
                    ),
                    EditorWriteFollowUp::StayOpen
                );
                cx.notify();
            }
            Err(error) => {
                if close_requested || operation.intent == EditorWriteIntent::Close {
                    cancel_application_exit(cx);
                }
                self.set_rich_text_write_locked(false, cx);
                self.notice = Some(Notice {
                    text: format!("保存任务异常停止，编辑器保持打开：{error}"),
                    error: true,
                });
                cx.notify();
            }
        }
        self.resume_close_confirmation(window, cx);
    }

    fn apply_persisted_editor_state(
        &mut self,
        document: BookDocument,
        cover_bytes: Option<Arc<Vec<u8>>>,
        generation: u64,
        snapshot: LibraryStore,
        cx: &mut Context<Self>,
    ) -> bool {
        if apply_editor_library_projection(
            &mut self.library,
            &mut self.library_projection_generation,
            generation,
            snapshot.clone(),
        ) {
            self.sync_ai_scope(cx);
        }
        let _ = self.library_view.update(cx, |library, cx| {
            library.refresh_after_projected_mutation(generation, snapshot, cx);
        });
        self.cover = cover_bytes.and_then(|bytes| CoverDraft::from_arc(bytes).ok());
        self.cover_preview = self.cover.as_ref().and_then(|cover| {
            image_format_from_mime(cover.mime())
                .map(|format| Arc::new(Image::from_bytes(format, (**cover.bytes()).clone())))
        });
        self.cover_dirty = false;
        self.pending_asset_bytes.clear();
        if document.units.len() != self.chapters.len() {
            tracing::warn!(
                book_id = self.book_id,
                canonical_units = document.units.len(),
                editor_chapters = self.chapters.len(),
                "saved canonical document no longer aligns with editor projection"
            );
            self.canonical_document = Some(document);
            self.unit_states.clear();
            self.notice = Some(Notice {
                text: "图书已保存，但编辑器内容单元已失配；请重新打开编辑窗口。".to_string(),
                error: true,
            });
            cx.notify();
            return false;
        }
        if let Err(error) = self.web_state.authorize_media(&document) {
            tracing::warn!(book_id = self.book_id, %error, "cannot refresh saved media authorization");
            self.canonical_document = Some(document);
            self.unit_states.clear();
            self.notice = Some(Notice {
                text: "图书已保存，但无法刷新媒体授权；请重新打开编辑窗口。".to_string(),
                error: true,
            });
            cx.notify();
            return false;
        }
        self.unit_states = EditorUnitState::from_document(&document);
        self.canonical_document = Some(document);
        for (index, chapter) in self.chapters.iter_mut().enumerate() {
            chapter.spine_index = Some(index);
            if let Some(unit) = self.unit_states.get(index) {
                chapter.title = self
                    .canonical_document
                    .as_ref()
                    .and_then(|document| document.units.iter().find(|item| item.id == unit.id))
                    .map(|unit| unit.title.clone())
                    .unwrap_or_else(|| chapter.title.clone());
            }
        }
        true
    }

    fn set_rich_text_write_locked(&self, locked: bool, cx: &mut Context<Self>) {
        let Some(webview) = self.editor_webview.as_ref() else {
            return;
        };
        let script = if locked {
            r#"document.activeElement && document.activeElement.blur(); document.querySelector('.ProseMirror')?.setAttribute('contenteditable','false'); document.querySelectorAll('[data-moye-editor-ui="toolbar"] button').forEach((button) => button.disabled = true);"#
        } else {
            r#"document.querySelector('.ProseMirror')?.setAttribute('contenteditable','true'); document.querySelectorAll('[data-moye-editor-ui="toolbar"] button').forEach((button) => button.disabled = false);"#
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(script) {
            tracing::warn!(locked, %error, "cannot update rich-text write lock");
        }
    }

    fn render_media_metadata_modal(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let modal = self.media_modal.clone()?;
        let view = cx.entity().clone();
        let kind = match modal.kind {
            MediaKind::Image => "图片",
            MediaKind::Audio => "音频",
            MediaKind::Video => "视频",
        };
        let (heading, confirm_label) = match modal.action {
            EditorMediaModalAction::InsertOrReplace {
                replace_block_id: Some(_),
                ..
            } => (format!("替换{kind}并编辑信息"), "替换"),
            EditorMediaModalAction::InsertOrReplace { .. } => {
                (format!("插入{kind}并编辑信息"), "插入")
            }
            EditorMediaModalAction::Edit { .. } => (format!("编辑{kind}信息"), "保存信息"),
        };
        let cancel_view = view.clone();
        let confirm_view = view.clone();
        Some(
            div()
                .id("editor-media-metadata-overlay")
                .absolute()
                .inset_0()
                .occlude()
                .bg(rgba(0x1f1d1a80))
                .v_flex()
                .items_center()
                .justify_center()
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .occlude()
                        .v_flex()
                        .w(px(460.))
                        .p_5()
                        .gap_4()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(SURFACE))
                        .shadow_lg()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child(heading),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_2()
                                .child(div().text_sm().text_color(rgb(INK)).child("标题（可选）"))
                                .child(Input::new(&modal.title_input)),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_2()
                                .child(div().text_sm().text_color(rgb(INK)).child("说明（可选）"))
                                .child(
                                    div()
                                        .h(px(120.))
                                        .child(Input::new(&modal.description_input).h_full()),
                                )
                                .child(div().text_xs().text_color(rgb(MUTED)).child(
                                    "图片说明会写入 alt 与 caption；所有说明都会参与全文检索。",
                                )),
                        )
                        .child(
                            div()
                                .h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("editor-media-metadata-cancel")
                                        .label("取消")
                                        .outline()
                                        .on_click(move |_, window, cx| {
                                            cancel_view.update(cx, |this, cx| {
                                                this.close_media_metadata_modal(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-media-metadata-confirm")
                                        .label(confirm_label)
                                        .primary()
                                        .on_click(move |_, window, cx| {
                                            confirm_view.update(cx, |this, cx| {
                                                this.confirm_media_metadata_modal(window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn release_webview(&mut self, cx: &mut Context<Self>) -> Option<WeakEntity<WebView>> {
        // In-flight asynchronous custom-protocol tasks may outlive the Wry
        // child. Mark the channel closed before hiding/dropping the WebView so
        // late object-store reads cannot call its responder afterwards.
        self.web_state.close_protocol();
        self.active_web_page = None;
        self.ai_selected_text = None;
        self.pending_snapshot = None;
        self.pending_ready_action = None;
        self.pending_cover_action = None;
        self.pending_export_path = None;
        self.pending_ai_request = None;
        self.pending_citation_source = None;
        self.pending_citation_navigation = None;
        self.pending_toc_id = None;
        self.pending_media_action = None;
        self.media_modal = None;
        self.ipc_sync_task.take();
        self.ready_timeout_task.take();
        self.editor_webview.take().map(|webview| {
            let weak = webview.downgrade();
            webview.update(cx, |webview, _| webview.hide());
            weak
        })
    }

    fn schedule_window_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
        self.ai_controller.close();
        self.closing_webview = self.release_webview(cx);
        cx.notify();
        window.refresh();
    }

    pub(super) fn handle_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.closing
            || self.close_prompt_open
            || self.close_without_saving_pending
            || self.save_and_close_pending()
        {
            return false;
        }
        if self.export_dialog_open {
            cancel_application_exit(cx);
            self.notice = Some(Notice {
                text: "请先关闭导出文件选择器，再关闭编辑器。".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        if self.webview_build_gate.request_close() {
            self.notice = Some(Notice {
                text: "正在等待编辑器初始化，完成后将询问是否保存。".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        // An explicitly accepted save/export may already be committing. Let it
        // settle before asking; closing must not silently authorize another save.
        if self.active_write.is_some() {
            self.close_confirmation_pending = true;
            self.notice = Some(Notice {
                text: "正在等待当前保存或导出完成，随后将询问是否保存并关闭。".to_string(),
                error: false,
            });
            cx.notify();
            return false;
        }
        self.close_prompt_open = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "关闭编辑窗口前，是否保存图书修改？",
            Some("选择“不保存”将放弃尚未保存的修改；选择“取消”继续编辑。"),
            &[
                PromptButton::ok("保存并关闭"),
                PromptButton::new("不保存"),
                PromptButton::cancel("取消"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            let answer = answer.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.close_prompt_open = false;
                match answer {
                    Ok(0) => this.begin_action(PendingEditorAction::Close, window, cx),
                    Ok(1) => {
                        // A previously requested Save/Export may have finished
                        // its snapshot barrier while the native prompt was open.
                        // Keep its result/projection callback alive, without
                        // authorizing any further write on close.
                        if this.active_write.is_some() {
                            this.close_without_saving_pending = true;
                            this.notice = Some(Notice {
                                text: "正在等待已开始的保存或导出结束，随后关闭编辑器。"
                                    .to_string(),
                                error: false,
                            });
                            cx.notify();
                        } else {
                            this.schedule_window_removal(window, cx);
                        }
                    }
                    Ok(_) => {
                        cancel_application_exit(cx);
                        cx.notify();
                    }
                    Err(_) => {
                        cancel_application_exit(cx);
                        this.notice = Some(Notice {
                            text: "保存确认未完成，编辑器保持打开。请重试关闭。".to_string(),
                            error: true,
                        });
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        false
    }

    /// Closes this editor because its book left the library.
    ///
    /// Unsaved edits are discarded: the document they belong to is already
    /// gone, so neither a save nor a confirmation may delay the close.
    pub(super) fn close_for_removed_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing || self.closing_for_removed_book {
            return;
        }
        self.closing_for_removed_book = true;
        if self.webview_build_gate.request_close() {
            return;
        }
        self.finish_removal_close(window, cx);
    }

    fn finish_removal_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        // Drop every pending save/close decision so teardown cannot authorize
        // another write against a book that no longer exists.
        self.close_prompt_open = false;
        self.close_after_write = false;
        self.close_confirmation_pending = false;
        self.schedule_window_removal(window, cx);
    }

    fn save_and_close_pending(&self) -> bool {
        self.close_after_write
            || self
                .pending_snapshot
                .as_ref()
                .is_some_and(|pending| pending.action == PendingEditorAction::Close)
            || [
                self.pending_ready_action,
                self.pending_cover_action,
                self.pending_media_action,
            ]
            .contains(&Some(PendingEditorAction::Close))
    }

    fn resume_close_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.close_without_saving_pending) {
            self.schedule_window_removal(window, cx);
        } else if std::mem::take(&mut self.close_confirmation_pending) {
            self.handle_window_close(window, cx);
        }
    }
}

#[cfg(test)]
mod close_tests;

#[cfg(test)]
mod xhtml_tests;

const DEFAULT_CHAPTER_HTML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>新章节</title></head>
  <body>
    <h2>新章节</h2>
    <p>在这里输入正文内容…</p>
  </body>
</html>"#;

/// Keeps each sizeable GPUI builder on its own call frame. In unoptimized
/// Windows builds a single monolithic editor render previously reserved almost
/// the entire 1 MiB main-thread stack before GPUI started layout.
#[inline(never)]
fn render_editor_section(render: impl FnOnce() -> gpui::AnyElement) -> gpui::AnyElement {
    render()
}

#[inline(never)]
fn render_editor_resize_handle(
    id: &'static str,
    pane: EditorResizablePane,
    view: Entity<EditorApp>,
) -> gpui::AnyElement {
    div()
        .id(id)
        .h_full()
        .w(px(EDITOR_RESIZE_HANDLE_WIDTH))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_col_resize()
        .hover(|this| this.bg(rgb(ACCENT_SOFT)))
        .on_drag(EditorPaneResizeDrag(pane), move |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| EditorPaneResizePreview)
        })
        .on_drag_move::<EditorPaneResizeDrag>(move |event, window, cx| {
            if event.drag(cx).0 != pane {
                return;
            }
            let pointer_x = event.event.position.x;
            let viewport_width = window.viewport_size().width;
            view.update(cx, |this, cx| {
                this.resize_pane_from_pointer(pane, pointer_x, viewport_width, cx);
            });
        })
        .child(div().h_full().w(px(1.)).bg(rgb(BORDER)))
        .into_any_element()
}

impl Render for EditorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.closing && !self.removal_scheduled {
            self.removal_scheduled = true;
            remove_window_after_current_frame(window, cx, self.closing_webview.take());
        }
        let view = cx.entity().clone();
        let write_busy = self.active_write.is_some() || self.export_dialog_open;
        let media_modal_open = self.media_modal.is_some();
        let (status_icon, status_text, status_color) = if self.closing
            || self.webview_build_gate.close_requested
        {
            (IconName::BookOpen, "正在安全关闭编辑器…", ACCENT)
        } else if let Some(operation) = self.active_write.as_ref() {
            match operation.intent {
                EditorWriteIntent::Save => (IconName::BookOpen, "正在后台保存…", ACCENT),
                EditorWriteIntent::Export => (IconName::BookOpen, "正在后台保存并导出…", ACCENT),
                EditorWriteIntent::Close => (IconName::BookOpen, "正在后台保存并关闭…", ACCENT),
            }
        } else if self.export_dialog_open {
            (IconName::ExternalLink, "正在选择导出位置…", ACCENT)
        } else if self.cover_loading {
            (IconName::BookOpen, "正在读取封面图片…", ACCENT)
        } else if self.media_loading {
            (IconName::BookOpen, "正在读取媒体文件…", ACCENT)
        } else if media_modal_open {
            (IconName::Settings2, "正在编辑媒体信息…", ACCENT)
        } else if let Some(pending) = self.pending_snapshot.as_ref() {
            match pending.action {
                PendingEditorAction::Save => (IconName::BookOpen, "正在同步并保存…", ACCENT),
                PendingEditorAction::Export => (IconName::BookOpen, "正在同步并导出…", ACCENT),
                PendingEditorAction::Close => (IconName::BookOpen, "正在保存并关闭…", ACCENT),
                _ => (IconName::BookOpen, "正在同步富文本…", ACCENT),
            }
        } else if self.pending_ready_action.is_some() {
            (IconName::BookOpen, "等待富文本视图就绪…", ACCENT)
        } else if self.webview_build_gate.building {
            (IconName::BookOpen, "正在初始化预览组件…", ACCENT)
        } else if self.tab.uses_webview() && self.editor_webview.is_none() {
            (IconName::TriangleAlert, "预览组件不可用", DANGER)
        } else if self.tab == EditorTab::RichText
            && !self.active_web_page.as_ref().is_some_and(|page| page.ready)
        {
            (IconName::BookOpen, "富文本视图正在加载…", ACCENT)
        } else {
            (IconName::CircleCheck, "编辑器已就绪", 0x376441)
        };
        let chapter_title = self.chapter_title_input.read(cx).value().trim().to_string();
        let editor_summary = if self.chapters.get(self.selected).is_some() {
            format!(
                "第 {} / {} 章 · {}",
                self.selected + 1,
                self.chapters.len(),
                if chapter_title.is_empty() {
                    "未命名章节"
                } else {
                    chapter_title.as_str()
                }
            )
        } else {
            "暂无章节".to_string()
        };
        let mode = match self.tab {
            EditorTab::Source => "源码编辑",
            EditorTab::Preview => "只读预览",
            EditorTab::RichText => "富文本 · 所见即所得",
        };
        let editor_context = if self.search_query.is_empty() {
            format!("{mode} · 手动保存")
        } else {
            format!(
                "{mode} · 检索 {} 个结果 · 手动保存",
                self.search_results.len()
            )
        };
        let status_bar = render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            editor_summary,
            editor_context,
        );

        let toc_rows = self
            .canonical_document
            .as_ref()
            .map(|document| editor_toc_rows(&document.toc))
            .unwrap_or_default();
        let selected_toc_id = self.current_toc_id();
        let chapter_list = if self.search_query.is_empty() {
            let listed_units = toc_rows
                .iter()
                .map(|row| row.unit_id.as_str())
                .collect::<HashSet<_>>();
            let mut items = toc_rows
                .iter()
                .map(|row| {
                    let selected = selected_toc_id.as_deref() == Some(row.id.as_str());
                    let id = row.id.clone();
                    let list_view = view.clone();
                    Button::new(SharedString::from(format!("editor-toc-{}", row.id)))
                        .debug_selector(|| format!("editor-toc-{}", row.id))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(38.))
                        .flex_none()
                        .justify_start()
                        .pl(px(18. + row.depth.min(4) as f32 * 14.))
                        .pr_3()
                        .py_2()
                        .when(selected, |this| this.bg(rgb(ACCENT_SOFT)))
                        .disabled(write_busy)
                        .on_click(move |_, window, cx| {
                            list_view.update(cx, |this, cx| this.select_toc(&id, window, cx));
                        })
                        .child(
                            div()
                                .w_full()
                                .min_w(px(0.))
                                .text_left()
                                .text_sm()
                                .line_clamp(2)
                                .text_color(if selected { rgb(ACCENT) } else { rgb(INK) })
                                .child(row.label.clone()),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let unlisted = self
                .unit_states
                .iter()
                .enumerate()
                .filter(|(_, unit)| !listed_units.contains(unit.id.as_str()))
                .collect::<Vec<_>>();
            if !toc_rows.is_empty() && !unlisted.is_empty() {
                items.push(
                    div()
                        .px_3()
                        .py_2()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("未列入目录的章节")
                        .into_any_element(),
                );
            }
            for (index, _) in unlisted {
                let Some(chapter) = self.chapters.get(index) else {
                    continue;
                };
                let selected = index == self.selected;
                let list_view = view.clone();
                items.push(
                    Button::new(("editor-chapter", index))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(38.))
                        .flex_none()
                        .justify_start()
                        .px_3()
                        .py_2()
                        .when(selected, |this| this.bg(rgb(ACCENT_SOFT)))
                        .disabled(write_busy)
                        .on_click(move |_, window, cx| {
                            list_view.update(cx, |this, cx| this.select_chapter(index, window, cx));
                        })
                        .child(
                            div()
                                .w_full()
                                .text_left()
                                .text_sm()
                                .line_clamp(2)
                                .child(chapter.title.clone()),
                        )
                        .into_any_element(),
                );
            }
            items
        } else {
            if self.search_results.is_empty() {
                vec![
                    div()
                        .p_4()
                        .text_center()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child("当前草稿没有匹配章节")
                        .into_any_element(),
                ]
            } else {
                self.search_results
                    .iter()
                    .enumerate()
                    .map(|(index, hit)| {
                        let selected = hit.chapter_index == self.selected;
                        let chapter_index = hit.chapter_index;
                        let result_view = view.clone();
                        Button::new(("editor-search-result", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(px(66.))
                            .justify_start()
                            .px_2()
                            .py_2()
                            .when(selected, |this| this.bg(rgb(ACCENT_SOFT)))
                            .disabled(write_busy)
                            .on_click(move |_, window, cx| {
                                result_view.update(cx, |this, cx| {
                                    this.select_chapter(chapter_index, window, cx)
                                });
                            })
                            .child(
                                div()
                                    .v_flex()
                                    .w_full()
                                    .min_w(px(0.))
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_left()
                                            .text_sm()
                                            .font_medium()
                                            .text_color(if selected {
                                                rgb(ACCENT)
                                            } else {
                                                rgb(INK)
                                            })
                                            .child(hit.chapter_title.clone()),
                                    )
                                    .child(
                                        div()
                                            .line_clamp(2)
                                            .text_left()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(hit.snippet.clone()),
                                    ),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }
        };

        let add_view = view.clone();
        let remove_view = view.clone();
        let move_up_view = view.clone();
        let move_down_view = view.clone();
        let indent_view = view.clone();
        let outdent_view = view.clone();
        let export_view = view.clone();
        let save_view = view.clone();
        let change_cover_view = view.clone();
        let clear_cover_view = view.clone();
        let structure_disabled = self.canonical_document.is_none()
            || self.media_loading
            || media_modal_open
            || write_busy
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing;
        let cover_thumbnail = if let Some(image) = self.cover_preview.as_ref() {
            img(image.clone())
                .size_full()
                .rounded(px(7.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .rounded(px(7.))
                .bg(rgb(SIDEBAR))
                .text_color(rgb(ACCENT))
                .child(Icon::new(IconName::BookOpen).small())
                .child(
                    div().text_xs().font_medium().child(
                        self.canonical_document
                            .as_ref()
                            .map(editor_book_format_label)
                            .unwrap_or("图书"),
                    ),
                )
                .into_any_element()
        };
        let cover_status = if self.cover_loading {
            "正在读取…"
        } else if self.cover_dirty && self.cover.is_none() {
            "封面已清除 · 待保存"
        } else if self.cover_dirty {
            "新封面 · 待保存"
        } else if self.cover.is_some() {
            "当前封面"
        } else {
            "暂无封面"
        };
        let change_cover_disabled = self.cover_loading
            || self.canonical_document.is_none()
            || self.media_loading
            || media_modal_open
            || write_busy
            || self.webview_build_gate.building
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing;
        let clear_cover_enabled =
            editor_clear_cover_enabled(self.cover.is_some(), change_cover_disabled);
        let cover_panel = render_editor_section(|| {
            div()
                .h_flex()
                .min_h(px(104.))
                .flex_none()
                .items_center()
                .gap_3()
                .px_3()
                .py_3()
                .border_b_1()
                .border_color(rgb(BORDER))
                .child(
                    div()
                        .w(px(56.))
                        .h(px(76.))
                        .flex_none()
                        .overflow_hidden()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(white())
                        .child(cover_thumbnail),
                )
                .child(
                    div()
                        .v_flex()
                        .min_w(px(0.))
                        .flex_1()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(if self.cover_dirty {
                                    rgb(ACCENT)
                                } else {
                                    rgb(MUTED)
                                })
                                .child(cover_status),
                        )
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .child(
                                    Button::new("editor-change-cover")
                                        .small()
                                        .outline()
                                        .label(if self.cover_loading {
                                            "读取中…"
                                        } else if self.cover.is_some() {
                                            "更换封面"
                                        } else {
                                            "选择封面"
                                        })
                                        .disabled(change_cover_disabled)
                                        .on_click(move |_, window, cx| {
                                            change_cover_view.update(cx, |this, cx| {
                                                this.change_cover(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-clear-cover")
                                        .small()
                                        .outline()
                                        .label("清除封面")
                                        .disabled(!clear_cover_enabled)
                                        .on_click(move |_, _, cx| {
                                            clear_cover_view
                                                .update(cx, |this, cx| this.clear_cover(cx));
                                        }),
                                ),
                        ),
                )
                .into_any_element()
        });

        let toolbar = render_editor_section(|| {
            div()
                .v_flex()
                .bg(rgb(SURFACE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .when_some(self.notice.as_ref(), |toolbar, notice| {
                    let (background, foreground, icon) = if notice.error {
                        (rgb(0xf7e1df), rgb(0x9f302c), IconName::TriangleAlert)
                    } else {
                        (rgb(0xe3efe5), rgb(0x376441), IconName::CircleCheck)
                    };
                    toolbar.child(
                        div()
                            .h_flex()
                            .min_h(px(36.))
                            .px_5()
                            .gap_2()
                            .border_t_1()
                            .border_color(rgb(BORDER))
                            .bg(background)
                            .text_xs()
                            .text_color(foreground)
                            .child(Icon::new(icon).small())
                            .child(notice.text.clone()),
                    )
                })
                .child(
                    div()
                        .h_flex()
                        .h(px(64.))
                        .px_5()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(72.))
                                        .text_sm()
                                        .text_color(rgb(MUTED))
                                        .child("书名"),
                                )
                                .child(
                                    div()
                                        .w(px(280.))
                                        .child(Input::new(&self.title_input).disabled(write_busy)),
                                )
                                .child(
                                    div()
                                        .w(px(56.))
                                        .text_sm()
                                        .text_color(rgb(MUTED))
                                        .child("作者"),
                                )
                                .child(
                                    div()
                                        .w(px(200.))
                                        .child(Input::new(&self.author_input).disabled(write_busy)),
                                ),
                        )
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .child(
                                    Button::new("editor-export")
                                        .icon(IconName::ExternalLink)
                                        .label("导出 EPUB")
                                        .outline()
                                        .disabled(write_busy || media_modal_open)
                                        .on_click(move |_, window, cx| {
                                            export_view.update(cx, |this, cx| {
                                                this.export_book(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-save")
                                        .label(if write_busy { "处理中…" } else { "保存" })
                                        .primary()
                                        .disabled(write_busy || media_modal_open)
                                        .on_click(move |_, window, cx| {
                                            save_view
                                                .update(cx, |this, cx| this.save_draft(window, cx));
                                        }),
                                ),
                        ),
                )
                .into_any_element()
        });

        let left_sidebar_width = self.left_sidebar_width;
        let sidebar = render_editor_section(|| {
            div()
                .v_flex()
                .w(left_sidebar_width)
                .h_full()
                .flex_none()
                .border_r_1()
                .border_color(rgb(BORDER))
                .bg(rgb(SIDEBAR))
                .child(
                    div()
                        .h_flex()
                        .h(px(48.))
                        .px_4()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .text_sm()
                                .font_semibold()
                                .text_color(rgb(INK))
                                .child(Icon::new(IconName::Menu).small())
                                .child(if self.search_query.is_empty() {
                                    format!("目录（{}）", toc_rows.len())
                                } else {
                                    format!("搜索结果（{}）", self.search_results.len())
                                }),
                        )
                        .child(
                            div()
                                .h_flex()
                                .gap_1()
                                .child(
                                    Button::new("editor-move-chapter-up")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowUp)
                                        .tooltip("上移阅读顺序")
                                        .disabled(structure_disabled || self.selected == 0)
                                        .on_click(move |_, window, cx| {
                                            move_up_view.update(cx, |this, cx| {
                                                this.move_chapter(-1, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-move-chapter-down")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowDown)
                                        .tooltip("下移阅读顺序")
                                        .disabled(
                                            structure_disabled
                                                || self.selected + 1 >= self.chapters.len(),
                                        )
                                        .on_click(move |_, window, cx| {
                                            move_down_view.update(cx, |this, cx| {
                                                this.move_chapter(1, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-outdent-toc")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowLeft)
                                        .tooltip("提升目录层级")
                                        .disabled(structure_disabled)
                                        .on_click(move |_, window, cx| {
                                            outdent_view.update(cx, |this, cx| {
                                                this.outdent_toc(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-indent-toc")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowRight)
                                        .tooltip("缩进为上一项的子目录")
                                        .disabled(structure_disabled || selected_toc_id.is_none())
                                        .on_click(move |_, window, cx| {
                                            indent_view
                                                .update(cx, |this, cx| this.indent_toc(window, cx));
                                        }),
                                )
                                .child(
                                    Button::new("editor-add-chapter")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Plus)
                                        .tooltip("添加章节")
                                        .disabled(structure_disabled)
                                        .on_click(move |_, window, cx| {
                                            add_view.update(cx, |this, cx| {
                                                this.add_chapter(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-remove-chapter")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Delete)
                                        .tooltip("删除本章")
                                        .disabled(structure_disabled || self.chapters.len() <= 1)
                                        .on_click(move |_, window, cx| {
                                            remove_view.update(cx, |this, cx| {
                                                this.remove_chapter(window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(BORDER))
                        .child(
                            Input::new(&self.search_input)
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true)
                                .disabled(write_busy),
                        ),
                )
                .child(cover_panel)
                .child(
                    div()
                        .id("editor-chapter-scroll")
                        .flex_1()
                        .min_h(px(0.))
                        .p_2()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .v_flex()
                                .h_auto()
                                .flex_none()
                                .gap_0p5()
                                .children(chapter_list),
                        ),
                )
                .into_any_element()
        });

        let current_source_kind = self
            .unit_states
            .get(self.selected)
            .map(|unit| unit.source_kind)
            .unwrap_or(SourceKind::Html);
        let current_unit_kind = self
            .unit_states
            .get(self.selected)
            .map(|unit| unit.kind)
            .unwrap_or(ContentUnitKind::Chapter);
        let kind_view = view.clone();
        let markdown_view = view.clone();
        let html_view = view.clone();
        let chapter_title_bar = render_editor_section(|| {
            div()
                .h_flex()
                .h(px(50.))
                .px_4()
                .items_center()
                .gap_3()
                .border_b_1()
                .border_color(rgb(BORDER))
                .child(
                    div().text_xs().text_color(rgb(MUTED)).child(
                        if self
                            .chapters
                            .get(self.selected)
                            .is_some_and(|c| c.spine_index.is_none())
                        {
                            "新章节".to_string()
                        } else {
                            "已有章节".to_string()
                        },
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(Input::new(&self.chapter_title_input).disabled(write_busy)),
                )
                .child(
                    Button::new("editor-unit-kind")
                        .small()
                        .outline()
                        .label(unit_kind_label(current_unit_kind))
                        .tooltip("切换章节、页面、幻灯片或工作表类型")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            kind_view.update(cx, |this, cx| this.cycle_unit_kind(window, cx));
                        }),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_1()
                        .child(
                            Button::new("editor-source-markdown")
                                .small()
                                .label("Markdown")
                                .when(current_source_kind == SourceKind::Markdown, |button| {
                                    button.primary()
                                })
                                .when(current_source_kind != SourceKind::Markdown, |button| {
                                    button.outline()
                                })
                                .disabled(structure_disabled)
                                .on_click(move |_, window, cx| {
                                    markdown_view.update(cx, |this, cx| {
                                        this.set_source_kind(SourceKind::Markdown, window, cx)
                                    });
                                }),
                        )
                        .child(
                            Button::new("editor-source-html")
                                .small()
                                .label("HTML")
                                .when(current_source_kind == SourceKind::Html, |button| {
                                    button.primary()
                                })
                                .when(current_source_kind != SourceKind::Html, |button| {
                                    button.outline()
                                })
                                .disabled(structure_disabled)
                                .on_click(move |_, window, cx| {
                                    html_view.update(cx, |this, cx| {
                                        this.set_source_kind(SourceKind::Html, window, cx)
                                    });
                                }),
                        ),
                )
                .into_any_element()
        });

        let current_media = self
            .unit_states
            .get(self.selected)
            .and_then(|state| {
                self.canonical_document.as_ref().and_then(|document| {
                    document
                        .units
                        .iter()
                        .find(|unit| unit.id == state.id)
                        .map(|unit| media_blocks(&unit.document.blocks))
                })
            })
            .unwrap_or_default();
        let image_view = view.clone();
        let audio_view = view.clone();
        let video_view = view.clone();
        let media_controls = current_media
            .into_iter()
            .enumerate()
            .flat_map(|(index, media)| {
                let edit_view = view.clone();
                let replace_view = view.clone();
                let delete_view = view.clone();
                let edit_target = media.clone();
                let replace_target = media.clone();
                let delete_target = media.clone();
                let kind = match media.kind {
                    MediaKind::Image => "图片",
                    MediaKind::Audio => "音频",
                    MediaKind::Video => "视频",
                };
                [
                    Button::new(("editor-edit-media-metadata", index))
                        .xsmall()
                        .outline()
                        .icon(IconName::Settings2)
                        .label(format!("{kind} · {}", media.label))
                        .tooltip("编辑此媒体的标题与说明")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            edit_view.update(cx, |this, cx| {
                                this.edit_media_metadata(edit_target.clone(), window, cx)
                            });
                        })
                        .into_any_element(),
                    Button::new(("editor-replace-media", index))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Replace)
                        .tooltip("替换此媒体文件并确认信息")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            replace_view.update(cx, |this, cx| {
                                this.replace_media(replace_target.clone(), window, cx)
                            });
                        })
                        .into_any_element(),
                    Button::new(("editor-delete-media", index))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Delete)
                        .tooltip("删除此媒体块")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.request_delete_media(delete_target.clone(), window, cx)
                            });
                        })
                        .into_any_element(),
                ]
            })
            .collect::<Vec<_>>();
        let media_toolbar = render_editor_section(|| {
            div()
                .id("editor-media-toolbar")
                .h_flex()
                .min_h(px(42.))
                .flex_none()
                .gap_1()
                .px_4()
                .py_1()
                .border_b_1()
                .border_color(rgb(BORDER))
                // gpui-component 0.5.1's `overflow_x_scrollbar` moves this
                // element's styles onto a `size_full` wrapper and clears the
                // inner flex-row style. That makes this fixed-height toolbar
                // consume the whole editor and stacks its controls vertically.
                .overflow_x_scroll()
                .child(div().text_xs().text_color(rgb(MUTED)).child("媒体"))
                .child(
                    Button::new("editor-insert-image")
                        .xsmall()
                        .outline()
                        .label("+ 图片")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            image_view.update(cx, |this, cx| {
                                this.insert_media(MediaKind::Image, window, cx)
                            });
                        }),
                )
                .child(
                    Button::new("editor-insert-audio")
                        .xsmall()
                        .outline()
                        .label("+ 音频")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            audio_view.update(cx, |this, cx| {
                                this.insert_media(MediaKind::Audio, window, cx)
                            });
                        }),
                )
                .child(
                    Button::new("editor-insert-video")
                        .xsmall()
                        .outline()
                        .label("+ 视频")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            video_view.update(cx, |this, cx| {
                                this.insert_media(MediaKind::Video, window, cx)
                            });
                        }),
                )
                .children(media_controls)
                .into_any_element()
        });

        // Three editing views: raw source, read-only preview, and WYSIWYG.
        let source_view = view.clone();
        let preview_view = view.clone();
        let richtext_view = view.clone();
        let tab_bar = render_editor_section(|| {
            TabBar::new("editor-tabs")
                .underline()
                .selected_index(self.tab as usize)
                .on_click(move |index, window, cx| {
                    let tab = match index {
                        0 => EditorTab::Source,
                        1 => EditorTab::Preview,
                        _ => EditorTab::RichText,
                    };
                    let view = match tab {
                        EditorTab::Source => source_view.clone(),
                        EditorTab::Preview => preview_view.clone(),
                        EditorTab::RichText => richtext_view.clone(),
                    };
                    view.update(cx, |this, cx| this.switch_tab(tab, window, cx));
                })
                .child(Tab::new().label("源码"))
                .child(Tab::new().label("预览"))
                .child(Tab::new().label("富文本"))
                .into_any_element()
        });

        let editor_body = render_editor_section(|| match self.tab {
            EditorTab::Source => div()
                .flex_1()
                .min_h(px(0.))
                .bg(rgb(0xfbfaf7))
                .p_2()
                .child(Input::new(&self.body_input).h_full().disabled(write_busy))
                .into_any_element(),
            EditorTab::Preview | EditorTab::RichText => match &self.editor_webview {
                Some(webview) => div()
                    .flex_1()
                    .min_h(px(0.))
                    .bg(rgb(0xfbfaf7))
                    .p_2()
                    .child(
                        div()
                            .size_full()
                            .overflow_hidden()
                            .rounded(px(10.))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(white())
                            .child(webview.clone()),
                    )
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .min_h(px(0.))
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(rgb(MUTED))
                    .child(Icon::new(IconName::Settings2).large())
                    .child(div().text_sm().child("正在加载预览视图…"))
                    .into_any_element(),
            },
        });

        let content = render_editor_section(|| {
            div()
                .v_flex()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .bg(rgb(0xfbfaf7))
                .child(chapter_title_bar)
                .child(media_toolbar)
                .child(tab_bar)
                .child(editor_body)
                .into_any_element()
        });
        let media_modal = self.render_media_metadata_modal(cx);
        let left_resize_handle = render_editor_resize_handle(
            "editor-left-resize",
            EditorResizablePane::Left,
            view.clone(),
        );
        let right_resize_handle = (!self.ai_sidebar_collapsed).then(|| {
            render_editor_resize_handle(
                "editor-right-resize",
                EditorResizablePane::Right,
                view.clone(),
            )
        });

        render_editor_section(|| {
            div()
                .relative()
                .v_flex()
                .size_full()
                .bg(rgb(PAPER))
                .child(toolbar)
                .child(
                    div()
                        .h_flex()
                        .items_start()
                        .flex_1()
                        .min_h(px(0.))
                        .child(sidebar)
                        .child(left_resize_handle)
                        .child(content)
                        .when_some(right_resize_handle, |this, handle| this.child(handle))
                        .child(self.ai_sidebar.clone()),
                )
                .child(status_bar)
                .when_some(media_modal, |this, modal| this.child(modal))
                .into_any_element()
        })
    }
}

fn editor_reference_hints(
    book_id: &str,
    chapters: &[EditorChapter],
    unit_states: &[EditorUnitState],
    selected: usize,
    live_selected_title: Option<&str>,
    selected_text: Option<&str>,
    revision: u64,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..chapters.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != selected, *index));
    indices
        .into_iter()
        .filter_map(|index| {
            let chapter = chapters.get(index)?;
            let unit_id = unit_states
                .get(index)
                .map(|unit| unit.id.clone())
                .unwrap_or_else(|| chapter.href.clone());
            let title = if index == selected {
                live_selected_title
                    .filter(|title| !title.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| chapter.title.clone())
            } else {
                chapter.title.clone()
            };
            let label = if index == selected {
                if selected_text.is_some() {
                    format!("当前章节高亮 · {title}")
                } else {
                    format!("当前章节 · {title}")
                }
            } else {
                title
            };
            let mut reference = AiReferenceHint::chapter(book_id, unit_id, index, label);
            if index == selected {
                reference.frozen_text = selected_text.map(str::to_string);
            }
            reference.revision = Some(revision);
            Some(reference)
        })
        .collect()
}

fn freeze_editor_reference_hints(
    book_id: &str,
    chapters: &[EditorChapter],
    unit_states: &[EditorUnitState],
    selected: usize,
    selected_text: Option<&str>,
    revision: u64,
    references: &mut [AiReferenceHint],
) {
    for reference in references {
        if reference.book_id != book_id {
            continue;
        }
        let index = unit_states
            .iter()
            .position(|unit| unit.id == reference.unit_id)
            .or(reference.unit_index)
            .filter(|index| *index < chapters.len());
        let Some(chapter) = index.and_then(|index| chapters.get(index)) else {
            continue;
        };
        reference.frozen_text = if index == Some(selected) {
            selected_text
                .map(str::to_string)
                .or_else(|| Some(searchable_text_from_xhtml(&chapter.html)))
        } else {
            Some(searchable_text_from_xhtml(&chapter.html))
        };
        if index == Some(selected) && selected_text.is_some() {
            let title = chapter.title.trim();
            reference.label = if title.is_empty() {
                "当前章节高亮".to_string()
            } else {
                format!("当前章节高亮 · {title}")
            };
        }
        reference.revision = Some(revision);
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;
    use moye_epub_editor::document::{BlockDocument, ContentUnit};
    use tempfile::tempdir;

    #[test]
    fn pane_widths_respect_limits_and_preserve_the_editor_center() {
        assert_eq!(
            constrained_editor_pane_width(
                px(100.),
                px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
                px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
                px(1400.),
                px(AI_SIDEBAR_WIDTH),
            ),
            px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH)
        );
        assert_eq!(
            constrained_editor_pane_width(
                px(900.),
                px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
                px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
                px(1400.),
                px(AI_SIDEBAR_WIDTH),
            ),
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH)
        );

        let viewport = px(1100.);
        let left = constrained_editor_pane_width(
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
            px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
            viewport,
            px(AI_SIDEBAR_MAX_WIDTH),
        );
        let right = constrained_editor_pane_width(
            px(AI_SIDEBAR_MAX_WIDTH),
            px(AI_SIDEBAR_MIN_WIDTH),
            px(AI_SIDEBAR_MAX_WIDTH),
            viewport,
            left,
        );
        assert!(
            viewport - left - right - px(EDITOR_RESIZE_HANDLE_WIDTH * 2.)
                >= px(EDITOR_CONTENT_MIN_WIDTH)
        );
    }

    #[test]
    fn resize_handle_center_preserves_each_sidebar_width() {
        let half_handle = px(EDITOR_RESIZE_HANDLE_WIDTH / 2.);
        let viewport = px(1400.);
        let left = px(EDITOR_LEFT_SIDEBAR_DEFAULT_WIDTH);
        let right = px(AI_SIDEBAR_WIDTH);

        assert_eq!(
            editor_pane_width_from_pointer(EditorResizablePane::Left, left + half_handle, viewport,),
            left
        );
        assert_eq!(
            editor_pane_width_from_pointer(
                EditorResizablePane::Right,
                viewport - right - half_handle,
                viewport,
            ),
            right
        );
    }

    fn close_test_data_dir(data_dir: tempfile::TempDir) {
        #[cfg(target_os = "windows")]
        {
            // SQLite/object-store worker handles can take one scheduler turn
            // to disappear after a runtime joins while the full parallel test
            // suite is active. Keep ownership of the exact tempfile path and
            // require it to become removable instead of silently leaking it.
            let path = data_dir.keep();
            assert!(path.starts_with(std::env::temp_dir()));
            let mut last_error = None;
            for _ in 0..100 {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => return,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                    Err(error) => last_error = Some(error),
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!(
                "temporary editor data directory remained locked: {}: {}",
                path.display(),
                last_error.expect("at least one remove attempt")
            );
        }
        #[cfg(not(target_os = "windows"))]
        data_dir.close().unwrap();
    }

    fn document_with_audio(book_id: &str, asset: AssetRef) -> BookDocument {
        let source = format!(
            "<audio controls=\"controls\" src=\"moye-asset:{}\"></audio>",
            asset.id
        );
        let mut document = BookDocument::created(book_id, "媒体测试");
        document.units.push(ContentUnit::new(
            "unit-1",
            ContentUnitKind::Chapter,
            "第一章",
            SourceKind::Html,
            source,
            BlockDocument::new(vec![Block::Audio {
                id: "block-audio".to_string(),
                asset_id: asset.id.clone(),
                title: None,
                caption: Vec::new(),
            }]),
        ));
        document.assets.push(asset);
        document
    }

    fn protocol_request(
        uri: impl AsRef<str>,
        range: Option<&str>,
    ) -> gpui_component::wry::http::Request<()> {
        let mut builder = gpui_component::wry::http::Request::builder().uri(uri.as_ref());
        if let Some(range) = range {
            builder = builder.header("Range", range);
        }
        builder.body(()).unwrap()
    }

    fn editor_ipc_identity(state: &EditorWebState, revision: u64) -> (String, String) {
        (
            state.session_id().to_string(),
            state.page(revision).unwrap().chapter_id,
        )
    }

    #[test]
    fn empty_cover_placeholder_uses_the_document_format() {
        let formats = [
            (BookFormat::Epub, "EPUB"),
            (BookFormat::Pdf, "PDF"),
            (BookFormat::Doc, "DOC"),
            (BookFormat::Docx, "DOCX"),
            (BookFormat::Pptx, "PPTX"),
            (BookFormat::Xlsx, "XLSX"),
            (BookFormat::Mobi, "MOBI"),
            (BookFormat::Azw, "AZW"),
            (BookFormat::Azw3, "AZW3"),
        ];
        for (format, expected) in formats {
            let document = BookDocument::new(
                "book-a",
                "测试",
                BookSource::imported(format, "original-a", Some("sample.bin".to_string())),
            );
            assert_eq!(editor_book_format_label(&document), expected);
        }
        assert_eq!(
            editor_book_format_label(&BookDocument::created("book-a", "测试")),
            "EPUB"
        );
    }

    #[test]
    fn clearing_cover_removes_the_assignment_and_only_retains_content_use() {
        let bytes = b"cover-bytes";
        let mut cover = AssetRef::from_bytes(AssetRole::Cover, "image/png", None, bytes);
        cover.add_role(AssetRole::ContentImage);
        let cover_id = cover.id.clone();
        let mut document = BookDocument::created("book-a", "封面测试");
        document.cover_asset_id = Some(cover_id.clone());
        document.assets.push(cover);

        clear_editor_cover_assignment(&mut document);
        prune_editor_assets(&mut document);
        assert!(document.cover_asset_id.is_none());
        assert!(document.assets.is_empty());
        document.validate().unwrap();

        let mut referenced = AssetRef::from_bytes(AssetRole::Cover, "image/png", None, bytes);
        referenced.add_role(AssetRole::ContentImage);
        let referenced_id = referenced.id.clone();
        document.cover_asset_id = Some(referenced_id.clone());
        document.assets.push(referenced);
        document.units.push(ContentUnit::new(
            "unit-a",
            ContentUnitKind::Chapter,
            "第一章",
            SourceKind::Html,
            "<img src=\"moye-asset:image\" />",
            BlockDocument::new(vec![Block::Image {
                id: "image-block".to_string(),
                asset_id: referenced_id.clone(),
                alt: "封面插图".to_string(),
                title: None,
                caption: Vec::new(),
            }]),
        ));

        clear_editor_cover_assignment(&mut document);
        prune_editor_assets(&mut document);
        let retained = document.find_asset(&referenced_id).unwrap();
        assert!(!retained.has_role(AssetRole::Cover));
        assert!(retained.has_role(AssetRole::ContentImage));
        document.validate().unwrap();
    }

    #[test]
    fn clear_cover_control_requires_a_cover_and_an_idle_editor() {
        assert!(editor_clear_cover_enabled(true, false));
        assert!(!editor_clear_cover_enabled(false, false));
        assert!(!editor_clear_cover_enabled(true, true));
    }

    #[test]
    fn editor_ai_references_use_stable_unit_ids_and_freeze_multiple_unsaved_chapters() {
        let chapters = vec![
            EditorChapter {
                title: "第一章".to_string(),
                href: "Text/one.xhtml".to_string(),
                spine_index: Some(0),
                html: "<body><p>未保存的一</p></body>".to_string(),
            },
            EditorChapter {
                title: "第二章".to_string(),
                href: "Text/two.xhtml".to_string(),
                spine_index: Some(1),
                html: "<body><p>未保存的二</p></body>".to_string(),
            },
        ];
        let units = vec![
            EditorUnitState {
                id: "unit-one".to_string(),
                kind: ContentUnitKind::Chapter,
                source_kind: SourceKind::Html,
                source: chapters[0].html.clone(),
            },
            EditorUnitState {
                id: "unit-two".to_string(),
                kind: ContentUnitKind::Chapter,
                source_kind: SourceKind::Html,
                source: chapters[1].html.clone(),
            },
        ];

        let options = editor_reference_hints(
            "book-a",
            &chapters,
            &units,
            1,
            Some("第二章（改）"),
            Some("精确高亮"),
            7,
        );
        assert_eq!(options[0].unit_id, "unit-two");
        assert_eq!(options[1].unit_id, "unit-one");
        assert!(options[0].label.contains("第二章（改）"));

        let mut selected = options;
        selected.push(AiReferenceHint::chapter(
            "book-other",
            "unit-other",
            0,
            "其它书",
        ));
        freeze_editor_reference_hints(
            "book-a",
            &chapters,
            &units,
            1,
            Some("精确高亮"),
            9,
            &mut selected,
        );
        assert_eq!(selected[0].frozen_text.as_deref(), Some("精确高亮"));
        assert_eq!(selected[1].frozen_text.as_deref(), Some("未保存的一"));
        assert!(selected[0].label.starts_with("当前章节高亮"));
        assert_eq!(selected[0].revision, Some(9));
        assert!(selected[2].frozen_text.is_none());
    }

    #[test]
    fn markdown_source_builds_a_sanitized_preview_without_changing_source_kind() {
        let (preview, source, document) = preview_document_from_source(
            DEFAULT_CHAPTER_HTML,
            "测试章",
            "unit-test",
            SourceKind::Markdown,
            "# 标题\n\n正文 **加粗**",
        )
        .unwrap();

        assert!(preview.contains("<h1"));
        assert!(preview.contains("正文"));
        assert!(source.contains("# 标题"));
        assert_eq!("标题\n\n正文 加粗", document.plain_text());
        assert!(
            preview_document_from_source(
                DEFAULT_CHAPTER_HTML,
                "测试章",
                "unit-test",
                SourceKind::Markdown,
                "正文\n\n<script>alert(1)</script>",
            )
            .is_err()
        );
    }

    #[test]
    fn editor_projection_uses_the_canonical_ast_without_loading_original_resources() {
        let mut document = BookDocument::created("book-a", "统一模型");
        document.units.push(ContentUnit::new(
            "unit-a",
            ContentUnitKind::Chapter,
            "第一章",
            SourceKind::Markdown,
            "# 过期源码",
            BlockDocument::new(vec![Block::Paragraph {
                id: "paragraph-a".to_string(),
                content: vec![Inline::Text {
                    value: "来自规范化 AST".to_string(),
                }],
            }]),
        ));

        let chapters = editor_chapters_from_document(&document).unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].href, "chapter-1.xhtml");
        assert_eq!(chapters[0].spine_index, Some(0));
        assert!(chapters[0].html.contains("来自规范化 AST"));
        assert!(!chapters[0].html.contains("过期源码"));
        assert!(!chapters[0].html.contains("stylesheet"));
    }

    #[test]
    fn media_helpers_find_replace_and_delete_nested_blocks() {
        let mut blocks = vec![Block::BlockQuote {
            id: "quote".to_string(),
            blocks: vec![Block::Audio {
                id: "audio".to_string(),
                asset_id: "old".to_string(),
                title: Some("讲解".to_string()),
                caption: Vec::new(),
            }],
        }];

        assert_eq!(
            vec![EditorMediaBlock {
                id: "audio".to_string(),
                kind: MediaKind::Audio,
                label: "讲解".to_string(),
                title: Some("讲解".to_string()),
                description: None,
            }],
            media_blocks(&blocks)
        );
        assert!(replace_media_asset(&mut blocks, "audio", "new"));
        assert!(matches!(
            &blocks[0],
            Block::BlockQuote { blocks, .. }
                if matches!(&blocks[0], Block::Audio { asset_id, .. } if asset_id == "new")
        ));
        assert!(delete_media(&mut blocks, "audio"));
        assert!(media_blocks(&blocks).is_empty());
    }

    #[test]
    fn media_metadata_updates_alt_caption_and_visible_search_text() {
        let mut blocks = vec![Block::Image {
            id: "diagram".to_string(),
            asset_id: "asset-image".to_string(),
            alt: "旧说明".to_string(),
            title: None,
            caption: Vec::new(),
        }];
        let metadata =
            EditorMediaMetadataDraft::from_inputs("  人物关系图  ", "  第一行说明\n第二行说明  ")
                .unwrap();

        assert!(update_media_metadata(&mut blocks, "diagram", &metadata));
        assert!(matches!(
            &blocks[0],
            Block::Image { alt, title, caption, .. }
                if alt == "第一行说明\n第二行说明"
                    && title.as_deref() == Some("人物关系图")
                    && editor_caption_text(caption).as_deref()
                        == Some("第一行说明\n第二行说明")
        ));
        let document = BlockDocument::new(blocks.clone());
        assert!(document.plain_text().contains("第一行说明"));
        assert!(document.plain_text().contains("人物关系图"));
        let media = media_blocks(&blocks);
        assert_eq!(media[0].title.as_deref(), Some("人物关系图"));
        assert_eq!(
            media[0].description.as_deref(),
            Some("第一行说明\n第二行说明")
        );
        assert!(EditorMediaMetadataDraft::from_inputs("ok\u{0007}", "").is_err());
        assert!(
            EditorMediaMetadataDraft::from_inputs(
                &"题".repeat(MAX_EDITOR_MEDIA_TITLE_CHARS + 1),
                ""
            )
            .is_err()
        );
    }

    #[test]
    fn existing_inline_image_metadata_can_be_edited_without_new_bytes() {
        let mut blocks = vec![Block::Paragraph {
            id: "paragraph".to_string(),
            content: vec![Inline::Image {
                asset_id: "asset-inline".to_string(),
                alt: "旧说明".to_string(),
                title: Some("旧标题".to_string()),
            }],
        }];
        let metadata = EditorMediaMetadataDraft::from_inputs("新标题", "新说明").unwrap();
        assert!(update_media_metadata(
            &mut blocks,
            "inline-image:asset-inline",
            &metadata,
        ));
        assert!(matches!(
            &blocks[0],
            Block::Paragraph { content, .. }
                if matches!(
                    &content[0],
                    Inline::Image { alt, title, .. }
                        if alt == "新说明" && title.as_deref() == Some("新标题")
                )
        ));
    }

    #[test]
    fn toc_depth_and_unit_kind_cycle_are_deterministic() {
        let mut parent = TocNode::new(
            "parent",
            "上级",
            moye_epub_editor::document::TocTarget::unit("unit-a"),
        );
        parent.children.push(TocNode::new(
            "child",
            "下级",
            moye_epub_editor::document::TocTarget::unit("unit-b"),
        ));
        assert_eq!(0, toc_depth_for_unit(&[parent.clone()], "unit-a"));
        assert_eq!(1, toc_depth_for_unit(&[parent], "unit-b"));

        let mut kind = ContentUnitKind::Chapter;
        for expected in [
            ContentUnitKind::Section,
            ContentUnitKind::Page,
            ContentUnitKind::Slide,
            ContentUnitKind::Worksheet,
            ContentUnitKind::Chapter,
        ] {
            kind = next_unit_kind(kind);
            assert_eq!(expected, kind);
        }
    }

    #[test]
    fn suggested_export_filename_is_epub_and_windows_safe() {
        assert_eq!(suggested_epub_filename("Rust/入门"), "Rust_入门.epub");
        assert_eq!(
            suggested_epub_filename("A<B>C:D\"E/F\\G|H?I*J"),
            "A_B_C_D_E_F_G_H_I_J.epub"
        );
        assert_eq!(suggested_epub_filename("  墨页.EPUB  "), "墨页.epub");
        assert_eq!(suggested_epub_filename(" . . "), "book.epub");
        assert_eq!(suggested_epub_filename("CON"), "CON_.epub");
        assert_eq!(suggested_epub_filename("LPT9.notes"), "LPT9_.notes.epub");
    }

    #[test]
    fn editor_search_uses_visible_xhtml_text_and_unsaved_chapters() {
        let chapters = vec![EditorChapter {
            title: "起步".to_string(),
            href: "Text/start.xhtml".to_string(),
            spine_index: Some(0),
            html: r#"<html><head><title>HEAD_ONLY_TOKEN</title>
                <style>.hidden{content:'错误命中'}</style></head>
                <body><h1>全<strong>文</strong>检索 Rust &amp; EPUB</h1>
                <p>Rust 的 EPUB 是尚未保存的关键词。</p></body></html>"#
                .to_string(),
        }];

        let hits = search_editor_chapters(&chapters, "尚未保存");
        assert_eq!(1, hits.len());
        assert_eq!(0, hits[0].chapter_index);
        assert!(hits[0].snippet.contains("尚未保存"));
        assert!(search_editor_chapters(&chapters, "Rust & EPUB").len() == 1);
        assert_eq!(1, search_editor_chapters(&chapters, "Rust EPUB").len());
        assert_eq!(1, search_editor_chapters(&chapters, "全文检索").len());
        assert!(search_editor_chapters(&chapters, "错误命中").is_empty());
        assert!(search_editor_chapters(&chapters, "HEAD_ONLY_TOKEN").is_empty());
    }

    #[test]
    fn parses_editor_mode_revision_and_real_chapter_path() {
        let mut valid_uris = vec![
            "epubeditor://shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=7",
        ];
        #[cfg(target_os = "windows")]
        valid_uris.push(
            "http://epubeditor.shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=7",
        );
        for uri in valid_uris {
            let uri = uri.parse().expect("valid editor URI");
            assert_eq!(
                editor_document_request(&uri),
                Some(EditorDocumentRequest {
                    kind: EditorDocumentKind::TrustedShell,
                    session_id: "session-a".to_string(),
                    chapter_id: "unit-a".to_string(),
                    revision: 7,
                    href: "EPUB/Text/ch.xhtml".to_string(),
                })
            );
        }

        let preview =
            "epubeditor://content/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=8"
                .parse()
                .expect("valid preview URI");
        assert_eq!(
            editor_document_request(&preview).unwrap().kind,
            EditorDocumentKind::Preview
        );
        let source =
            "epubeditor://content/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=8"
                .parse()
                .expect("valid rich source URI");
        assert_eq!(
            editor_document_request(&source).unwrap().kind,
            EditorDocumentKind::RichSource
        );
        let forged = "http://epubeditor.evil/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid forged URI");
        assert!(editor_document_request(&forged).is_none());
        let forged_https = "https://epubeditor.shell/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8"
            .parse()
            .expect("valid forged HTTPS URI");
        assert!(editor_document_request(&forged_https).is_none());
        let legacy =
            "epubeditor://book/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8"
                .parse()
                .expect("valid legacy-origin URI");
        assert!(editor_document_request(&legacy).is_none());
        let duplicate = "epubeditor://shell/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&chapter_id=unit-b&rev=8"
            .parse()
            .expect("valid duplicate-query URI");
        assert!(editor_document_request(&duplicate).is_none());
        for mismatched_role in [
            "epubeditor://shell/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=8",
            "epubeditor://shell/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=8",
            "epubeditor://content/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=8",
        ] {
            assert!(
                editor_document_request(&mismatched_role.parse().unwrap()).is_none(),
                "{mismatched_role}"
            );
        }
    }

    #[test]
    fn builds_platform_editor_navigation_url() {
        let url = editor_navigation_url(
            EditorTab::RichText,
            "session-a",
            "unit-a",
            9,
            "EPUB/Text/ch.xhtml",
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            url,
            "http://epubeditor.shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=9"
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            url,
            "epubeditor://shell/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=9"
        );

        let preview = editor_navigation_url(
            EditorTab::Preview,
            "session-a",
            "unit-a",
            10,
            "EPUB/Text/ch.xhtml",
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            preview,
            "http://epubeditor.content/EPUB/Text/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=10"
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            preview,
            "epubeditor://content/EPUB/Text/ch.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=10"
        );
        assert!(is_editor_navigation_url(&url));
        assert!(is_editor_navigation_url(&preview));
        assert!(!is_editor_navigation_url(
            "epubeditor://content/EPUB/Text/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=10"
        ));
        assert!(!is_editor_navigation_url(
            "epubeditor://book/EPUB/Text/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=10"
        ));
    }

    #[test]
    fn body_snapshot_preserves_the_xhtml_document_shell() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>T</title><!-- <body>fake</body> --><script>keep()</script></head><xhtml:body class="book"><p>old</p></xhtml:body></html>"#;
        let replacement = r#"<body xmlns="http://www.w3.org/1999/xhtml" class="book"><p><strong>new</strong></p></body>"#;
        let updated = replace_body_element(original, replacement).expect("body is replaceable");
        assert!(updated.starts_with("<?xml version="));
        assert!(updated.contains("<!DOCTYPE html>"));
        assert!(updated.contains("<head><title>T</title>"));
        assert!(updated.contains("<script>keep()</script></head>"));
        assert!(updated.contains("<!-- <body>fake</body> -->"));
        assert!(updated.contains("<strong>new</strong>"));
        assert!(!updated.contains("<p>old</p>"));
    }

    #[test]
    fn rich_text_snapshot_does_not_promote_head_title_into_body() {
        let snapshot = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>HEAD_ONLY_TOKEN</title></head>
  <body xmlns="http://www.w3.org/1999/xhtml">
    <h1>第一章</h1><p>BODY_ONLY_TOKEN</p>
  </body>
</html>"#;

        let parsed = parse_rich_text_snapshot(snapshot, "unit-1").expect("valid snapshot");
        let text = parsed.document.plain_text();
        assert!(!text.contains("HEAD_ONLY_TOKEN"));
        assert!(text.contains("第一章"));
        assert!(text.contains("BODY_ONLY_TOKEN"));
        assert!(!parsed.canonical_source.contains("HEAD_ONLY_TOKEN"));
    }

    #[test]
    fn xml_scanner_recognizes_self_closing_body_elements() {
        for body in [
            "<body/>",
            "<body />",
            "<xhtml:body/>",
            "<xhtml:body data-note=\"a > b\"\n\t />",
        ] {
            assert_eq!(
                xml_body_element_range(body),
                Some((0, body.len())),
                "{body}"
            );
        }
    }

    #[test]
    fn body_snapshot_replaces_a_self_closing_prefixed_body() {
        let original = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head/><xhtml:body class="empty"
 /></html>"#;
        let replacement = r#"<body xmlns="http://www.w3.org/1999/xhtml"><p>new</p></body>"#;

        assert_eq!(
            replace_body_element(original, replacement),
            Some(r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head/><body xmlns="http://www.w3.org/1999/xhtml"><p>new</p></body></html>"#.to_string())
        );
    }

    #[test]
    fn body_snapshot_accepts_a_self_closing_replacement() {
        let original = r#"<html><head/><body><p>old</p></body></html>"#;
        let replacement = "<xhtml:body class=\"empty\"\n />";

        assert_eq!(
            replace_body_element(original, replacement),
            Some(
                r#"<html><head/><xhtml:body class="empty"
 /></html>"#
                    .to_string()
            )
        );
    }

    #[test]
    fn ipc_snapshot_is_keyed_to_its_page() {
        let state = EditorWebState::new(
            "book-a".to_string(),
            "/EPUB/ch.xhtml".to_string(),
            "<html><head></head><body><p>old</p></body></html>".to_string(),
        );
        let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
        let update = state
            .apply_message(EditorIpcMessage {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: "EPUB/ch.xhtml".to_string(),
                revision: 0,
                request_id: Some(11),
                body: Some("<body><p>new</p></body>".to_string()),
                selected_text: "  exact\n selection  ".to_string(),
                too_large: false,
                ready: false,
            })
            .expect("matching snapshot");
        assert!(update.html.contains("<p>new</p>"));
        assert_eq!(update.href, "/EPUB/ch.xhtml");
        assert_eq!(update.request_id, Some(11));
        assert_eq!(update.selected_text.as_deref(), Some("exact selection"));
        assert!(
            state
                .apply_message(EditorIpcMessage {
                    session_id: session_id.clone(),
                    chapter_id: chapter_id.clone(),
                    href: "EPUB/other.xhtml".to_string(),
                    revision: 0,
                    request_id: None,
                    body: Some("<body><p>wrong chapter</p></body>".to_string()),
                    selected_text: String::new(),
                    too_large: false,
                    ready: false,
                })
                .is_none()
        );
        let unchanged = state
            .apply_message(EditorIpcMessage {
                session_id,
                chapter_id,
                href: "EPUB/ch.xhtml".to_string(),
                revision: 0,
                request_id: Some(12),
                body: None,
                selected_text: String::new(),
                too_large: false,
                ready: false,
            })
            .expect("unchanged snapshot acknowledgement");
        assert_eq!(unchanged.request_id, Some(12));
        assert!(unchanged.html.contains("<p>new</p>"));
    }

    #[test]
    fn ipc_parser_requires_the_complete_snapshot_identity() {
        let complete = serde_json::json!({
            "session_id": "session-a",
            "chapter_id": "unit-a",
            "href": "EPUB/ch.xhtml",
            "revision": 3,
            "request_id": 11,
            "body": null,
            "selected_text": "",
            "too_large": false,
            "ready": false
        });
        let parsed = parse_editor_ipc_message(&complete.to_string()).expect("complete identity");
        assert_eq!(parsed.session_id, "session-a");
        assert_eq!(parsed.chapter_id, "unit-a");
        assert_eq!(parsed.request_id, Some(11));

        for missing in ["session_id", "chapter_id", "revision", "request_id"] {
            let mut legacy = complete.clone();
            legacy.as_object_mut().unwrap().remove(missing);
            assert!(
                parse_editor_ipc_message(&legacy.to_string()).is_none(),
                "legacy message without {missing} must be rejected"
            );
        }
        let mut unknown = complete;
        unknown["legacy_href"] = serde_json::json!("EPUB/ch.xhtml");
        assert!(parse_editor_ipc_message(&unknown.to_string()).is_none());
        let mut zero_request = unknown;
        zero_request.as_object_mut().unwrap().remove("legacy_href");
        zero_request["request_id"] = serde_json::json!(0);
        assert!(parse_editor_ipc_message(&zero_request.to_string()).is_none());
    }

    #[test]
    fn ipc_is_accepted_only_from_the_trusted_shell_origin() {
        let message = EditorIpcMessage {
            session_id: "session-a".to_string(),
            chapter_id: "unit-a".to_string(),
            href: "EPUB/ch.xhtml".to_string(),
            revision: 3,
            request_id: Some(11),
            body: None,
            selected_text: String::new(),
            too_large: false,
            ready: false,
        };
        let shell = editor_document_request(
            &"epubeditor://shell/EPUB/ch.xhtml?mode=edit&session_id=session-a&chapter_id=unit-a&rev=3"
                .parse()
                .unwrap(),
        )
        .unwrap();
        let content = editor_document_request(
            &"epubeditor://content/EPUB/ch.xhtml?mode=source&session_id=session-a&chapter_id=unit-a&rev=3"
                .parse()
                .unwrap(),
        )
        .unwrap();

        assert!(editor_ipc_matches_document_request(&shell, &message));
        assert!(!editor_ipc_matches_document_request(&content, &message));
        let mut stale = message;
        stale.revision += 1;
        assert!(!editor_ipc_matches_document_request(&shell, &stale));
    }

    #[test]
    fn citation_result_requires_the_exact_preview_page_identity() {
        let result = serde_json::json!({
            "type": "citation_navigation_result",
            "session_id": "session-a",
            "chapter_id": "unit-a",
            "href": "chapter-1.xhtml",
            "revision": 4,
            "request_id": 4,
            "found": false,
            "reason": "ambiguous"
        });
        let parsed = parse_editor_citation_navigation_result(&result.to_string()).unwrap();
        assert_eq!(parsed.chapter_id, "unit-a");
        assert_eq!(parsed.reason, "ambiguous");

        let preview = editor_document_request(
            &"epubeditor://content/chapter-1.xhtml?mode=preview&session_id=session-a&chapter_id=unit-a&rev=4"
                .parse()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(preview.kind, EditorDocumentKind::Preview);
        assert_eq!(parsed.session_id, preview.session_id);
        assert_eq!(parsed.chapter_id, preview.chapter_id);
        assert_eq!(parsed.revision, preview.revision);
        assert_eq!(normalized_editor_path(&parsed.href).unwrap(), preview.href);

        let mut stale = result;
        stale["revision"] = serde_json::json!(3);
        let stale = parse_editor_citation_navigation_result(&stale.to_string()).unwrap();
        assert_ne!(stale.revision, preview.revision);
    }

    #[test]
    fn ipc_state_rejects_another_session_or_stable_chapter() {
        let state = EditorWebState::new(
            "book-a".to_string(),
            "EPUB/ch.xhtml".to_string(),
            "<html><body><p>old</p></body></html>".to_string(),
        );
        let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
        let message = |session_id: String, chapter_id: String| EditorIpcMessage {
            session_id,
            chapter_id,
            href: "EPUB/ch.xhtml".to_string(),
            revision: 0,
            request_id: Some(1),
            body: None,
            selected_text: String::new(),
            too_large: false,
            ready: false,
        };
        assert!(
            state
                .apply_message(message("another-session".to_string(), chapter_id.clone()))
                .is_none()
        );
        assert!(
            state
                .apply_message(message(session_id, "another-unit".to_string()))
                .is_none()
        );
    }

    #[test]
    fn ipc_ready_handshake_is_keyed_to_its_page() {
        let state = EditorWebState::new(
            "book-a".to_string(),
            "/EPUB/ch.xhtml".to_string(),
            "<html><body><p>old</p></body></html>".to_string(),
        );
        let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
        let ready = state
            .apply_message(EditorIpcMessage {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: "EPUB/ch.xhtml".to_string(),
                revision: 0,
                request_id: None,
                body: None,
                selected_text: String::new(),
                too_large: false,
                ready: true,
            })
            .expect("matching ready message");
        assert!(ready.ready);
        assert_eq!(ready.href, "/EPUB/ch.xhtml");
        assert!(
            state
                .apply_message(EditorIpcMessage {
                    session_id,
                    chapter_id,
                    href: "EPUB/other.xhtml".to_string(),
                    revision: 0,
                    request_id: None,
                    body: None,
                    selected_text: String::new(),
                    too_large: false,
                    ready: true,
                })
                .is_none()
        );
    }

    #[test]
    fn ipc_rejects_selection_text_over_the_host_limit() {
        let state = EditorWebState::new(
            "book-a".to_string(),
            "/EPUB/ch.xhtml".to_string(),
            "<html><body><p>old</p></body></html>".to_string(),
        );
        let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
        assert!(
            state
                .apply_message(EditorIpcMessage {
                    session_id,
                    chapter_id,
                    href: "EPUB/ch.xhtml".to_string(),
                    revision: 0,
                    request_id: Some(9),
                    body: None,
                    selected_text: "x".repeat(MAX_EDITOR_SELECTION_BYTES + 1),
                    too_large: false,
                    ready: false,
                })
                .is_none()
        );
    }

    #[test]
    fn pending_editor_actions_are_latest_wins_but_close_is_sticky() {
        let mut export_path = None;
        assert_eq!(
            merge_pending_editor_action(
                PendingEditorAction::SwitchTab(EditorTab::Preview),
                PendingEditorAction::SwitchTab(EditorTab::Source),
                &mut export_path,
            ),
            PendingEditorAction::SwitchTab(EditorTab::Source)
        );
        let mut export_path = None;
        assert_eq!(
            merge_pending_editor_action(
                PendingEditorAction::Save,
                PendingEditorAction::Close,
                &mut export_path,
            ),
            PendingEditorAction::Close
        );
        let mut export_path = None;
        assert_eq!(
            merge_pending_editor_action(
                PendingEditorAction::Close,
                PendingEditorAction::SwitchTab(EditorTab::Source),
                &mut export_path,
            ),
            PendingEditorAction::Close
        );
    }

    #[test]
    fn editor_write_completion_never_closes_after_failure_or_export_error() {
        let operation = ActiveEditorWrite {
            id: 1,
            intent: EditorWriteIntent::Close,
            draft_generation: 7,
            title: "测试".to_string(),
            target_display: None,
        };
        assert_eq!(
            editor_write_follow_up(&operation, true, 7, false, false),
            EditorWriteFollowUp::StayOpen
        );

        let export = ActiveEditorWrite {
            intent: EditorWriteIntent::Export,
            ..operation.clone()
        };
        assert_eq!(
            editor_write_follow_up(&export, true, 7, true, false),
            EditorWriteFollowUp::StayOpen
        );
        assert_eq!(
            editor_write_follow_up(&operation, true, 8, true, true),
            EditorWriteFollowUp::SaveThenClose
        );
        assert_eq!(
            editor_write_follow_up(&operation, true, 7, true, true),
            EditorWriteFollowUp::Close
        );
    }

    #[test]
    fn editor_write_uses_background_runtime_and_stale_failure_does_not_overwrite() {
        let data_dir = tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let book_id = runtime.block_on(async {
            services
                .spawn_library(|library| library.create_book("初始书名", "作者"))
                .await
                .unwrap()
                .unwrap()
                .id
        });
        let stale = runtime.block_on(async {
            let lookup = book_id.clone();
            services
                .spawn_library_read(move |library| library.document(&lookup))
                .await
                .unwrap()
                .unwrap()
        });

        let mut winner = stale.clone();
        winner.title = "后台保存成功".to_string();
        let caller_thread = std::thread::current().id();
        let saved = runtime.block_on(async {
            spawn_editor_write(
                &services,
                EditorWriteJob {
                    document: winner,
                    new_asset_bytes: HashMap::new(),
                    export_target: None,
                },
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert_ne!(caller_thread, saved.value.worker_thread_id);
        assert_eq!(saved.value.document.title, "后台保存成功");
        assert_eq!(
            saved
                .snapshot
                .books()
                .iter()
                .find(|book| book.id == book_id)
                .unwrap()
                .title,
            "后台保存成功"
        );

        let export_target = data_dir.path().join("background-export.epub");
        let mut export_document = saved.value.document.clone();
        export_document.title = "后台保存并导出".to_string();
        let exported = runtime.block_on(async {
            spawn_editor_write(
                &services,
                EditorWriteJob {
                    document: export_document,
                    new_asset_bytes: HashMap::new(),
                    export_target: Some(export_target.clone()),
                },
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert_ne!(caller_thread, exported.value.worker_thread_id);
        assert!(exported.generation > saved.generation);
        assert!(matches!(
            exported.value.export,
            EditorExportOutcome::Exported(_)
        ));
        assert!(std::fs::metadata(&export_target).unwrap().len() > 0);

        let mut loser = stale;
        loser.title = "过期窗口不应覆盖".to_string();
        let stale_result = runtime.block_on(async {
            spawn_editor_write(
                &services,
                EditorWriteJob {
                    document: loser,
                    new_asset_bytes: HashMap::new(),
                    export_target: None,
                },
            )
            .await
            .unwrap()
        });
        assert!(stale_result.is_err());

        let persisted = runtime.block_on(async {
            let lookup = book_id.clone();
            services
                .spawn_library_read(move |library| library.document(&lookup))
                .await
                .unwrap()
                .unwrap()
        });
        assert_eq!(persisted.title, "后台保存并导出");
        assert_eq!(Arc::strong_count(&services), 1);
        drop(services);
        drop(runtime);
        close_test_data_dir(data_dir);
    }

    #[test]
    fn editor_projection_guard_preserves_a_newer_group_assignment() {
        let data_dir = tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let created = runtime.block_on(async {
            services
                .spawn_library_projected(|library| library.create_book("初始书名", "作者"))
                .await
                .unwrap()
                .unwrap()
        });
        let book_id = created.value.id;
        let group = runtime.block_on(async {
            services
                .spawn_library_projected(|library| library.create_group("稍后分组", None))
                .await
                .unwrap()
                .unwrap()
        });
        let group_id = group.value.id;
        let mut editor_document = runtime.block_on(async {
            let lookup = book_id.clone();
            services
                .spawn_library_read(move |library| library.document(&lookup))
                .await
                .unwrap()
                .unwrap()
        });
        editor_document.title = "编辑器保存".to_string();
        let editor = runtime.block_on(async {
            spawn_editor_write(
                &services,
                EditorWriteJob {
                    document: editor_document,
                    new_asset_bytes: HashMap::new(),
                    export_target: None,
                },
            )
            .await
            .unwrap()
            .unwrap()
        });
        let moved = runtime.block_on(async {
            let moved_book_id = book_id.clone();
            let moved_group_id = group_id.clone();
            services
                .spawn_library_projected(move |library| {
                    library.set_book_group(&moved_book_id, Some(&moved_group_id))
                })
                .await
                .unwrap()
                .unwrap()
        });
        assert!(moved.generation > editor.generation);

        let mut local = group.snapshot;
        let mut applied_generation = group.generation;
        assert!(apply_editor_library_projection(
            &mut local,
            &mut applied_generation,
            moved.generation,
            moved.snapshot,
        ));
        assert_eq!(
            local
                .books()
                .iter()
                .find(|book| book.id == book_id)
                .unwrap()
                .group_id
                .as_deref(),
            Some(group_id.as_str())
        );
        assert!(!apply_editor_library_projection(
            &mut local,
            &mut applied_generation,
            editor.generation,
            editor.snapshot,
        ));
        assert_eq!(
            local
                .books()
                .iter()
                .find(|book| book.id == book_id)
                .unwrap()
                .group_id
                .as_deref(),
            Some(group_id.as_str())
        );

        drop(services);
        drop(runtime);
        close_test_data_dir(data_dir);
    }

    #[test]
    fn replacing_a_pending_export_discards_its_target_path() {
        let mut export_path = Some(PathBuf::from("first.epub"));
        assert_eq!(
            merge_pending_editor_action(
                PendingEditorAction::Save,
                PendingEditorAction::Export,
                &mut export_path,
            ),
            PendingEditorAction::Export
        );
        assert_eq!(
            export_path.as_deref(),
            Some(std::path::Path::new("first.epub"))
        );

        assert_eq!(
            merge_pending_editor_action(
                PendingEditorAction::Export,
                PendingEditorAction::SwitchTab(EditorTab::Preview),
                &mut export_path,
            ),
            PendingEditorAction::SwitchTab(EditorTab::Preview)
        );
        assert!(export_path.is_none());
    }

    #[test]
    fn webview_build_gate_defers_close_until_the_builder_settles() {
        let mut gate = EditorWebViewBuildGate::new(true);
        assert!(gate.request_close());
        assert!(gate.request_close());
        assert!(gate.finish());
        assert!(!gate.finish());
        assert!(!gate.request_close());
    }

    #[test]
    fn ready_timeout_only_matches_the_current_unready_page() {
        let mut page = ActiveEditorPage {
            session_id: "session-a".to_string(),
            chapter_id: "unit-a".to_string(),
            href: "/EPUB/ch.xhtml".to_string(),
            revision: 7,
            ready: false,
        };
        assert!(page.is_unready_match("session-a", "unit-a", 7, "EPUB/ch.xhtml"));
        assert!(!page.is_unready_match("session-b", "unit-a", 7, "EPUB/ch.xhtml"));
        assert!(!page.is_unready_match("session-a", "unit-b", 7, "EPUB/ch.xhtml"));
        assert!(!page.is_unready_match("session-a", "unit-a", 8, "EPUB/ch.xhtml"));
        assert!(!page.is_unready_match("session-a", "unit-a", 7, "EPUB/other.xhtml"));
        page.ready = true;
        assert!(!page.is_unready_match("session-a", "unit-a", 7, "EPUB/ch.xhtml"));
    }

    #[test]
    fn trusted_bridge_supplies_prosemirror_editor_contract() {
        assert!(
            EDITOR_INITIALIZATION_SCRIPT.starts_with("(()=>{const __moyeEditorTrustedShell=()=>{"),
            "the complete ProseMirror bundle must wait for an XHTML document element"
        );
        assert!(
            EDITOR_INITIALIZATION_SCRIPT
                .contains("DOMContentLoaded\",__moyeEditorStart,{once:true}")
        );
        for marker in [
            "contenteditable",
            "ProseMirror",
            "__moyeProseMirror",
            "bullet_list",
            "ordered_list",
            "table_cell",
            "restricted_html",
            "insertResource",
            "replaceSelectedResource",
            "XMLSerializer",
            "createElementNS",
            "session_id",
            "chapter_id",
            "request_id",
            "selected_text",
            "too_large",
            "epubeditor.shell",
            "epubeditor.content",
            "source",
            "credentials",
            "omit",
            "redirect",
            "error",
        ] {
            assert!(
                EDITOR_INITIALIZATION_SCRIPT.contains(marker),
                "missing {marker}"
            );
        }
        let send_installed = EDITOR_INITIALIZATION_SCRIPT
            .find("window.__moyeEditorSend=")
            .expect("snapshot bridge installation");
        let ready_sent = EDITOR_INITIALIZATION_SCRIPT
            .find("ready:!0")
            .expect("ready handshake");
        assert!(send_installed < ready_sent);
        assert!(!EDITOR_SHELL_CSP.contains("script-src 'unsafe-inline'"));
        assert!(EDITOR_SHELL_CSP.contains("connect-src http://epubeditor.content"));
        assert!(EDITOR_CONTENT_CSP.contains("connect-src 'none'"));
        assert!(EDITOR_CONTENT_CSP.contains("script-src 'none'"));
    }

    #[test]
    fn editor_protocol_separates_trusted_shell_from_canonical_content() {
        let href = "chapter-1.xhtml".to_string();
        let html = editor_document_shell("规范化章节", "<h1>规范化正文</h1>");
        let state = EditorWebState::new("book-a".to_string(), href.clone(), html);
        let (session_id, chapter_id) = editor_ipc_identity(&state, 0);

        let document_request = gpui_component::wry::http::Request::builder()
            .uri(editor_custom_url(
                EditorTab::RichText,
                &session_id,
                &chapter_id,
                0,
                &href,
            ))
            .body(())
            .unwrap();
        let response = editor_protocol_response(&state, &document_request);
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "application/xhtml+xml; charset=utf-8"
        );
        let shell = String::from_utf8_lossy(response.body());
        assert_eq!(shell, EDITOR_TRUSTED_SHELL);
        assert!(!shell.contains("规范化正文"));
        assert_eq!(
            response.headers()["content-security-policy"],
            EDITOR_SHELL_CSP
        );
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );

        let source_url = format!(
            "epubeditor://content/{href}?mode=source&session_id={}&chapter_id={chapter_id}&rev=0",
            urlencoding::encode(&session_id)
        );
        let source_request = gpui_component::wry::http::Request::builder()
            .uri(&source_url)
            .header("Origin", EDITOR_SHELL_ORIGIN)
            .body(())
            .unwrap();
        let source_response = editor_protocol_response(&state, &source_request);
        assert_eq!(source_response.status(), 200);
        assert!(String::from_utf8_lossy(source_response.body()).contains("规范化正文"));
        assert_eq!(
            source_response.headers()["content-security-policy"],
            EDITOR_CONTENT_CSP
        );
        assert_eq!(
            source_response.headers()["access-control-allow-origin"],
            EDITOR_SHELL_ORIGIN
        );
        assert_eq!(
            source_response.headers()["cross-origin-resource-policy"],
            "cross-origin"
        );
        let source_without_origin =
            editor_protocol_response(&state, &protocol_request(&source_url, None));
        assert_eq!(source_without_origin.status(), 403);
        let forged_source = gpui_component::wry::http::Request::builder()
            .uri(&source_url)
            .header("Origin", "http://epubeditor.evil")
            .body(())
            .unwrap();
        assert_eq!(
            editor_protocol_response(&state, &forged_source).status(),
            403
        );

        let preview_request = protocol_request(
            editor_custom_url(EditorTab::Preview, &session_id, &chapter_id, 0, &href),
            None,
        );
        let preview_response = editor_protocol_response(&state, &preview_request);
        assert_eq!(preview_response.status(), 200);
        assert!(String::from_utf8_lossy(preview_response.body()).contains("规范化正文"));
        assert!(
            preview_response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );

        let stale_session = editor_protocol_response(
            &state,
            &protocol_request(
                editor_custom_url(
                    EditorTab::RichText,
                    "another-session",
                    &chapter_id,
                    0,
                    &href,
                ),
                None,
            ),
        );
        assert_eq!(stale_session.status(), 404);
        let stale_chapter = editor_protocol_response(
            &state,
            &protocol_request(
                editor_custom_url(EditorTab::RichText, &session_id, "another-unit", 0, &href),
                None,
            ),
        );
        assert_eq!(stale_chapter.status(), 404);

        let legacy_origin = editor_protocol_response(
            &state,
            &protocol_request(
                format!(
                    "epubeditor://book/{href}?mode=edit&session_id={session_id}&chapter_id={chapter_id}&rev=0"
                ),
                None,
            ),
        );
        assert_eq!(legacy_origin.status(), 403);

        for uri in [
            "epubeditor://content/EPUB/Styles/book.css",
            "epubeditor://content/EPUB/Images/pixel.png",
            "epubeditor://content/EPUB/Fonts/book.woff2",
        ] {
            let request = gpui_component::wry::http::Request::builder()
                .uri(uri)
                .body(())
                .unwrap();
            let response = editor_protocol_response(&state, &request);
            assert_eq!(response.status(), 404, "{uri}");
        }

        let missing_request = gpui_component::wry::http::Request::builder()
            .uri("epubeditor://content/EPUB/Images/missing.png")
            .body(())
            .unwrap();
        let missing = editor_protocol_response(&state, &missing_request);
        assert_eq!(missing.status(), 404);
    }

    #[test]
    fn editor_protocol_rewrites_and_serves_new_media_from_the_private_origin() {
        let bytes = Arc::new(b"new-audio-bytes".to_vec());
        let asset = AssetRef::from_bytes(
            AssetRole::Audio,
            "audio/mpeg",
            Some("sample.mp3".to_string()),
            bytes.as_slice(),
        );
        let document = document_with_audio("book-a", asset.clone());
        let href = "EPUB/Text/ch.xhtml".to_string();
        let html = format!(
            "<html><body><audio SRC = 'moye-asset:{}'></audio><p>src=\"moye-asset:{}\"</p><audio src=\"moye-asset:unknown\"></audio></body></html>",
            asset.id, asset.id
        );
        let state = EditorWebState::new("book-a".to_string(), href.clone(), html);
        let (session_id, chapter_id) = editor_ipc_identity(&state, 0);
        state
            .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
            .unwrap();

        let page = editor_protocol_response(
            &state,
            &protocol_request(
                editor_custom_url(EditorTab::Preview, &session_id, &chapter_id, 0, &href),
                None,
            ),
        );
        let page = String::from_utf8(page.body().to_vec()).unwrap();
        let private_path = editor_asset_path(&asset.id);
        assert!(page.contains(&format!("SRC = '{private_path}'")));
        assert!(page.contains(&format!("<p>src=\"moye-asset:{}\"</p>", asset.id)));
        assert!(page.contains("src=\"moye-asset:unknown\""));

        let media = editor_protocol_response(
            &state,
            &protocol_request(
                format!("epubeditor://content{}", editor_asset_path(&asset.id)),
                None,
            ),
        );
        assert_eq!(media.status(), 200);
        assert_eq!(media.headers()["content-type"], "audio/mpeg");
        assert_eq!(media.headers()["accept-ranges"], "bytes");
        assert_eq!(media.body().as_ref(), bytes.as_slice());
        let shell_media = editor_protocol_response(
            &state,
            &protocol_request(
                format!("epubeditor://shell{}", editor_asset_path(&asset.id)),
                None,
            ),
        );
        assert_eq!(shell_media.status(), 404);

        let canonical = state
            .apply_message(EditorIpcMessage {
                session_id,
                chapter_id,
                href,
                revision: 0,
                request_id: Some(7),
                body: Some(format!(
                    "<body><audio src=\"{}\"></audio></body>",
                    editor_asset_path(&asset.id)
                )),
                selected_text: String::new(),
                too_large: false,
                ready: false,
            })
            .unwrap();
        assert!(canonical.html.contains(&format!("moye-asset:{}", asset.id)));
        assert!(!canonical.html.contains(EDITOR_ASSET_PATH_PREFIX));

        let canonical_absolute = state
            .apply_message(EditorIpcMessage {
                session_id: canonical.session_id,
                chapter_id: canonical.chapter_id,
                href: canonical.href,
                revision: canonical.revision,
                request_id: Some(8),
                body: Some(format!(
                    "<body><audio src=\"epubeditor://content{}\"></audio></body>",
                    editor_asset_path(&asset.id)
                )),
                selected_text: String::new(),
                too_large: false,
                ready: false,
            })
            .unwrap();
        assert!(
            canonical_absolute
                .html
                .contains(&format!("moye-asset:{}", asset.id))
        );
        #[cfg(target_os = "windows")]
        {
            let canonical_windows = state
                .apply_message(EditorIpcMessage {
                    session_id: canonical_absolute.session_id,
                    chapter_id: canonical_absolute.chapter_id,
                    href: canonical_absolute.href,
                    revision: canonical_absolute.revision,
                    request_id: Some(9),
                    body: Some(format!(
                        "<body><audio src=\"http://epubeditor.content{}\"></audio></body>",
                        editor_asset_path(&asset.id)
                    )),
                    selected_text: String::new(),
                    too_large: false,
                    ready: false,
                })
                .unwrap();
            assert!(
                canonical_windows
                    .html
                    .contains(&format!("moye-asset:{}", asset.id))
            );
        }
    }

    #[test]
    fn editor_media_protocol_supports_seek_ranges_and_unsatisfiable_ranges() {
        let bytes = Arc::new(b"0123456789".to_vec());
        let asset = AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, bytes.as_slice());
        let document = document_with_audio("book-a", asset.clone());
        let state = EditorWebState::new(
            "book-a".to_string(),
            "EPUB/Text/ch.xhtml".to_string(),
            "<html><body></body></html>".to_string(),
        );
        state
            .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
            .unwrap();
        let uri = format!("epubeditor://content{}", editor_asset_path(&asset.id));

        let partial = editor_protocol_response(&state, &protocol_request(&uri, Some("bytes=2-5")));
        assert_eq!(partial.status(), 206);
        assert_eq!(partial.headers()["content-range"], "bytes 2-5/10");
        assert_eq!(partial.headers()["content-length"], "4");
        assert_eq!(partial.body().as_ref(), b"2345");

        let invalid =
            editor_protocol_response(&state, &protocol_request(&uri, Some("bytes=20-30")));
        assert_eq!(invalid.status(), 416);
        assert_eq!(invalid.headers()["content-range"], "bytes */10");
        assert_eq!(invalid.headers()["content-length"], "0");
        assert!(invalid.body().is_empty());
    }

    #[test]
    fn editor_media_files_are_size_bounded_before_full_allocation() {
        let data_dir = tempdir().unwrap();
        let small = data_dir.path().join("small.bin");
        std::fs::write(&small, b"small-media").unwrap();
        assert_eq!(
            read_editor_media_file(&small, MediaKind::Image).unwrap(),
            b"small-media"
        );

        let oversized = data_dir.path().join("oversized.bin");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MediaKind::Image.max_bytes() + 1).unwrap();
        drop(file);
        let error = read_editor_media_file(&oversized, MediaKind::Image).unwrap_err();
        assert!(error.to_string().contains("安全上限"));
        close_test_data_dir(data_dir);
    }

    #[test]
    fn pending_media_range_is_served_on_the_background_runtime() {
        let data_dir = tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let bytes = Arc::new(b"0123456789".to_vec());
        let asset = AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, bytes.as_slice());
        let document = document_with_audio("book-a", asset.clone());
        let state = EditorWebState::new(
            "book-a".to_string(),
            "chapter-1.xhtml".to_string(),
            editor_document_shell("第一章", ""),
        );
        state
            .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
            .unwrap();
        let snapshot = state.media_snapshot();
        let expected = snapshot.assets.get(&asset.id).unwrap().clone();

        let response = runtime.block_on(async {
            spawn_editor_media(
                &services,
                snapshot,
                "book-a".to_string(),
                asset.id.clone(),
                expected,
                Some("bytes=3-6".to_string()),
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert_eq!(response.status, 206);
        assert_eq!(response.body, b"3456");
        state.close_protocol();
        assert!(!state.protocol_is_open());
        drop(services);
        drop(runtime);
        close_test_data_dir(data_dir);
    }

    #[test]
    fn pending_asset_bytes_drop_unreferenced_replacements() {
        let keep_bytes = Arc::new(b"keep".to_vec());
        let keep = AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, keep_bytes.as_slice());
        let orphan_bytes = Arc::new(b"orphan".to_vec());
        let orphan =
            AssetRef::from_bytes(AssetRole::Audio, "audio/ogg", None, orphan_bytes.as_slice());
        let document = document_with_audio("book-a", keep.clone());
        let mut pending = HashMap::from([
            (keep.id.clone(), keep_bytes),
            (orphan.id.clone(), orphan_bytes),
        ]);

        retain_referenced_pending_assets(&mut pending, &document);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&keep.id));
        assert!(!pending.contains_key(&orphan.id));
    }

    #[test]
    fn persisted_editor_media_is_authorized_without_hydration_and_read_by_range() {
        let data_dir = tempdir().unwrap();
        let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
        let runtime = services.runtime();
        let bytes = Arc::new(b"0123456789-persisted-audio".to_vec());
        let asset = AssetRef::from_bytes(
            AssetRole::Audio,
            "audio/ogg",
            Some("persisted.ogg".to_string()),
            bytes.as_slice(),
        );
        let stored_document = runtime.block_on(async {
            let bytes = Arc::clone(&bytes);
            let asset = asset.clone();
            services
                .spawn_library(move |library| {
                    let record = library.create_book("按需媒体", "测试")?;
                    let mut document = library.document(&record.id)?;
                    let unit = document.units.first_mut().context("default unit")?;
                    unit.document.blocks.push(Block::Audio {
                        id: "persisted-audio".to_string(),
                        asset_id: asset.id.clone(),
                        title: None,
                        caption: Vec::new(),
                    });
                    let metadata = EditorMediaMetadataDraft::from_inputs("音频", "按需读取")?;
                    anyhow::ensure!(
                        update_media_metadata(
                            &mut unit.document.blocks,
                            "persisted-audio",
                            &metadata,
                        ),
                        "audio block must accept metadata"
                    );
                    unit.source = serialize_source(&unit.document, unit.source_kind)?;
                    document.assets.push(asset.clone());
                    library.apply_document_with_assets(
                        document,
                        HashMap::from([(asset.id.clone(), bytes)]),
                    )?;
                    library.document(&record.id)
                })
                .await
                .unwrap()
                .unwrap()
        });

        let state = EditorWebState::new(
            stored_document.id.clone(),
            "chapter.xhtml".to_string(),
            "<html><body></body></html>".to_string(),
        );
        let search_hits = runtime.block_on(async {
            let book_id = stored_document.id.clone();
            services
                .spawn_library_read(move |library| library.search_book(&book_id, "按需读取", 10))
                .await
                .unwrap()
                .unwrap()
        });
        assert!(!search_hits.is_empty(), "media captions must enter FTS");
        state.authorize_media(&stored_document).unwrap();
        let snapshot = state.media_snapshot();
        let authorized = snapshot.assets.get(&asset.id).unwrap().clone();
        assert!(authorized.bytes.is_none());
        assert!(
            MediaService::new(snapshot)
                .serve(&stored_document.id, &asset.id, Some("bytes=2-5"))
                .is_err(),
            "persistent bytes must not be sliced from an in-memory Vec"
        );

        let response = runtime.block_on(async {
            spawn_persisted_editor_media(
                &services,
                stored_document.id.clone(),
                asset.id.clone(),
                authorized.clone(),
                Some("bytes=2-5".to_string()),
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert_eq!(response.status, 206);
        let expected_content_range = format!("bytes 2-5/{}", bytes.len());
        assert_eq!(
            response.content_range.as_deref(),
            Some(expected_content_range.as_str())
        );
        assert_eq!(response.body, b"2345");

        let unsatisfiable = runtime.block_on(async {
            spawn_persisted_editor_media(
                &services,
                stored_document.id.clone(),
                asset.id.clone(),
                authorized.clone(),
                Some("bytes=100-200".to_string()),
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert_eq!(unsatisfiable.status, 416);
        let expected_unsatisfiable = format!("bytes */{}", bytes.len());
        assert_eq!(
            unsatisfiable.content_range.as_deref(),
            Some(expected_unsatisfiable.as_str())
        );

        let forbidden = runtime.block_on(async {
            spawn_persisted_editor_media(
                &services,
                "another-book".to_string(),
                asset.id.clone(),
                authorized,
                None,
            )
            .await
            .unwrap()
        });
        assert!(forbidden.is_err());
        assert_eq!(Arc::strong_count(&services), 1);
        drop(services);
        drop(runtime);
        close_test_data_dir(data_dir);
    }

    #[test]
    fn editor_media_snapshot_rejects_wrong_book_and_illegal_or_unknown_ids() {
        let bytes = Arc::new(b"owned".to_vec());
        let asset = AssetRef::from_bytes(AssetRole::Audio, "audio/mpeg", None, bytes.as_slice());
        let foreign_document = document_with_audio("book-b", asset.clone());
        let state = EditorWebState::new(
            "book-a".to_string(),
            "EPUB/Text/ch.xhtml".to_string(),
            "<html><body></body></html>".to_string(),
        );
        assert!(state.authorize_media(&foreign_document).is_err());

        let document = document_with_audio("book-a", asset.clone());
        assert!(EditorMediaAsset::checked(&asset, Arc::new(b"tampered".to_vec())).is_err());
        state
            .sync_media(&document, Some((&asset, Arc::clone(&bytes))))
            .unwrap();
        assert!(
            MediaService::new(state.media_snapshot())
                .serve("book-b", &asset.id, None)
                .is_err()
        );
        for uri in [
            "epubeditor://content/.moye/assets/../secret",
            "epubeditor://content/.moye/assets/bad%2Fid",
            "epubeditor://content/.moye/assets/unknown",
        ] {
            let response = editor_protocol_response(&state, &protocol_request(uri, None));
            assert_eq!(response.status(), 404, "{uri}");
        }
    }

    #[test]
    fn replacing_media_atomically_revokes_the_old_preview_resource() {
        let old_bytes = Arc::new(b"old-media".to_vec());
        let old_asset =
            AssetRef::from_bytes(AssetRole::Audio, "audio/mpeg", None, old_bytes.as_slice());
        let old_document = document_with_audio("book-a", old_asset.clone());
        let state = EditorWebState::new(
            "book-a".to_string(),
            "EPUB/Text/ch.xhtml".to_string(),
            "<html><body></body></html>".to_string(),
        );
        state
            .sync_media(&old_document, Some((&old_asset, Arc::clone(&old_bytes))))
            .unwrap();

        let new_bytes = Arc::new(b"new-media".to_vec());
        let new_asset =
            AssetRef::from_bytes(AssetRole::Audio, "audio/mpeg", None, new_bytes.as_slice());
        let new_document = document_with_audio("book-a", new_asset.clone());
        state
            .sync_media(&new_document, Some((&new_asset, Arc::clone(&new_bytes))))
            .unwrap();

        let old = editor_protocol_response(
            &state,
            &protocol_request(
                format!("epubeditor://content{}", editor_asset_path(&old_asset.id)),
                None,
            ),
        );
        assert_eq!(old.status(), 404);
        let new = editor_protocol_response(
            &state,
            &protocol_request(
                format!("epubeditor://content{}", editor_asset_path(&new_asset.id)),
                None,
            ),
        );
        assert_eq!(new.status(), 200);
        assert_eq!(new.body().as_ref(), new_bytes.as_slice());
    }

    #[test]
    fn source_is_the_only_tab_without_a_webview() {
        assert!(!EditorTab::Source.uses_webview());
        assert!(EditorTab::Preview.uses_webview());
        assert!(EditorTab::RichText.uses_webview());
    }
}
