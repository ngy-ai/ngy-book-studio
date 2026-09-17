//! Asynchronous course workspace and isolated developer-lab execution.

pub use crate::learning_records::{
    LearningLesson, LearningMetrics, LearningRunReport, LearningRunSummary, LearningTrace,
    LearningWorkspace, SCENARIOS, chapter_scenarios, chapter_title, reference_code,
    reference_code_for,
};

use crate::{
    learning_records::{LearningStore, chapter_course_id, lessons_for, validate_chapter},
    runtime::IoRuntime,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

mod host;

mod report;

mod service;

mod supervise;

use host::*;

use report::*;

use supervise::*;

#[cfg(test)]
mod tests;
#[derive(Clone, Debug)]
pub struct LearningEnvironment {
    pub ready: bool,
    pub message: String,
    pub install_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LearningSnapshot {
    pub lessons: Vec<LearningLesson>,
    pub workspace: LearningWorkspace,
    pub history: Vec<LearningRunSummary>,
    pub environment: LearningEnvironment,
}

pub struct LearningService {
    pub(crate) runtime: IoRuntime,
    pub(crate) store: Arc<Mutex<LearningStore>>,
    pub(crate) stores: Arc<[Arc<Mutex<LearningStore>>; 10]>,
    pub(crate) chapter: u8,
    pub(crate) course_root: PathBuf,
    pub(crate) archive_directory: PathBuf,
}
