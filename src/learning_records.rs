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

#[path = "learning_catalog.rs"]
mod catalog;
pub use catalog::{
    chapter_course_id, chapter_markdown, chapter_scenarios, chapter_title, reference_code_for,
    starter_code_for, validate_chapter,
};

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
            include_str!("../courses/agent-foundations/lessons/01-prerequisites.md"),
        ),
        (
            "02",
            "2 · 理解工具循环",
            include_str!("../courses/agent-foundations/lessons/02-observe-loop.md"),
        ),
        (
            "03",
            "3 · 手搓 Agent",
            include_str!("../courses/agent-foundations/lessons/03-build-manual.md"),
        ),
        (
            "04",
            "4 · 诊断与修复",
            include_str!("../courses/agent-foundations/lessons/04-recover-failures.md"),
        ),
        (
            "05",
            "5 · LangGraph 重建",
            include_str!("../courses/agent-foundations/lessons/05-build-langgraph.md"),
        ),
        (
            "06",
            "6 · 迁移与复测",
            include_str!("../courses/agent-foundations/lessons/06-transfer-and-review.md"),
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
struct StoredAttempt {
    id: String,
    created_at: String,
    workspace: LearningWorkspace,
    code_digest: String,
    report: Value,
    imported: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordPayload {
    course_id: String,
    course_version: String,
    workspace: LearningWorkspace,
    attempts: Vec<StoredAttempt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordEnvelope {
    format_version: u32,
    digest: String,
    payload: RecordPayload,
}

pub struct LearningStore {
    directory: PathBuf,
    chapter: u8,
}

pub fn new_run_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).context("无法生成学习记录标识")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl LearningStore {
    pub fn prepare_directory(&self) -> Result<()> {
        fs::create_dir_all(self.directory.join("archives")).context("无法创建学习档案目录")
    }
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            chapter: 1,
        }
    }

    pub fn for_chapter(directory: PathBuf, chapter: u8) -> Result<Self> {
        validate_chapter(chapter)?;
        Ok(Self { directory, chapter })
    }

    pub fn chapter(&self) -> u8 {
        self.chapter
    }

    pub fn course_id(&self) -> &'static str {
        chapter_course_id(self.chapter)
    }

    fn record_path(&self) -> PathBuf {
        self.directory
            .join(format!("chapter-{:02}.json", self.chapter))
    }

    fn load(&self) -> Result<RecordPayload> {
        let path = self.record_path();
        if !path.try_exists().context("无法检查学习记录")? {
            return Ok(RecordPayload {
                course_id: self.course_id().into(),
                course_version: COURSE_VERSION.into(),
                workspace: LearningWorkspace::for_chapter(self.chapter)?,
                attempts: vec![],
            });
        }
        decode_for(&read_bounded(&path)?, self.chapter)
    }

    fn write(&self, payload: &RecordPayload) -> Result<()> {
        fs::create_dir_all(&self.directory).context("无法创建学习记录目录")?;
        atomic_write(&self.record_path(), &encode(payload)?)
    }

    pub fn workspace(&self) -> Result<LearningWorkspace> {
        Ok(self.load()?.workspace)
    }

    pub fn save(&self, mut workspace: LearningWorkspace) -> Result<LearningWorkspace> {
        workspace.validate_for(self.chapter)?;
        let mut payload = self.load()?;
        ensure!(
            workspace.revision == payload.workspace.revision,
            "学习记录已更新，请重新载入后再保存"
        );
        workspace.revision = workspace
            .revision
            .checked_add(1)
            .context("学习记录版本溢出")?;
        payload.workspace = workspace.clone();
        self.write(&payload)?;
        Ok(workspace)
    }

    /// Save an immutable first attempt before launching any student code.
    pub fn begin(&self, workspace: LearningWorkspace, id: &str) -> Result<LearningWorkspace> {
        workspace.validate_for(self.chapter)?;
        ensure!(valid_id(id), "运行标识无效");
        let mut payload = self.load()?;
        ensure!(
            workspace.revision == payload.workspace.revision,
            "学习记录已更新，请重新载入"
        );
        ensure!(
            payload.attempts.len() < MAX_ATTEMPTS,
            "已有 64 次运行记录；请点击新一轮，归档已有证据后继续练习"
        );
        ensure!(
            !payload.attempts.iter().any(|attempt| attempt.id == id),
            "运行标识重复"
        );
        let mut saved = workspace;
        saved.revision = saved.revision.checked_add(1).context("学习记录版本溢出")?;
        payload.workspace = saved.clone();
        payload.attempts.push(StoredAttempt {
            id: id.into(), created_at: now(), code_digest: blake3::hash(saved.code().as_bytes()).to_hex().to_string(),
            workspace: saved.clone(), imported: false,
            report: serde_json::json!({"run_id":id,"request":{"chapter":self.chapter,"course_id":self.course_id(),"course_version":COURSE_VERSION,"rules_version":"1.0.0","task_version":"1.0.0","implementation":saved.implementation,"task_id":saved.scenario,"model_mode":"scripted"},"outcome":{"status":"interrupted","stop_reason":"run_not_committed","answer":null},"passed":false,"metrics":{},"observations":{},"learning":{"status":"pending_review"}}),
        });
        self.write(&payload)?;
        Ok(saved)
    }

    pub fn finish(&self, id: &str, report: Value) -> Result<LearningRunReport> {
        ensure!(
            serde_json::to_vec(&report)?.len() <= MAX_REPORT_BYTES,
            "运行报告超过 2 MiB"
        );
        ensure!(
            report.get("run_id").and_then(Value::as_str) == Some(id),
            "运行报告标识不匹配"
        );
        let mut payload = self.load()?;
        let attempt = payload
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .context("运行记录不存在")?;
        ensure!(
            attempt
                .report
                .pointer("/outcome/stop_reason")
                .and_then(Value::as_str)
                == Some("run_not_committed"),
            "已经完成的运行证据不能被覆盖"
        );
        validate_attempt_identity(&report, &attempt.workspace, self.chapter, false)?;
        attempt.report = report;
        let result = present(attempt);
        self.write(&payload)?;
        Ok(result)
    }

    pub fn history(&self) -> Result<Vec<LearningRunSummary>> {
        Ok(self
            .load()?
            .attempts
            .iter()
            .rev()
            .map(|attempt| {
                let text = |key: &str| {
                    attempt
                        .report
                        .pointer(key)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                LearningRunSummary {
                    id: attempt.id.clone(),
                    created_at: attempt.created_at.clone(),
                    status: text("/outcome/status"),
                    stop_reason: text("/outcome/stop_reason"),
                    passed: attempt
                        .report
                        .get("passed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    imported: attempt.imported,
                    implementation: attempt.workspace.implementation.clone(),
                    scenario: attempt.workspace.scenario.clone(),
                }
            })
            .collect())
    }

    pub fn report(&self, id: &str) -> Result<LearningRunReport> {
        ensure!(valid_id(id), "运行标识无效");
        let payload = self.load()?;
        Ok(present(
            payload
                .attempts
                .iter()
                .find(|attempt| attempt.id == id)
                .context("运行记录不存在")?,
        ))
    }

    pub fn export(&self, path: &Path) -> Result<()> {
        let payload = self.load()?;
        self.check_export_destination(path)?;
        atomic_write(path, &encode(&payload)?)
    }

    fn check_export_destination(&self, path: &Path) -> Result<()> {
        // A backup must not overwrite any chapter's live record, including a
        // differently spelled Windows path to the same data directory.
        if let Ok(directory) = fs::canonicalize(&self.directory) {
            let parent = path.parent().context("学习备份路径无效")?;
            let parent = fs::canonicalize(parent).context("无法定位学习备份目录")?;
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            ensure!(
                parent != directory
                    || !(1..=10).any(|chapter| filename
                        .eq_ignore_ascii_case(&format!("chapter-{chapter:02}.json"))),
                "请导出到新的备份文件，不能覆盖任何章节的当前档案"
            );
            if let Ok(target) = fs::canonicalize(path) {
                for chapter in 1..=10 {
                    ensure!(
                        fs::canonicalize(self.directory.join(format!("chapter-{chapter:02}.json")))
                            .ok()
                            .as_ref()
                            != Some(&target),
                        "请导出到新的备份文件，不能覆盖任何章节的当前档案"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn restore(&self, path: &Path) -> Result<()> {
        let mut imported = decode_for(&read_bounded(path)?, self.chapter)?;
        // A valid backup must also be usable when the current envelope is corrupt.
        // Keep its exact bytes for diagnosis before publishing any replacement.
        let current_path = self.record_path();
        let current_bytes = if current_path.try_exists().context("无法检查当前学习档案")?
        {
            read_bounded(&current_path)?
        } else {
            encode(&self.load()?)?
        };
        let current_revision = decode_for(&current_bytes, self.chapter)
            .map(|current| current.workspace.revision)
            .unwrap_or(imported.workspace.revision);
        for attempt in &mut imported.attempts {
            attempt.imported = true;
        }
        imported.workspace.revision = current_revision
            .checked_add(1)
            .context("学习记录版本溢出")?;
        // Archive the current first attempts before publishing a restored workbook.
        fs::create_dir_all(self.directory.join("archives")).context("无法创建学习档案目录")?;
        let backup = self
            .directory
            .join("archives")
            .join(format!("{}.json", new_run_id()?));
        atomic_write(&backup, &current_bytes)?;
        self.write(&imported)
    }

    /// Preserve prior evidence in a restorable archive; keep the current work.
    pub fn new_round(&self, mut workspace: LearningWorkspace) -> Result<()> {
        workspace.validate_for(self.chapter)?;
        let mut payload = self.load()?;
        ensure!(
            workspace.revision == payload.workspace.revision,
            "学习记录已更新，请重新载入后再开始新一轮"
        );
        fs::create_dir_all(self.directory.join("archives")).context("无法创建学习档案目录")?;
        atomic_write(
            &self
                .directory
                .join("archives")
                .join(format!("{}.json", new_run_id()?)),
            &encode(&payload)?,
        )?;
        payload.attempts.clear();
        workspace.revision = workspace
            .revision
            .checked_add(1)
            .context("学习记录版本溢出")?;
        payload.workspace = workspace;
        self.write(&payload)
    }
}

fn present(attempt: &StoredAttempt) -> LearningRunReport {
    let raw = &attempt.report;
    let model_call_label = if raw
        .pointer("/request/chapter")
        .and_then(Value::as_u64)
        .is_some_and(|chapter| chapter > 1)
    {
        "练习输入获取"
    } else {
        "模型决策"
    };
    let text = |pointer: &str| {
        raw.pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut trace = Vec::new();
    let tool_calls = raw
        .pointer("/observations/tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut displayed = std::collections::HashSet::new();
    if let Some(calls) = raw
        .pointer("/observations/model_calls")
        .and_then(Value::as_array)
    {
        for (index, call) in calls.iter().enumerate() {
            trace.push(LearningTrace {
                label: format!("{model_call_label} {}", index + 1),
                detail: serde_json::to_string_pretty(call).unwrap_or_default(),
            });
            if let Some(requested) = call
                .pointer("/response/tool_calls")
                .and_then(Value::as_array)
            {
                for request in requested {
                    if let Some((position, tool)) = tool_calls
                        .iter()
                        .enumerate()
                        .find(|(_, tool)| tool.get("tool_call_id") == request.get("id"))
                        && displayed.insert(position)
                    {
                        trace.push(tool_trace(position, tool));
                    }
                }
            }
        }
    }
    for (index, call) in tool_calls.iter().enumerate() {
        if !displayed.contains(&index) {
            trace.push(tool_trace(index, call));
        }
    }
    if let Some(checks) = raw.get("checks").and_then(Value::as_array) {
        for check in checks {
            trace.push(LearningTrace {
                label: format!(
                    "{} · {}",
                    if check.get("passed").and_then(Value::as_bool) == Some(true) {
                        "通过"
                    } else {
                        "未通过"
                    },
                    check.get("id").and_then(Value::as_str).unwrap_or("检查")
                ),
                detail: check
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            });
        }
    }
    if let Some(error) = raw.get("error").filter(|value| !value.is_null()) {
        trace.push(LearningTrace {
            label: "运行诊断".into(),
            detail: serde_json::to_string_pretty(error).unwrap_or_default(),
        });
    }
    let count = |key: &str| {
        raw.pointer(&format!("/metrics/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    LearningRunReport {
        id: attempt.id.clone(),
        created_at: attempt.created_at.clone(),
        status: text("/outcome/status"),
        stop_reason: text("/outcome/stop_reason"),
        passed: raw.get("passed").and_then(Value::as_bool).unwrap_or(false),
        imported: attempt.imported,
        answer: raw
            .pointer("/outcome/answer")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_default(),
        metrics: LearningMetrics {
            model_decisions: count("model_decisions"),
            actual_tool_executions: count("actual_tool_executions"),
            framework_steps: count("framework_steps"),
            elapsed_seconds: raw
                .pointer("/metrics/elapsed_seconds")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.)
                .unwrap_or(0.),
        },
        trace,
        learning_status: if attempt.imported {
            "导入记录 · 待核验"
        } else {
            "学习证据待核验"
        }
        .into(),
        raw: raw.clone(),
        workspace: attempt.workspace.clone(),
    }
}

fn tool_trace(index: usize, call: &Value) -> LearningTrace {
    LearningTrace {
        label: format!(
            "工具结果 {} · {}",
            index + 1,
            call.get("name").and_then(Value::as_str).unwrap_or("")
        ),
        detail: serde_json::to_string_pretty(call).unwrap_or_default(),
    }
}

fn encode(payload: &RecordPayload) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(payload)?;
    let envelope = RecordEnvelope {
        format_version: 1,
        digest: blake3::hash(&bytes).to_hex().to_string(),
        payload: payload.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&envelope)?;
    ensure!(
        encoded.len() <= MAX_RECORD_BYTES,
        "学习档案超过 32 MiB，请点击新一轮归档后继续"
    );
    Ok(encoded)
}

#[cfg(test)]
fn decode(bytes: &[u8]) -> Result<RecordPayload> {
    decode_for(bytes, 1)
}

fn decode_for(bytes: &[u8], chapter: u8) -> Result<RecordPayload> {
    validate_chapter(chapter)?;
    ensure!(bytes.len() <= MAX_RECORD_BYTES, "学习档案超过 32 MiB");
    let envelope: RecordEnvelope = serde_json::from_slice(bytes).context("学习档案格式无效")?;
    ensure!(envelope.format_version == 1, "不支持此学习档案版本");
    let payload = envelope.payload;
    ensure!(
        payload.course_id == chapter_course_id(chapter) && payload.course_version == COURSE_VERSION,
        "学习档案不属于当前章节，或课程版本不匹配"
    );
    ensure!(
        blake3::hash(&serde_json::to_vec(&payload)?)
            .to_hex()
            .as_str()
            == envelope.digest,
        "学习档案摘要不匹配，文件可能已损坏"
    );
    payload.workspace.validate_for(chapter)?;
    ensure!(
        payload.attempts.len() <= MAX_ATTEMPTS,
        "学习档案运行记录过多"
    );
    let mut seen = std::collections::HashSet::new();
    for attempt in &payload.attempts {
        ensure!(
            valid_id(&attempt.id) && seen.insert(&attempt.id),
            "学习档案运行标识无效或重复"
        );
        attempt.workspace.validate_for(chapter)?;
        ensure!(
            attempt.created_at.len() <= 64 && attempt.created_at.parse::<u64>().is_ok(),
            "学习记录时间无效"
        );
        ensure!(
            blake3::hash(attempt.workspace.code().as_bytes())
                .to_hex()
                .as_str()
                == attempt.code_digest,
            "首次代码快照与摘要不一致"
        );
        ensure!(
            serde_json::to_vec(&attempt.report)?.len() <= MAX_REPORT_BYTES,
            "运行报告过大"
        );
        ensure!(
            attempt.report.get("run_id").and_then(Value::as_str) == Some(&attempt.id),
            "运行报告标识不匹配"
        );
        validate_attempt_identity(&attempt.report, &attempt.workspace, chapter, true)?;
    }
    Ok(payload)
}

fn validate_attempt_identity(
    report: &Value,
    workspace: &LearningWorkspace,
    chapter: u8,
    loading: bool,
) -> Result<()> {
    // Existing first-chapter interrupted attempts predate request metadata.
    // Read those exact records without rewriting the file or weakening new
    // completed reports, later chapters, or the live host protocol.
    if loading
        && chapter == 1
        && report.get("request").is_none()
        && report
            .pointer("/outcome/stop_reason")
            .and_then(Value::as_str)
            == Some("run_not_committed")
        && report.get("passed").and_then(Value::as_bool) == Some(false)
    {
        return Ok(());
    }
    ensure!(
        report.pointer("/request/course_id").and_then(Value::as_str)
            == Some(chapter_course_id(chapter))
            && report
                .pointer("/request/course_version")
                .and_then(Value::as_str)
                == Some(COURSE_VERSION),
        "运行报告课程身份不匹配"
    );
    let reported_chapter = report.pointer("/request/chapter");
    ensure!(
        (chapter == 1 && reported_chapter.is_none())
            || reported_chapter.and_then(Value::as_u64) == Some(u64::from(chapter)),
        "运行报告章节不匹配"
    );
    ensure!(
        report
            .pointer("/request/implementation")
            .and_then(Value::as_str)
            == Some(workspace.implementation.as_str())
            && report.pointer("/request/task_id").and_then(Value::as_str)
                == Some(workspace.scenario.as_str()),
        "运行报告任务与首次作答不匹配"
    );
    if chapter > 1 {
        ensure!(
            report
                .pointer("/request/rules_version")
                .and_then(Value::as_str)
                == Some("1.0.0")
                && report
                    .pointer("/request/task_version")
                    .and_then(Value::as_str)
                    == Some("1.0.0")
                && report
                    .pointer("/request/model_mode")
                    .and_then(Value::as_str)
                    == Some("scripted"),
            "运行报告任务版本或模型模式不匹配"
        );
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path).context("无法读取学习档案")?;
    let mut bytes = vec![];
    file.take((MAX_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("学习档案读取失败")?;
    ensure!(bytes.len() <= MAX_RECORD_BYTES, "学习档案超过 32 MiB");
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("学习档案路径无效")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("无法创建学习档案临时文件")?;
    temporary.write_all(bytes).context("无法写入学习档案")?;
    temporary.as_file().sync_all().context("无法同步学习档案")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("无法提交学习档案")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(id: &str) -> Value {
        report_for(id, 1, &LearningWorkspace::default())
    }

    fn report_for(id: &str, chapter: u8, workspace: &LearningWorkspace) -> Value {
        json!({"run_id":id,"request":{"chapter":chapter,"course_id":chapter_course_id(chapter),"course_version":COURSE_VERSION,"rules_version":"1.0.0","task_version":"1.0.0","implementation":workspace.implementation,"task_id":workspace.scenario,"model_mode":"scripted"},"passed":true,"outcome":{"status":"completed","stop_reason":"final_answer","answer":{"start_date":{"value":"2026-10-12","source_ids":["D1"]}}},"metrics":{"model_decisions":3,"actual_tool_executions":3},"checks":[{"id":"sources","passed":true,"detail":"正文证据"}]})
    }

    #[test]
    fn chapter_workspaces_have_exact_assets_scenarios_and_lesson_bounds() {
        assert_eq!(
            LearningWorkspace::for_chapter(1).unwrap(),
            LearningWorkspace::default()
        );
        assert_eq!(lessons_for(1).unwrap().len(), 6);
        assert_eq!(
            chapter_scenarios(1)
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            SCENARIOS
        );
        for chapter in 2..=10 {
            let mut workspace = LearningWorkspace::for_chapter(chapter).unwrap();
            assert!(!workspace.manual_code.is_empty() && !workspace.langgraph_code.is_empty());
            assert!(!reference_code_for(chapter, "manual").is_empty());
            assert!(!reference_code_for(chapter, "langgraph").is_empty());
            assert_eq!(lessons_for(chapter).unwrap().len(), 1);
            for scenario in ["normal", "fault", "transfer"] {
                workspace.scenario = scenario.into();
                assert!(workspace.validate_for(chapter).is_ok());
            }
            workspace.lesson_index = 1;
            assert!(workspace.validate_for(chapter).is_err());
            workspace.lesson_index = 0;
            workspace.scenario = "transient".into();
            assert!(workspace.validate_for(chapter).is_err());
        }
        for chapter in [0, 11, u8::MAX] {
            assert!(LearningWorkspace::for_chapter(chapter).is_err());
            assert!(LearningWorkspace::default().validate_for(chapter).is_err());
        }
    }

    #[test]
    fn chapters_keep_work_history_and_first_attempts_separate() {
        let temporary = tempfile::tempdir().unwrap();
        let first = LearningStore::new(temporary.path().to_path_buf());
        let second = LearningStore::for_chapter(temporary.path().to_path_buf(), 2).unwrap();
        let id = new_run_id().unwrap();
        let first_work = LearningWorkspace {
            notes: "first chapter".into(),
            lesson_index: 5,
            ..Default::default()
        };
        first.begin(first_work.clone(), &id).unwrap();
        first.finish(&id, report_for(&id, 1, &first_work)).unwrap();
        let first_bytes = fs::read(first.record_path()).unwrap();
        let second_work = LearningWorkspace {
            notes: "second chapter".into(),
            prediction: "check tool schema".into(),
            help_level: "H2".into(),
            implementation: "langgraph".into(),
            scenario: "fault".into(),
            ..LearningWorkspace::for_chapter(2).unwrap()
        };
        let mut second_saved = second.begin(second_work.clone(), &id).unwrap();
        second
            .finish(&id, report_for(&id, 2, &second_work))
            .unwrap();
        second_saved.notes = "edited second chapter".into();
        second.save(second_saved).unwrap();
        assert_eq!(
            second.report(&id).unwrap().workspace.notes,
            "second chapter"
        );
        assert_eq!(first.report(&id).unwrap().workspace.notes, "first chapter");
        assert_eq!(first.history().unwrap()[0].scenario, "normal");
        assert_eq!(second.history().unwrap()[0].scenario, "fault");
        assert_eq!(fs::read(first.record_path()).unwrap(), first_bytes);
        assert_eq!(
            LearningStore::new(temporary.path().to_path_buf())
                .workspace()
                .unwrap()
                .lesson_index,
            5
        );
    }

    #[test]
    fn cross_chapter_restore_and_export_cannot_replace_live_records() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("records");
        let first = LearningStore::new(directory.clone());
        let second = LearningStore::for_chapter(directory.clone(), 2).unwrap();
        first.save(LearningWorkspace::default()).unwrap();
        second
            .save(LearningWorkspace::for_chapter(2).unwrap())
            .unwrap();
        let first_bytes = fs::read(first.record_path()).unwrap();
        let second_bytes = fs::read(second.record_path()).unwrap();
        let first_backup = temporary.path().join("first.json");
        let second_backup = temporary.path().join("second.json");
        first.export(&first_backup).unwrap();
        second.export(&second_backup).unwrap();
        assert!(second.restore(&first_backup).is_err());
        assert!(first.restore(&second_backup).is_err());
        assert!(second.export(&first.record_path()).is_err());
        assert!(
            first
                .export(&directory.join(".").join("chapter-02.json"))
                .is_err()
        );
        assert_eq!(fs::read(first.record_path()).unwrap(), first_bytes);
        assert_eq!(fs::read(second.record_path()).unwrap(), second_bytes);
        assert!(
            !directory.join("archives").exists(),
            "rejected restore must not publish an archive"
        );
    }

    #[test]
    fn report_identity_is_checked_on_finish_and_recomputed_backup_load() {
        let temporary = tempfile::tempdir().unwrap();
        let store = LearningStore::for_chapter(temporary.path().to_path_buf(), 2).unwrap();
        let id = new_run_id().unwrap();
        let workspace = store
            .begin(LearningWorkspace::for_chapter(2).unwrap(), &id)
            .unwrap();
        let valid = report_for(&id, 2, &workspace);
        let before = fs::read(store.record_path()).unwrap();
        for variant in [
            "course",
            "chapter",
            "missing",
            "task",
            "rules_version",
            "task_version",
            "model_mode",
            "implementation",
        ] {
            let mut forged = valid.clone();
            match variant {
                "course" => forged["request"]["course_id"] = json!(COURSE_ID),
                "chapter" => forged["request"]["chapter"] = json!(1),
                "missing" => {
                    forged["request"].as_object_mut().unwrap().remove("chapter");
                }
                "task" => forged["request"]["task_id"] = json!("fault"),
                "rules_version" | "task_version" => forged["request"][variant] = json!("2.0.0"),
                "model_mode" => forged["request"][variant] = json!("live"),
                _ => forged["request"]["implementation"] = json!("langgraph"),
            }
            assert!(store.finish(&id, forged.clone()).is_err(), "{variant}");
            assert_eq!(fs::read(store.record_path()).unwrap(), before);
            let mut payload = store.load().unwrap();
            payload.attempts[0].report = forged;
            assert!(
                decode_for(&encode(&payload).unwrap(), 2).is_err(),
                "{variant}"
            );
        }
        store.finish(&id, valid).unwrap();
    }

    #[test]
    fn old_first_chapter_interrupted_records_load_without_rewriting() {
        let temporary = tempfile::tempdir().unwrap();
        let store = LearningStore::new(temporary.path().to_path_buf());
        let id = new_run_id().unwrap();
        store
            .begin(
                LearningWorkspace {
                    lesson_index: 5,
                    notes: "old first notes".into(),
                    ..Default::default()
                },
                &id,
            )
            .unwrap();
        let mut payload = store.load().unwrap();
        payload.attempts[0]
            .report
            .as_object_mut()
            .unwrap()
            .remove("request");
        let original = encode(&payload).unwrap();
        fs::write(store.record_path(), &original).unwrap();
        let reopened = LearningStore::new(temporary.path().to_path_buf());
        assert_eq!(reopened.workspace().unwrap().notes, "old first notes");
        assert_eq!(reopened.workspace().unwrap().lesson_index, 5);
        assert_eq!(
            reopened.report(&id).unwrap().stop_reason,
            "run_not_committed"
        );
        assert_eq!(fs::read(reopened.record_path()).unwrap(), original);
        assert!(
            LearningStore::for_chapter(temporary.path().to_path_buf(), 2)
                .unwrap()
                .history()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn old_first_chapter_completed_records_without_chapter_metadata_still_load() {
        let temporary = tempfile::tempdir().unwrap();
        let store = LearningStore::new(temporary.path().to_path_buf());
        let id = new_run_id().unwrap();
        store.begin(LearningWorkspace::default(), &id).unwrap();
        let mut old_report = report(&id);
        old_report["request"]
            .as_object_mut()
            .unwrap()
            .remove("chapter");
        store.finish(&id, old_report).unwrap();
        let original = fs::read(store.record_path()).unwrap();
        let reopened = LearningStore::new(temporary.path().to_path_buf());
        assert!(reopened.report(&id).unwrap().passed);
        assert_eq!(fs::read(reopened.record_path()).unwrap(), original);
    }

    #[test]
    fn later_chapter_restore_and_round_archive_keep_their_course_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let store = LearningStore::for_chapter(temporary.path().join("records"), 10).unwrap();
        let id = new_run_id().unwrap();
        let saved = store
            .begin(
                LearningWorkspace {
                    notes: "capstone evidence".into(),
                    ..LearningWorkspace::for_chapter(10).unwrap()
                },
                &id,
            )
            .unwrap();
        store.finish(&id, report_for(&id, 10, &saved)).unwrap();
        let backup = temporary.path().join("chapter10-backup.json");
        store.export(&backup).unwrap();
        store.new_round(saved).unwrap();
        assert!(store.history().unwrap().is_empty());
        store.restore(&backup).unwrap();
        assert!(store.report(&id).unwrap().imported);
        for file in fs::read_dir(store.directory.join("archives")).unwrap() {
            let payload = decode_for(&fs::read(file.unwrap().path()).unwrap(), 10).unwrap();
            assert_eq!(payload.course_id, "agent-foundations.chapter-10");
            assert_eq!(payload.workspace.notes, "capstone evidence");
        }
    }

    #[test]
    fn first_attempt_and_prediction_survive_edits_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().join("learning"));
        let id = new_run_id().unwrap();
        let original = LearningWorkspace {
            prediction: "我预测 D2 只重试一次".into(),
            manual_code: "first attempt".into(),
            ..Default::default()
        };
        let mut saved = store.begin(original.clone(), &id).unwrap();
        assert_eq!(store.report(&id).unwrap().stop_reason, "run_not_committed");
        saved.manual_code = "fixed attempt".into();
        saved.prediction = "edited prediction".into();
        store.save(saved).unwrap();
        store.finish(&id, report(&id)).unwrap();
        let reopened = LearningStore::new(directory.path().join("learning"));
        assert_eq!(reopened.workspace().unwrap().manual_code, "fixed attempt");
        let records = reopened.load().unwrap();
        assert_eq!(
            records.attempts[0].workspace.manual_code,
            original.manual_code
        );
        assert_eq!(
            records.attempts[0].workspace.prediction,
            original.prediction
        );
        assert!(reopened.finish(&id, report(&id)).is_err());
    }

    #[test]
    fn export_restore_preserves_evidence_and_archives_current_work() {
        let directory = tempfile::tempdir().unwrap();
        let source = LearningStore::new(directory.path().join("source"));
        let target = LearningStore::new(directory.path().join("target"));
        let id = new_run_id().unwrap();
        let work = LearningWorkspace {
            notes: "独立解释：宿主执行工具，模型决定调用".into(),
            langgraph_code: "graph notes".into(),
            ..Default::default()
        };
        source.begin(work, &id).unwrap();
        source.finish(&id, report(&id)).unwrap();
        let export = directory.path().join("export.json");
        source.export(&export).unwrap();
        let current = LearningWorkspace {
            notes: "恢复前的作答，不能丢失".into(),
            ..Default::default()
        };
        target.save(current.clone()).unwrap();
        target.restore(&export).unwrap();
        assert_eq!(
            target.workspace().unwrap().notes,
            source.workspace().unwrap().notes
        );
        assert_eq!(target.workspace().unwrap().langgraph_code, "graph notes");
        let restored = target.report(&id).unwrap();
        assert!(restored.imported && restored.passed);
        assert_eq!(restored.learning_status, "导入记录 · 待核验");
        let archive = fs::read_dir(target.directory.join("archives"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            decode(&fs::read(archive).unwrap()).unwrap().workspace.notes,
            current.notes
        );
    }

    #[test]
    fn invalid_restore_never_replaces_or_resets_existing_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().join("store"));
        store.save(LearningWorkspace::default()).unwrap();
        let path = store.directory.join("chapter-01.json");
        let before = fs::read(&path).unwrap();
        let candidate = directory.path().join("bad.json");
        let envelope: Value = serde_json::from_slice(&before).unwrap();
        for variant in ["digest", "version", "course", "unknown", "syntax"] {
            let mut value = envelope.clone();
            match variant {
                "digest" => value["payload"]["workspace"]["notes"] = json!("corrupt"),
                "version" => value["format_version"] = json!(2),
                "course" => value["payload"]["course_id"] = json!("other"),
                "unknown" => value["extra"] = json!(true),
                _ => value = json!("invalid envelope"),
            }
            fs::write(&candidate, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(store.restore(&candidate).is_err(), "{variant}");
            assert_eq!(fs::read(&path).unwrap(), before, "{variant}");
        }
    }

    #[test]
    fn valid_backup_can_recover_corrupt_current_records_without_losing_raw_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().join("store"));
        let saved = store
            .save(LearningWorkspace {
                notes: "应恢复的作答".into(),
                ..Default::default()
            })
            .unwrap();
        let backup = directory.path().join("good.json");
        store.export(&backup).unwrap();
        let corrupt = b"{ interrupted write or corrupt bytes";
        fs::write(store.directory.join("chapter-01.json"), corrupt).unwrap();
        assert!(store.workspace().is_err());
        store.restore(&backup).unwrap();
        assert_eq!(store.workspace().unwrap().notes, saved.notes);
        let archive = fs::read_dir(store.directory.join("archives"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(archive).unwrap(), corrupt);
    }

    #[test]
    fn new_round_accepts_unsaved_work_without_growing_the_previous_archive() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().join("store"));
        let id = new_run_id().unwrap();
        let mut workspace = store.begin(LearningWorkspace::default(), &id).unwrap();
        workspace.notes = "这一轮的新笔记".into();
        store.new_round(workspace.clone()).unwrap();
        assert_eq!(store.workspace().unwrap().notes, workspace.notes);
        assert!(store.history().unwrap().is_empty());
        let archive = fs::read_dir(store.directory.join("archives"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let original = decode(&fs::read(archive).unwrap()).unwrap();
        assert!(original.workspace.notes.is_empty());
        assert_eq!(original.attempts[0].id, id);
    }

    #[test]
    fn save_conflict_and_failed_publish_keep_current_data() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().join("store"));
        let saved = store.save(LearningWorkspace::default()).unwrap();
        assert!(store.save(LearningWorkspace::default()).is_err());
        assert_eq!(store.workspace().unwrap(), saved);
        let destination = directory.path().join("directory.json");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "keep").unwrap();
        assert!(store.export(&destination).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "keep"
        );
        assert_eq!(store.workspace().unwrap(), saved);
    }

    #[test]
    fn new_round_keeps_work_and_old_evidence_restorable() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().join("store"));
        let id = new_run_id().unwrap();
        let saved = store
            .begin(
                LearningWorkspace {
                    notes: "继续补练".into(),
                    ..Default::default()
                },
                &id,
            )
            .unwrap();
        store.finish(&id, report(&id)).unwrap();
        store.new_round(saved.clone()).unwrap();
        assert!(store.history().unwrap().is_empty());
        assert_eq!(store.workspace().unwrap().notes, saved.notes);
        assert!(store.workspace().unwrap().revision > saved.revision);
        let archive = fs::read_dir(store.directory.join("archives"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        store.restore(&archive).unwrap();
        assert_eq!(store.history().unwrap()[0].id, id);
    }

    #[test]
    fn traces_interleave_model_decisions_and_their_tool_results() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().to_path_buf());
        let id = new_run_id().unwrap();
        store.begin(LearningWorkspace::default(), &id).unwrap();
        let mut evidence = report(&id);
        evidence["observations"] = json!({"model_calls":[{"response":{"tool_calls":[{"id":"call1"}]}},{"response":{"content":"answer"}}],"tool_calls":[{"tool_call_id":"call1","name":"read_document","result":{"text":"fixture"}}]});
        let shown = store.finish(&id, evidence).unwrap();
        assert!(shown.trace[0].label.starts_with("模型决策"));
        assert!(shown.trace[1].label.starts_with("工具结果"));
        assert!(shown.trace[2].label.starts_with("模型决策"));
    }

    #[test]
    fn later_chapter_input_transport_is_not_presented_as_a_model_decision() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::for_chapter(directory.path().to_path_buf(), 2).unwrap();
        let id = new_run_id().unwrap();
        let workspace = store
            .begin(LearningWorkspace::for_chapter(2).unwrap(), &id)
            .unwrap();
        let mut evidence = report_for(&id, 2, &workspace);
        evidence["observations"] = json!({
            "model_calls": [{"response": {"content": "runtime exercise input"}}],
            "tool_calls": [{"tool_call_id": "read", "name": "read_document", "result": {"text": "fixture"}}]
        });
        let shown = store.finish(&id, evidence).unwrap();
        assert_eq!(shown.trace[0].label, "练习输入获取 1");
        assert!(shown.trace[1].label.starts_with("工具结果"));
        assert_eq!(store.report(&id).unwrap().trace[0].label, "练习输入获取 1");
    }

    #[test]
    fn forged_snapshot_is_rejected_even_with_recomputed_envelope_digest() {
        let directory = tempfile::tempdir().unwrap();
        let store = LearningStore::new(directory.path().to_path_buf());
        let id = new_run_id().unwrap();
        store.begin(LearningWorkspace::default(), &id).unwrap();
        let mut payload = store.load().unwrap();
        payload.attempts[0].workspace.manual_code = "tampered".into();
        assert!(decode(&encode(&payload).unwrap()).is_err());
    }
}
