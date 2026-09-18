//! Chapters, units and the media drafts the editor edits.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorSearchHit {
    pub(crate) chapter_index: usize,
    pub(crate) chapter_title: String,
    pub(crate) snippet: String,
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
pub(crate) fn editor_chapters_from_document(document: &BookDocument) -> Result<Vec<EditorChapter>> {
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
pub(crate) struct EditorUnitState {
    pub(crate) id: String,
    pub(crate) kind: ContentUnitKind,
    pub(crate) source: String,
}

impl EditorUnitState {
    pub(crate) fn from_document(document: &BookDocument) -> Vec<Self> {
        document
            .units
            .iter()
            .map(|unit| Self {
                id: unit.id.clone(),
                kind: unit.kind,
                source: unit.source.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorMediaBlock {
    pub(crate) id: String,
    pub(crate) kind: MediaKind,
    pub(crate) label: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorMediaMetadataDraft {
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
}

impl EditorMediaMetadataDraft {
    pub(crate) fn from_inputs(title: &str, description: &str) -> Result<Self> {
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

pub(crate) fn normalize_editor_media_text(
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

pub(crate) fn read_editor_media_file(path: &std::path::Path, kind: MediaKind) -> Result<Vec<u8>> {
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
pub(crate) struct PreparedEditorMedia {
    pub(crate) draft: MediaDraft,
    pub(crate) asset: AssetRef,
}

impl PreparedEditorMedia {
    pub(crate) fn new(draft: MediaDraft) -> Self {
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
pub(crate) enum EditorMediaModalAction {
    InsertOrReplace {
        media: PreparedEditorMedia,
        replace_block_id: Option<String>,
    },
    Edit {
        target: EditorMediaBlock,
    },
}

#[derive(Clone)]
pub(crate) struct EditorMediaModal {
    pub(crate) action: EditorMediaModalAction,
    pub(crate) kind: MediaKind,
    pub(crate) title_input: Entity<InputState>,
    pub(crate) description_input: Entity<InputState>,
}

/// Which editor view is active for the current chapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EditorTab {
    /// Raw XHTML source in a plain text editor.
    #[default]
    Source,
    /// Read-only rendered view of the chapter.
    Preview,
    /// WYSIWYG editing via a contenteditable webview.
    RichText,
}

impl EditorTab {
    pub(crate) fn uses_webview(self) -> bool {
        self != Self::Source
    }
}
