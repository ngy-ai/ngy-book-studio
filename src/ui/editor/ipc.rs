//! The message vocabulary between the host and the editor WebView, and the
//! write jobs those messages schedule.

use super::*;

/// Body snapshot streamed back from the rich-text WebView via `window.ipc`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditorIpcMessage {
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) revision: u64,
    #[serde(default)]
    pub(crate) request_id: Option<u64>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    pub(crate) selected_text: String,
    #[serde(default)]
    pub(crate) too_large: bool,
    #[serde(default)]
    pub(crate) ready: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditorCitationNavigationResult {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) revision: u64,
    pub(crate) request_id: u64,
    pub(crate) found: bool,
    pub(crate) reason: String,
}

#[derive(Clone, Debug)]
pub(crate) struct EditorIpcUpdate {
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) revision: u64,
    pub(crate) request_id: Option<u64>,
    pub(crate) html: String,
    pub(crate) selected_text: Option<String>,
    pub(crate) too_large: bool,
    pub(crate) ready: bool,
    /// `true` when the rich-text page reported an actual document change.
    /// Used to distinguish real edits from "no change" snapshots that just
    /// echo the current page body.
    pub(crate) edited: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum EditorWebEvent {
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
pub(crate) struct EditorWebPage {
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) html: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveEditorPage {
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) revision: u64,
    pub(crate) ready: bool,
}

impl ActiveEditorPage {
    pub(crate) fn is_unready_match(
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
pub(crate) enum PendingEditorAction {
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
pub(crate) struct PendingEditorCitationNavigation {
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) revision: u64,
    pub(crate) request_id: u64,
    pub(crate) focus: AiCitationTextFocus,
}

pub(crate) fn merge_pending_editor_action(
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
pub(crate) struct PendingEditorSnapshot {
    pub(crate) session_id: String,
    pub(crate) chapter_id: String,
    pub(crate) href: String,
    pub(crate) revision: u64,
    pub(crate) request_id: u64,
    pub(crate) action: PendingEditorAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorWriteIntent {
    Save,
    Export,
    Close,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveEditorWrite {
    pub(crate) id: u64,
    pub(crate) intent: EditorWriteIntent,
    pub(crate) draft_generation: u64,
    pub(crate) title: String,
    pub(crate) target_display: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorWriteFollowUp {
    StayOpen,
    Close,
    SaveThenClose,
}

pub(crate) fn editor_write_follow_up(
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

pub(crate) struct EditorWriteJob {
    pub(crate) document: BookDocument,
    pub(crate) new_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    pub(crate) export_target: Option<PathBuf>,
}

pub(crate) enum EditorExportOutcome {
    NotRequested,
    Exported(u64),
    Failed(String),
}

impl EditorExportOutcome {
    pub(crate) fn succeeded(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }
}

pub(crate) struct EditorWriteWorkerResult {
    pub(crate) document: BookDocument,
    pub(crate) cover_bytes: Option<Arc<Vec<u8>>>,
    pub(crate) export: EditorExportOutcome,
    #[cfg(test)]
    pub(crate) worker_thread_id: std::thread::ThreadId,
}

pub(crate) fn spawn_editor_write(
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
        // The published revisions are reported back instead of guessed: an
        // untouched chapter keeps its own revision, so a post-commit read would
        // be the only other way to learn them, and a transient failure of that
        // read must not be misreported as a failed save.
        let mut saved_document = document.clone();
        let published = library.publish_document_with_assets(document, new_asset_bytes)?;
        saved_document.revision = Revision::new(published.record.revision);
        for (unit, revision) in saved_document
            .units
            .iter_mut()
            .zip(published.unit_revisions)
        {
            unit.revision = revision;
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

pub(crate) fn apply_editor_library_projection(
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
pub(crate) struct EditorWebViewBuildGate {
    pub(crate) building: bool,
    pub(crate) close_requested: bool,
}

impl EditorWebViewBuildGate {
    pub(crate) fn new(building: bool) -> Self {
        Self {
            building,
            close_requested: false,
        }
    }

    /// Returns true when native close must be vetoed until the child WebView
    /// builder has stopped using the copied parent HWND.
    pub(crate) fn request_close(&mut self) -> bool {
        if self.building {
            self.close_requested = true;
            true
        } else {
            false
        }
    }

    /// Marks the asynchronous build settled and consumes a deferred close.
    pub(crate) fn finish(&mut self) -> bool {
        self.building = false;
        std::mem::take(&mut self.close_requested)
    }
}

#[derive(Default)]
pub(crate) struct EditorWebPages {
    pub(crate) pages: BTreeMap<u64, EditorWebPage>,
}
