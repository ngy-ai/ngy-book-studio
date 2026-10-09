//! The panes, close actions and notices the window tracks.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StudyPane {
    Document(CourseDocument),
    Lesson,
    Code,
    Notes,
}

impl Default for StudyPane {
    fn default() -> Self {
        Self::Document(CourseDocument::Tutorial)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidencePane {
    Result,
    Trace,
    Attempt,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Loading,
    Saving,
    Exporting,
    Restoring,
    NewRound,
    LoadingReport,
    SwitchingChapter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    Wait,
    Save,
    Remove,
}

pub(crate) fn close_action(running: bool, pending: bool, dirty: bool) -> CloseAction {
    if running || pending {
        CloseAction::Wait
    } else if dirty {
        CloseAction::Save
    } else {
        CloseAction::Remove
    }
}

#[derive(Default)]
pub(crate) struct LearningWindowTracker {
    pub(crate) opening: bool,
    pub(crate) closing: bool,
    pub(crate) handle: Option<gpui_kit::AnyWindowHandle>,
}

impl gpui_kit::Global for LearningWindowTracker {}

pub(crate) struct LearningNotice {
    pub(crate) text: String,
    pub(crate) error: bool,
}
