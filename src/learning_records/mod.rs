//! Versioned course work and evidence, independent of the book database.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[path = "../learning_catalog.rs"]
mod catalog;
pub use catalog::{
    chapter_course_id, chapter_markdown, chapter_scenarios, chapter_title, reference_code_for,
    starter_code_for, validate_chapter,
};

mod codec;

mod present;

mod store;

use codec::*;

use present::*;

use store::*;

// Kept public so `crate::learning_records::*` is unchanged by the split.
pub use store::new_run_id;

#[cfg(test)]
mod tests;
pub const COURSE_ID: &str = "agent-foundations.chapter-01";

pub const COURSE_VERSION: &str = "1.0.0";

pub const MAX_CODE_BYTES: usize = 128 * 1024;

pub const MAX_NOTES_BYTES: usize = 256 * 1024;

pub const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;

pub const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;

pub const MAX_ATTEMPTS: usize = 64;

pub const SCENARIOS: &[&str] = &[
    "normal",
    "invalid_call",
    "invalid_arguments",
    "transient",
    "missing",
    "transfer",
    "model_budget",
    "tool_budget",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LearningWorkspace {
    pub revision: u64,
    pub manual_code: String,
    pub langgraph_code: String,
    pub notes: String,
    pub prediction: String,
    pub help_level: String,
    pub lesson_index: usize,
    pub implementation: String,
    pub scenario: String,
}

impl Default for LearningWorkspace {
    fn default() -> Self {
        Self::for_chapter(1).expect("the first chapter is registered")
    }
}

impl LearningWorkspace {
    pub fn for_chapter(chapter: u8) -> Result<Self> {
        validate_chapter(chapter)?;
        Ok(Self {
            revision: 0,
            manual_code: starter_code_for(chapter, "manual").into(),
            langgraph_code: starter_code_for(chapter, "langgraph").into(),
            notes: String::new(),
            prediction: String::new(),
            help_level: "H0".into(),
            lesson_index: 0,
            implementation: "manual".into(),
            scenario: "normal".into(),
        })
    }

    pub fn code(&self) -> &str {
        if self.implementation == "manual" {
            &self.manual_code
        } else {
            &self.langgraph_code
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_for(1)
    }

    pub fn validate_for(&self, chapter: u8) -> Result<()> {
        validate_chapter(chapter)?;
        ensure!(
            self.manual_code.len() <= MAX_CODE_BYTES && self.langgraph_code.len() <= MAX_CODE_BYTES,
            "每份代码最多 128 KiB"
        );
        ensure!(
            self.notes.len() <= MAX_NOTES_BYTES && self.prediction.len() <= MAX_NOTES_BYTES,
            "作答或预测最多 256 KiB"
        );
        ensure!(
            self.lesson_index < if chapter == 1 { 6 } else { 1 },
            "课程步骤无效"
        );
        ensure!(
            matches!(self.implementation.as_str(), "manual" | "langgraph"),
            "实现类型无效"
        );
        ensure!(
            chapter_scenarios(chapter)
                .iter()
                .any(|(id, _)| *id == self.scenario),
            "课程场景无效"
        );
        ensure!(
            ["H0", "H1", "H2", "H3", "S"].contains(&self.help_level.as_str()),
            "提示级别无效"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LearningLesson {
    pub id: String,
    pub title: String,
    pub markdown: String,
}

pub fn lessons() -> Vec<LearningLesson> {
    [
        (
            "01",
            "1 · 基础诊断",
            include_str!("../../courses/agent-foundations/lessons/01-prerequisites.md"),
        ),
        (
            "02",
            "2 · 理解工具循环",
            include_str!("../../courses/agent-foundations/lessons/02-observe-loop.md"),
        ),
        (
            "03",
            "3 · 手搓 Agent",
            include_str!("../../courses/agent-foundations/lessons/03-build-manual.md"),
        ),
        (
            "04",
            "4 · 诊断与修复",
            include_str!("../../courses/agent-foundations/lessons/04-recover-failures.md"),
        ),
        (
            "05",
            "5 · LangGraph 重建",
            include_str!("../../courses/agent-foundations/lessons/05-build-langgraph.md"),
        ),
        (
            "06",
            "6 · 迁移与复测",
            include_str!("../../courses/agent-foundations/lessons/06-transfer-and-review.md"),
        ),
    ]
    .into_iter()
    .map(|(id, title, markdown)| LearningLesson {
        id: id.into(),
        title: title.into(),
        markdown: markdown.into(),
    })
    .collect()
}

pub fn reference_code(implementation: &str) -> &'static str {
    reference_code_for(1, implementation)
}

pub fn lessons_for(chapter: u8) -> Result<Vec<LearningLesson>> {
    validate_chapter(chapter)?;
    if chapter == 1 {
        return Ok(lessons());
    }
    Ok(vec![LearningLesson {
        id: format!("chapter-{chapter:02}"),
        title: chapter_title(chapter).into(),
        markdown: chapter_markdown(chapter).into(),
    }])
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LearningMetrics {
    pub model_decisions: u64,
    pub actual_tool_executions: u64,
    pub framework_steps: u64,
    pub elapsed_seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningTrace {
    pub label: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct LearningRunReport {
    pub id: String,
    pub created_at: String,
    pub status: String,
    pub stop_reason: String,
    pub passed: bool,
    pub imported: bool,
    pub answer: String,
    pub metrics: LearningMetrics,
    pub trace: Vec<LearningTrace>,
    pub learning_status: String,
    pub raw: Value,
    pub workspace: LearningWorkspace,
}

#[derive(Clone, Debug)]
pub struct LearningRunSummary {
    pub id: String,
    pub created_at: String,
    pub status: String,
    pub stop_reason: String,
    pub passed: bool,
    pub imported: bool,
    pub implementation: String,
    pub scenario: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredAttempt {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) workspace: LearningWorkspace,
    pub(crate) code_digest: String,
    pub(crate) report: Value,
    pub(crate) imported: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordPayload {
    pub(crate) course_id: String,
    pub(crate) course_version: String,
    pub(crate) workspace: LearningWorkspace,
    pub(crate) attempts: Vec<StoredAttempt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordEnvelope {
    pub(crate) format_version: u32,
    pub(crate) digest: String,
    pub(crate) payload: RecordPayload,
}

pub struct LearningStore {
    pub(crate) directory: PathBuf,
    pub(crate) chapter: u8,
}
