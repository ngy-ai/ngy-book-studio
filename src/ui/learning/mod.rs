use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

use gpui_component::text::TextView;
use ngy_book_studio::learning::{
    LearningEnvironment, LearningLesson, LearningRunReport, LearningRunSummary, LearningService,
    LearningSnapshot, LearningTrace, LearningWorkspace,
};
use ngy_book_studio::learning_records::{chapter_scenarios, chapter_title, reference_code_for};

mod app;

mod chapter;

mod course;

mod history;

mod labels;

mod lifecycle;

mod model;

mod open;

mod render;

mod render_header;

mod render_lesson;

mod render_navigation;

mod render_result;

mod render_run;

mod render_trace;

mod run;

mod shared;

pub(crate) use course::*;

pub(crate) use labels::*;

pub(crate) use model::*;

pub(crate) use open::*;

pub(crate) use shared::*;

#[cfg(test)]
mod selection_test;

#[cfg(test)]
mod tutorial_tests;

#[cfg(test)]
mod tests;
/// Native controls only: no child HWND/WebView is introduced by this window.
/// All persistence and execution cross LearningService's asynchronous boundary.
pub(super) struct LearningWindow {
    service: Arc<LearningService>,
    lessons: Vec<LearningLesson>,
    saved_workspace: Option<LearningWorkspace>,
    reload_required: bool,
    environment: Option<LearningEnvironment>,
    history: Vec<LearningRunSummary>,
    manual_input: Entity<InputState>,
    langgraph_input: Entity<InputState>,
    notes_input: Entity<InputState>,
    prediction_input: Entity<InputState>,
    selected_lesson: usize,
    implementation: String,
    scenario: String,
    help_level: String,
    study_pane: StudyPane,
    evidence_pane: EvidencePane,
    showing_reference: bool,
    operation: Option<Operation>,
    running: bool,
    run_generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    report: Option<LearningRunReport>,
    trace: Vec<LearningTrace>,
    trace_selected: Option<usize>,
    omitted_traces: usize,
    closing: bool,
    removing: bool,
    notice: Option<LearningNotice>,
    _subscriptions: Vec<Subscription>,
}
