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
    runtime: IoRuntime,
    store: Arc<Mutex<LearningStore>>,
    stores: Arc<[Arc<Mutex<LearningStore>>; 10]>,
    chapter: u8,
    course_root: PathBuf,
    archive_directory: PathBuf,
}

impl LearningService {
    pub fn new(data_directory: PathBuf, runtime: IoRuntime) -> Self {
        let stores: Arc<[Arc<Mutex<LearningStore>>; 10]> = Arc::new(std::array::from_fn(|index| {
            Arc::new(Mutex::new(
                LearningStore::for_chapter(data_directory.clone(), (index + 1) as u8)
                    .expect("the ten desktop chapters are registered"),
            ))
        }));
        Self {
            archive_directory: data_directory.join("archives"),
            runtime,
            store: Arc::clone(&stores[0]),
            stores,
            chapter: 1,
            course_root: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("courses/agent-foundations"),
        }
    }

    pub fn chapter(&self) -> u8 {
        self.chapter
    }

    pub fn course_id(&self) -> &'static str {
        chapter_course_id(self.chapter)
    }

    pub fn for_chapter(&self, chapter: u8) -> Result<Self> {
        validate_chapter(chapter)?;
        Ok(Self {
            runtime: self.runtime.clone(),
            store: Arc::clone(&self.stores[usize::from(chapter - 1)]),
            stores: Arc::clone(&self.stores),
            chapter,
            course_root: self.course_root.clone(),
            archive_directory: self.archive_directory.clone(),
        })
    }

    pub fn archive_directory(&self) -> PathBuf {
        self.archive_directory.clone()
    }

    pub async fn restore_dialog_directory(&self) -> Result<PathBuf> {
        let directory = self.archive_directory();
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    std::fs::create_dir_all(&directory).context("无法准备学习备份目录")?;
                    let directory =
                        std::fs::canonicalize(directory).context("无法定位学习备份目录")?;
                    Ok(crate::startup::shell_dialog_directory(&directory))
                })
                .await?
            })
            .await?
    }

    pub async fn snapshot(&self) -> Result<LearningSnapshot> {
        let store = Arc::clone(&self.store);
        let root = self.course_root.clone();
        let chapter = self.chapter;
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let store = store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?;
                    store.prepare_directory()?;
                    Ok(LearningSnapshot {
                        lessons: lessons_for(chapter)?,
                        workspace: store.workspace()?,
                        history: store.history()?,
                        environment: probe_environment(&root),
                    })
                })
                .await?
            })
            .await?
    }

    pub async fn save(&self, workspace: LearningWorkspace) -> Result<LearningWorkspace> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .save(workspace)
                })
                .await?
            })
            .await?
    }

    pub async fn export(&self, path: PathBuf) -> Result<()> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .export(&path)
                })
                .await?
            })
            .await?
    }

    pub async fn restore(&self, path: PathBuf) -> Result<LearningSnapshot> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .restore(&path)
                })
                .await?
            })
            .await??;
        self.snapshot().await
    }

    pub async fn load_report(&self, id: String) -> Result<LearningRunReport> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .report(&id)
                })
                .await?
            })
            .await?
    }

    pub async fn new_round(&self, workspace: LearningWorkspace) -> Result<LearningSnapshot> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .new_round(workspace)
                })
                .await?
            })
            .await??;
        self.snapshot().await
    }

    pub async fn run(
        &self,
        workspace: LearningWorkspace,
        cancellation: Arc<AtomicBool>,
        events: async_channel::Sender<LearningTrace>,
    ) -> Result<LearningRunReport> {
        let store = Arc::clone(&self.store);
        let root = self.course_root.clone();
        let chapter = self.chapter;
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let id = crate::learning_records::new_run_id()?;
                    let saved = store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .begin(workspace, &id)?;
                    let started = Instant::now();
                    let scope = RunScope { chapter, id: &id };
                    let report = match execute(&root, &saved, scope, &cancellation, &events) {
                        Ok(report) => report,
                        Err(error) => failed_report(
                            scope,
                            &saved,
                            "error",
                            "host_error",
                            &format!("{error:#}"),
                            started.elapsed(),
                        ),
                    };
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .finish(&id, report)
                })
                .await?
            })
            .await?
    }
}

const MAX_PROTOCOL_BYTES: u64 = 4 * 1024 * 1024;
const HOST_DEADLINE: Duration = Duration::from_secs(185);
const CANCEL_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy)]
struct RunScope<'a> {
    chapter: u8,
    id: &'a str,
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host_command(root: &Path) -> Command {
    let mut command = Command::new(root.join(".venv/Scripts/python.exe"));
    command
        .args(["-I", "-u"])
        .arg(root.join("ngy_lab/desktop_host.py"));
    command.current_dir(root).env_clear();
    for key in [
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no helper consoles.
    }
    command
}

/// Only this reviewed host gets a normal process token. It never imports the
/// submission: that code is passed to an LPAC worker controlled by its Job Object.
fn execute(
    root: &Path,
    workspace: &LearningWorkspace,
    scope: RunScope<'_>,
    cancellation: &AtomicBool,
    events: &async_channel::Sender<LearningTrace>,
) -> Result<Value> {
    ensure!(
        cfg!(target_os = "windows"),
        "隔离运行目前只支持 Windows 10/11"
    );
    let directory = tempfile::Builder::new()
        .prefix("ngy-learning-run-")
        .tempdir()
        .context("无法创建独立运行目录")?;
    let started = Instant::now();
    let result = supervise(
        root,
        workspace,
        scope,
        directory.path(),
        cancellation,
        events,
    );
    // This also removes a profile left by a forcibly terminated host. Only the
    // task-owned, absolute directory is passed; the worker cannot write its marker.
    let cleanup = cleanup_profile(root, directory.path());
    match (result, cleanup) {
        (Ok(report), Ok(())) => Ok(report),
        (result, Err(error)) => {
            tracing::error!(error=%error, "learning isolation cleanup failed");
            let retained = directory.keep();
            Ok(failed_report(
                scope,
                workspace,
                "error",
                "cleanup_failed",
                &format!(
                    "隔离资源清理失败，运行目录已保留用于诊断：{}；{error:#}；运行结果：{}",
                    retained.display(),
                    if result.is_ok() {
                        "已结束"
                    } else {
                        "异常结束"
                    }
                ),
                started.elapsed(),
            ))
        }
        (Err(error), Ok(())) => Err(error),
    }
}

enum PipeEvent {
    Line(Vec<u8>),
    End,
    Error(String),
}

fn supervise(
    root: &Path,
    workspace: &LearningWorkspace,
    scope: RunScope<'_>,
    run_dir: &Path,
    cancellation: &AtomicBool,
    events: &async_channel::Sender<LearningTrace>,
) -> Result<Value> {
    supervise_command(
        host_command(root),
        workspace,
        scope,
        run_dir,
        cancellation,
        events,
        SupervisorTiming {
            host_deadline: HOST_DEADLINE,
            cancel_grace: CANCEL_GRACE,
        },
    )
}

#[derive(Clone, Copy)]
struct SupervisorTiming {
    host_deadline: Duration,
    cancel_grace: Duration,
}

fn supervise_command(
    mut command: Command,
    workspace: &LearningWorkspace,
    scope: RunScope<'_>,
    run_dir: &Path,
    cancellation: &AtomicBool,
    events: &async_channel::Sender<LearningTrace>,
    timing: SupervisorTiming,
) -> Result<Value> {
    let RunScope { chapter, id } = scope;
    workspace.validate_for(chapter)?;
    let started = Instant::now();
    if cancellation.load(Ordering::Acquire) {
        return Ok(failed_report(
            scope,
            workspace,
            "cancelled",
            "cancelled",
            "运行启动前已取消",
            started.elapsed(),
        ));
    }
    let mut child = ChildGuard(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("无法启动课程宿主；请在课程目录运行 uv sync --locked")?,
    );
    let mut input = child.0.stdin.take().context("无法打开课程控制通道")?;
    let output = child.0.stdout.take().context("无法打开课程事件通道")?;
    let error_output = child.0.stderr.take().context("无法打开课程诊断通道")?;
    let request = json!({"command":"run","run_id":id,"chapter":chapter,"course_id":chapter_course_id(chapter),"course_version":crate::learning_records::COURSE_VERSION,"rules_version":"1.0.0","task_version":"1.0.0","implementation":workspace.implementation,"scenario":workspace.scenario,"code":workspace.code(),"mode":"scripted","run_dir":run_dir,"limits":{"max_model_decisions":8,"max_tool_executions":6,"max_retries":1,"timeout_seconds":180.0}});
    let mut request_bytes = serde_json::to_vec(&request)?;
    request_bytes.push(b'\n');
    // A host stalled during imports may not consume stdin. Never let the GPUI
    // cancellation/deadline supervisor block behind a full control pipe.
    let (control, commands) = mpsc::sync_channel::<Vec<u8>>(2);
    let writer = thread::spawn(move || -> std::io::Result<()> {
        input.write_all(&request_bytes)?;
        input.flush()?;
        while let Ok(bytes) = commands.recv() {
            input.write_all(&bytes)?;
            input.flush()?;
        }
        Ok(())
    });
    let overflow = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(64);
    let reader_overflow = Arc::clone(&overflow);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(output.take(MAX_PROTOCOL_BYTES + 1));
        let mut total = 0;
        loop {
            let mut line = Vec::new();
            let event = match reader.read_until(b'\n', &mut line) {
                Ok(0) => PipeEvent::End,
                Ok(size) => {
                    total += size;
                    if total as u64 > MAX_PROTOCOL_BYTES
                        || line.len() > crate::learning_records::MAX_REPORT_BYTES + 65536
                    {
                        reader_overflow.store(true, Ordering::Release);
                        break;
                    }
                    PipeEvent::Line(line)
                }
                Err(error) => PipeEvent::Error(error.to_string()),
            };
            let end = !matches!(event, PipeEvent::Line(_));
            if sender.try_send(event).is_err() {
                reader_overflow.store(true, Ordering::Release);
                break;
            }
            if end {
                break;
            }
        }
    });
    let stderr_overflow = Arc::clone(&overflow);
    let diagnostics = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = error_output.take(65537).read_to_end(&mut bytes);
        if bytes.len() > 65536 {
            stderr_overflow.store(true, Ordering::Release);
            bytes.truncate(65536);
        }
        (result, bytes)
    });
    let mut report = None;
    let mut finished_at: Option<Instant> = None;
    let mut stopping: Option<(Instant, &'static str)> = None;
    let mut protocol_error = None;
    let mut output_ended = false;
    let exit_status = loop {
        if stopping.is_none() {
            let reason = if cancellation.load(Ordering::Acquire) {
                Some("cancelled")
            } else if started.elapsed() >= timing.host_deadline {
                Some("host_timeout")
            } else if overflow.load(Ordering::Acquire) {
                Some("output_limit")
            } else if finished_at.is_some_and(|when| when.elapsed() >= timing.cancel_grace) {
                Some("host_exit_timeout")
            } else {
                None
            };
            if let Some(reason) = reason {
                let command = json!({"command":"cancel","run_id":id});
                if let Ok(mut bytes) = serde_json::to_vec(&command) {
                    bytes.push(b'\n');
                    let _ = control.try_send(bytes);
                }
                stopping = Some((Instant::now(), reason));
            }
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(PipeEvent::Line(line)) => {
                let event =
                    serde_json::from_slice::<Value>(&line).context("课程宿主返回了无效事件");
                match event.and_then(|event| {
                    ensure!(
                        event.get("run_id").and_then(Value::as_str) == Some(id),
                        "课程事件不属于本次运行"
                    );
                    match event.get("event").and_then(Value::as_str) {
                        Some("started") => {}
                        Some("progress") => {
                            let label = event
                                .get("label")
                                .and_then(Value::as_str)
                                .context("课程进度标题缺失")?;
                            let detail = event
                                .get("detail")
                                .and_then(Value::as_str)
                                .context("课程进度内容缺失")?;
                            ensure!(label.len() <= 4096 && detail.len() <= 65536, "课程进度过大");
                            let _ = events.try_send(LearningTrace {
                                label: label.into(),
                                detail: detail.into(),
                            });
                        }
                        Some("finished") => {
                            ensure!(report.is_none(), "课程宿主重复提交报告");
                            let value = event
                                .get("report")
                                .filter(|v| v.is_object())
                                .context("课程宿主未生成运行报告")?;
                            validate_report(value, scope, workspace)?;
                            report = Some(value.clone());
                            finished_at = Some(Instant::now());
                        }
                        _ => anyhow::bail!("课程宿主事件类型无效"),
                    }
                    Ok(())
                }) {
                    Ok(()) => {}
                    Err(error) => {
                        protocol_error = Some(format!("{error:#}"));
                        stopping.get_or_insert((
                            Instant::now() - timing.cancel_grace,
                            "protocol_error",
                        ));
                    }
                }
            }
            Ok(PipeEvent::Error(error)) => {
                protocol_error = Some(error);
                stopping.get_or_insert((Instant::now() - timing.cancel_grace, "protocol_error"));
            }
            Ok(PipeEvent::End) | Err(mpsc::RecvTimeoutError::Disconnected) => output_ended = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if stopping
            .as_ref()
            .is_some_and(|(when, _)| when.elapsed() >= timing.cancel_grace)
        {
            let _ = child.0.kill();
        }
        if let Some(status) = child.0.try_wait()?
            && output_ended
        {
            break status;
        }
        if output_ended {
            thread::sleep(Duration::from_millis(10));
        }
    };
    drop(control);
    let _ = writer.join();
    let _ = reader.join();
    let diagnostic = diagnostics
        .join()
        .map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    if let Some(error) = protocol_error {
        return Ok(failed_report(
            scope,
            workspace,
            "error",
            "protocol_error",
            &error,
            started.elapsed(),
        ));
    }
    if let Some((_, reason)) = stopping {
        // A timely cancelled host report retains its observed calls. Other stops
        // cannot be turned into a pass by a late completion event.
        if reason == "cancelled"
            && report
                .as_ref()
                .and_then(|r| r.pointer("/outcome/status"))
                .and_then(Value::as_str)
                == Some("cancelled")
        {
            return Ok(report.unwrap());
        }
        return Ok(failed_report(
            scope,
            workspace,
            if reason == "cancelled" {
                "cancelled"
            } else {
                "error"
            },
            reason,
            "宿主已停止并回收隔离进程",
            started.elapsed(),
        ));
    }
    let report = report.context(format!("课程宿主退出但没有提交报告：{diagnostic}"))?;
    if overflow.load(Ordering::Acquire) || !valid_exit(&report, exit_status.code()) {
        return Ok(failed_report(
            scope,
            workspace,
            "error",
            "host_exit_error",
            &format!("宿主未正常结束（{exit_status}）或输出超限：{diagnostic}"),
            started.elapsed(),
        ));
    }
    Ok(report)
}

fn valid_exit(report: &Value, code: Option<i32>) -> bool {
    code == Some(
        if report.get("passed").and_then(Value::as_bool) == Some(true) {
            0
        } else {
            1
        },
    )
}

fn validate_report(
    report: &Value,
    scope: RunScope<'_>,
    workspace: &LearningWorkspace,
) -> Result<()> {
    let RunScope { chapter, id } = scope;
    workspace.validate_for(chapter)?;
    ensure!(
        report.get("report_version").and_then(Value::as_u64) == Some(1)
            && report
                .pointer("/request/course_version")
                .and_then(Value::as_str)
                == Some(crate::learning_records::COURSE_VERSION)
            && report
                .pointer("/request/rules_version")
                .and_then(Value::as_str)
                == Some("1.0.0"),
        "运行报告协议或评分版本不匹配"
    );
    ensure!(
        report.get("run_id").and_then(Value::as_str) == Some(id),
        "运行报告标识不匹配"
    );
    ensure!(
        report.pointer("/request/course_id").and_then(Value::as_str)
            == Some(chapter_course_id(chapter)),
        "运行报告课程不匹配"
    );
    ensure!(
        report.pointer("/request/chapter").and_then(Value::as_u64) == Some(u64::from(chapter))
            || (chapter == 1 && report.pointer("/request/chapter").is_none()),
        "运行报告章节不匹配"
    );
    if chapter > 1 {
        ensure!(
            report
                .pointer("/request/task_version")
                .and_then(Value::as_str)
                == Some("1.0.0"),
            "运行报告任务版本不匹配"
        );
    }
    ensure!(
        report
            .pointer("/request/implementation")
            .and_then(Value::as_str)
            == Some(&workspace.implementation)
            && report.pointer("/request/task_id").and_then(Value::as_str)
                == Some(&workspace.scenario),
        "运行报告任务不匹配"
    );
    ensure!(
        report
            .pointer("/request/model_mode")
            .and_then(Value::as_str)
            == Some("scripted"),
        "运行报告模型模式不匹配"
    );
    ensure!(
        report
            .get("code_snapshot")
            .and_then(|v| v.get(format!(
                "desktop_submission/{}.py",
                workspace.implementation
            )))
            .and_then(Value::as_str)
            == Some(workspace.code()),
        "报告代码与首次快照不匹配"
    );
    let checks = report
        .get("checks")
        .and_then(Value::as_array)
        .context("运行报告缺少检查依据")?;
    let passed = report
        .get("passed")
        .and_then(Value::as_bool)
        .context("运行报告缺少检查结论")?;
    ensure!(
        !checks.is_empty()
            && passed
                == checks
                    .iter()
                    .all(|c| c.get("passed").and_then(Value::as_bool) == Some(true)),
        "运行报告检查结论矛盾"
    );
    if passed {
        ensure!(
            report
                .pointer("/isolation/verified")
                .and_then(Value::as_bool)
                == Some(true),
            "未验证隔离的运行不能通过"
        );
    }
    ensure!(
        serde_json::to_vec(report)?.len() <= crate::learning_records::MAX_REPORT_BYTES,
        "运行报告超过 2 MiB"
    );
    Ok(())
}

fn cleanup_profile(root: &Path, directory: &Path) -> Result<()> {
    if !directory.join("sandbox-profile.json").exists() {
        return Ok(());
    }
    let mut child = ChildGuard(
        host_command(root)
            .arg("--cleanup")
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("无法启动隔离资源清理")?,
    );
    let started = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait()? {
            ensure!(status.success(), "Windows 隔离配置清理失败");
            return Ok(());
        }
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Windows 隔离配置清理超时"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn failed_report(
    scope: RunScope<'_>,
    workspace: &LearningWorkspace,
    status: &str,
    reason: &str,
    detail: &str,
    elapsed: Duration,
) -> Value {
    json!({"report_version":1,"run_id":scope.id,"request":{"chapter":scope.chapter,"course_id":chapter_course_id(scope.chapter),"course_version":crate::learning_records::COURSE_VERSION,"rules_version":"1.0.0","task_version":"1.0.0","implementation":workspace.implementation,"task_id":workspace.scenario,"model_mode":"scripted"},"outcome":{"status":status,"stop_reason":reason,"answer":null},"passed":false,"metrics":{"elapsed_seconds":elapsed.as_secs_f64()},"observations":{},"error":{"message":detail},"checks":[{"id":"host_completion","passed":false,"detail":"运行未形成完整的宿主检查结果"}],"learning":{"status":"pending_review"}})
}

fn probe_environment(root: &std::path::Path) -> LearningEnvironment {
    let ready = cfg!(target_os = "windows")
        && root.join(".venv/Scripts/python.exe").is_file()
        && root.join("ngy_lab/desktop_host.py").is_file();
    LearningEnvironment {
        ready,
        message: if ready {
            "Python 课程环境已就绪；运行时验证 Windows 隔离".into()
        } else {
            "需要 Windows 和课程 Python 3.12 环境。在 courses/agent-foundations 运行 uv sync --locked 后重试。".into()
        },
        install_url: Some("https://docs.astral.sh/uv/getting-started/installation/".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_services_share_each_chapter_lock_and_keep_saved_work_separate() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = IoRuntime::new(2).unwrap();
        let first = LearningService::new(temporary.path().join("records"), runtime.clone());
        let second = first.for_chapter(2).unwrap();
        let second_again = second.for_chapter(2).unwrap();
        let third = second.for_chapter(3).unwrap();
        assert_eq!(first.chapter(), 1);
        assert_eq!(first.course_id(), crate::learning_records::COURSE_ID);
        assert_eq!(second.chapter(), 2);
        assert_eq!(second.course_id(), "agent-foundations.chapter-02");
        assert!(Arc::ptr_eq(&second.store, &second_again.store));
        assert!(Arc::ptr_eq(&first.stores, &third.stores));
        assert!(!Arc::ptr_eq(&second.store, &third.store));
        assert!(first.for_chapter(0).is_err() && first.for_chapter(11).is_err());
        let first_saved = runtime
            .block_on(first.save(LearningWorkspace {
                lesson_index: 5,
                notes: "original first chapter".into(),
                ..Default::default()
            }))
            .unwrap();
        let second_saved = runtime
            .block_on(second.save(LearningWorkspace {
                notes: "second chapter notes".into(),
                prediction: "validate arguments before execution".into(),
                help_level: "H1".into(),
                ..LearningWorkspace::for_chapter(2).unwrap()
            }))
            .unwrap();
        assert_eq!(
            runtime.block_on(second_again.snapshot()).unwrap().workspace,
            second_saved
        );
        assert_eq!(
            runtime.block_on(first.snapshot()).unwrap().workspace,
            first_saved
        );
        let third_snapshot = runtime.block_on(third.snapshot()).unwrap();
        assert_eq!(
            third_snapshot.workspace,
            LearningWorkspace::for_chapter(3).unwrap()
        );
        assert_eq!(third_snapshot.lessons.len(), 1);
        assert_eq!(runtime.block_on(first.snapshot()).unwrap().lessons.len(), 6);
        assert!(third_snapshot.history.is_empty());
    }

    #[test]
    fn repeated_chapter_services_cannot_both_save_the_same_revision() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = IoRuntime::new(2).unwrap();
        let first = LearningService::new(temporary.path().join("records"), runtime.clone());
        let left = first.for_chapter(4).unwrap();
        let right = first.for_chapter(4).unwrap();
        let (a, b) = runtime.block_on(async {
            tokio::join!(
                left.save(LearningWorkspace {
                    notes: "left".into(),
                    ..LearningWorkspace::for_chapter(4).unwrap()
                }),
                right.save(LearningWorkspace {
                    notes: "right".into(),
                    ..LearningWorkspace::for_chapter(4).unwrap()
                })
            )
        });
        assert_ne!(
            a.is_ok(),
            b.is_ok(),
            "one accepted revision must reject the stale save"
        );
        let saved = a.or(b).unwrap();
        assert_eq!(runtime.block_on(left.snapshot()).unwrap().workspace, saved);
        assert_eq!(runtime.block_on(right.snapshot()).unwrap().workspace, saved);
        assert_eq!(saved.revision, 1);
    }

    #[test]
    fn later_chapter_host_reports_bind_chapter_versions_and_submission() {
        let workspace = LearningWorkspace::for_chapter(2).unwrap();
        let scope = RunScope {
            chapter: 2,
            id: "chapter-two",
        };
        let report = json!({"report_version":1,"run_id":scope.id,"request":{"chapter":2,"course_id":"agent-foundations.chapter-02","course_version":"1.0.0","rules_version":"1.0.0","task_version":"1.0.0","implementation":"manual","task_id":"normal","model_mode":"scripted"},"code_snapshot":{"desktop_submission/manual.py":workspace.code()},"checks":[{"passed":true}],"passed":true,"isolation":{"verified":true}});
        assert!(validate_report(&report, scope, &workspace).is_ok());
        for field in [
            "course",
            "chapter",
            "missing_chapter",
            "chapter_type",
            "task_version",
            "scenario",
            "implementation",
            "code",
            "isolation",
        ] {
            let mut forged = report.clone();
            match field {
                "course" => {
                    forged["request"]["course_id"] = json!(crate::learning_records::COURSE_ID)
                }
                "chapter" => forged["request"]["chapter"] = json!(1),
                "missing_chapter" => {
                    forged["request"].as_object_mut().unwrap().remove("chapter");
                }
                "chapter_type" => forged["request"]["chapter"] = json!("2"),
                "task_version" => forged["request"]["task_version"] = json!("2.0.0"),
                "scenario" => forged["request"]["task_id"] = json!("fault"),
                "implementation" => forged["request"]["implementation"] = json!("langgraph"),
                "code" => {
                    forged["code_snapshot"]["desktop_submission/manual.py"] = json!("forged code")
                }
                _ => forged["isolation"]["verified"] = json!(false),
            }
            assert!(
                validate_report(&forged, scope, &workspace).is_err(),
                "{field}"
            );
        }
        assert!(
            validate_report(
                &report,
                RunScope {
                    chapter: 3,
                    id: scope.id
                },
                &workspace
            )
            .is_err()
        );
    }

    #[test]
    fn restore_dialog_directory_prepares_first_use_without_creating_records() {
        let temporary = tempfile::tempdir().unwrap();
        let data = std::fs::canonicalize(temporary.path())
            .unwrap()
            .join("learning");
        let runtime = IoRuntime::new(1).unwrap();
        let service = LearningService::new(data.clone(), runtime.clone());
        assert!(!service.archive_directory().exists());
        let location = runtime
            .block_on(service.restore_dialog_directory())
            .unwrap();
        assert!(location.is_absolute() && location.is_dir());
        assert_eq!(
            std::fs::canonicalize(&location).unwrap(),
            std::fs::canonicalize(service.archive_directory()).unwrap()
        );
        assert!(!data.join("chapter-01.json").exists());
        #[cfg(target_os = "windows")]
        assert!(!location.as_os_str().to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn restore_dialog_directory_reports_an_unavailable_default_without_replacing_it() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("learning");
        std::fs::write(&data, b"existing file").unwrap();
        let runtime = IoRuntime::new(1).unwrap();
        let service = LearningService::new(data.clone(), runtime.clone());
        assert!(
            runtime
                .block_on(service.restore_dialog_directory())
                .is_err()
        );
        assert_eq!(std::fs::read(data).unwrap(), b"existing file");
    }

    #[cfg(target_os = "windows")]
    mod supervisor_control_flow {
        use super::*;
        use std::{fs, os::windows::process::CommandExt};

        // This trusted fixture only exercises the Rust process protocol. Its
        // synthetic passing envelope is not evidence that a sandbox was tested.
        const FAKE_HOST: &str = r#"
import json
import sys
import time
from pathlib import Path

mode, marker_path = sys.argv[1:]
marker = Path(marker_path)
if mode == "no_read":
    marker.write_text("started_without_reading", encoding="utf-8")
    time.sleep(12)
    raise SystemExit(4)

request = json.loads(sys.stdin.buffer.readline())
implementation = request["implementation"]
report = {
    "report_version": 1,
    "run_id": request["run_id"],
    "request": {
        "course_id": "agent-foundations.chapter-01",
        "course_version": "1.0.0",
        "rules_version": "1.0.0",
        "implementation": implementation,
        "task_id": request["scenario"],
        "model_mode": "scripted",
    },
    "code_snapshot": {"desktop_submission/" + implementation + ".py": request["code"]},
    "checks": [{"id": "controlled_protocol_fixture", "passed": True}],
    "passed": True,
    "isolation": {"verified": True},
    "outcome": {"status": "completed", "stop_reason": "final_answer", "answer": None},
}
print(json.dumps({"event": "finished", "run_id": request["run_id"], "report": report}), flush=True)
marker.write_text("passing_report_submitted", encoding="utf-8")
if mode == "exit3":
    raise SystemExit(3)
if mode == "hang":
    time.sleep(12)
    raise SystemExit(0)
raise SystemExit(5)
"#;

        fn fake_host(mode: &str) -> (tempfile::TempDir, Command, PathBuf) {
            let python = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("courses/agent-foundations/.venv/Scripts/python.exe");
            assert!(
                python.is_file(),
                "run uv sync --locked in courses/agent-foundations first"
            );
            let directory = tempfile::Builder::new()
                .prefix("ngy-supervisor-test-")
                .tempdir()
                .unwrap();
            let script = directory.path().join("controlled_host.py");
            let marker = directory.path().join("fixture-ready.txt");
            fs::write(&script, FAKE_HOST).unwrap();
            let mut command = Command::new(python);
            command
                .args(["-I", "-u"])
                .arg(script)
                .arg(mode)
                .arg(&marker)
                .current_dir(directory.path())
                .env_clear()
                .creation_flags(0x0800_0000);
            for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            (directory, command, marker)
        }

        fn timing() -> SupervisorTiming {
            SupervisorTiming {
                host_deadline: Duration::from_secs(5),
                cancel_grace: Duration::from_millis(250),
            }
        }

        #[test]
        #[ignore = "requires the provisioned local Python course environment on Windows"]
        fn supervisor_cancels_a_host_that_never_reads_the_large_request() {
            let (directory, command, marker) = fake_host("no_read");
            let workspace = LearningWorkspace {
                manual_code: "x".repeat(crate::learning_records::MAX_CODE_BYTES),
                ..Default::default()
            };
            let cancellation = Arc::new(AtomicBool::new(false));
            let trigger = Arc::clone(&cancellation);
            let ready = marker.clone();
            let canceller = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(4);
                while !ready.is_file() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                // Ensure this is cancellation after actual host startup, while
                // the request writer can be blocked behind the unread pipe.
                thread::sleep(Duration::from_millis(100));
                trigger.store(true, Ordering::Release);
            });
            let (events, _receiver) = async_channel::bounded(16);
            let started = Instant::now();
            let report = supervise_command(
                command,
                &workspace,
                RunScope {
                    chapter: 1,
                    id: "unread-request",
                },
                directory.path(),
                &cancellation,
                &events,
                timing(),
            )
            .unwrap();
            canceller.join().unwrap();
            assert_eq!(
                fs::read_to_string(marker).unwrap(),
                "started_without_reading"
            );
            assert_eq!(
                report.pointer("/outcome/status").and_then(Value::as_str),
                Some("cancelled")
            );
            assert_eq!(
                report
                    .pointer("/outcome/stop_reason")
                    .and_then(Value::as_str),
                Some("cancelled")
            );
            assert_eq!(report.get("passed").and_then(Value::as_bool), Some(false));
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "cancellation waited for the fixture's 12-second sleep"
            );
        }

        #[test]
        #[ignore = "requires the provisioned local Python course environment on Windows"]
        fn supervisor_rejects_a_passing_report_followed_by_exit_three() {
            let (directory, command, marker) = fake_host("exit3");
            let (events, _receiver) = async_channel::bounded(16);
            let report = supervise_command(
                command,
                &LearningWorkspace::default(),
                RunScope {
                    chapter: 1,
                    id: "abnormal-exit",
                },
                directory.path(),
                &AtomicBool::new(false),
                &events,
                timing(),
            )
            .unwrap();
            assert_eq!(
                fs::read_to_string(marker).unwrap(),
                "passing_report_submitted"
            );
            assert_eq!(
                report.pointer("/outcome/status").and_then(Value::as_str),
                Some("error")
            );
            assert_eq!(
                report
                    .pointer("/outcome/stop_reason")
                    .and_then(Value::as_str),
                Some("host_exit_error")
            );
            assert_eq!(report.get("passed").and_then(Value::as_bool), Some(false));
        }

        #[test]
        #[ignore = "requires the provisioned local Python course environment on Windows"]
        fn supervisor_kills_a_host_that_hangs_after_submitting_a_pass() {
            let (directory, command, marker) = fake_host("hang");
            let (events, _receiver) = async_channel::bounded(16);
            let started = Instant::now();
            let report = supervise_command(
                command,
                &LearningWorkspace::default(),
                RunScope {
                    chapter: 1,
                    id: "hang-after-report",
                },
                directory.path(),
                &AtomicBool::new(false),
                &events,
                timing(),
            )
            .unwrap();
            assert_eq!(
                fs::read_to_string(marker).unwrap(),
                "passing_report_submitted"
            );
            assert_eq!(
                report.pointer("/outcome/status").and_then(Value::as_str),
                Some("error")
            );
            assert_eq!(
                report
                    .pointer("/outcome/stop_reason")
                    .and_then(Value::as_str),
                Some("host_exit_timeout")
            );
            assert_eq!(report.get("passed").and_then(Value::as_bool), Some(false));
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "post-report shutdown did not enforce its deadline"
            );
        }
    }

    #[test]
    fn report_checks_bind_identity_submission_and_isolation() {
        let workspace = LearningWorkspace::default();
        let mut report = json!({"report_version":1,"run_id":"test","request":{"course_id":crate::learning_records::COURSE_ID,"course_version":"1.0.0","rules_version":"1.0.0","implementation":"manual","task_id":"normal","model_mode":"scripted"},"code_snapshot":{"desktop_submission/manual.py":workspace.code()},"checks":[{"passed":true}],"passed":true,"isolation":{"verified":true}});
        assert!(
            validate_report(
                &report,
                RunScope {
                    chapter: 1,
                    id: "test"
                },
                &workspace
            )
            .is_ok()
        );
        for field in ["identity", "code", "isolation", "verdict"] {
            let mut forged = report.clone();
            match field {
                "identity" => forged["run_id"] = json!("another"),
                "code" => {
                    forged["code_snapshot"]["desktop_submission/manual.py"] = json!("other code")
                }
                "isolation" => forged["isolation"]["verified"] = json!(false),
                _ => forged["checks"][0]["passed"] = json!(false),
            }
            assert!(
                validate_report(
                    &forged,
                    RunScope {
                        chapter: 1,
                        id: "test"
                    },
                    &workspace
                )
                .is_err(),
                "{field}"
            );
        }
        report["checks"][0]["passed"] = json!(false);
        report["passed"] = json!(false);
        report["isolation"]["verified"] = json!(false);
        assert!(
            validate_report(
                &report,
                RunScope {
                    chapter: 1,
                    id: "test"
                },
                &workspace
            )
            .is_ok()
        );
    }

    #[test]
    fn a_submitted_pass_requires_a_normal_host_exit() {
        assert!(valid_exit(&json!({"passed":true}), Some(0)));
        assert!(!valid_exit(&json!({"passed":true}), Some(1)));
        assert!(!valid_exit(&json!({"passed":true}), Some(137)));
        assert!(!valid_exit(&json!({"passed":true}), None));
        assert!(valid_exit(&json!({"passed":false}), Some(1)));
    }

    #[test]
    fn prelaunch_cancellation_is_saved_without_starting_a_host() {
        let runtime = IoRuntime::new(2).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let service = LearningService::new(directory.path().join("records"), runtime.clone());
        let mut ids = std::collections::HashSet::new();
        for chapter in 1..=10 {
            let current = service.for_chapter(chapter).unwrap();
            let (events, _) = async_channel::bounded(32);
            let report = runtime
                .block_on(current.run(
                    LearningWorkspace::for_chapter(chapter).unwrap(),
                    Arc::new(AtomicBool::new(true)),
                    events,
                ))
                .unwrap();
            assert_eq!(report.status, "cancelled");
            assert!(!report.passed);
            assert_eq!(report.raw["request"]["chapter"], json!(chapter));
            assert_eq!(
                report.raw["request"]["course_id"],
                json!(current.course_id())
            );
            assert!(ids.insert(report.id.clone()));
            let reopened = runtime.block_on(current.snapshot()).unwrap();
            assert_eq!(reopened.history.len(), 1);
            assert_eq!(reopened.history[0].id, report.id);
            assert_eq!(reopened.history[0].status, "cancelled");
            assert_eq!(reopened.workspace.revision, 1);
        }
    }

    /// Requires `uv sync --locked` and the Windows LPAC backend. Explicit local
    /// integration gate: never relies on the user's application data directory.
    #[test]
    #[ignore = "requires the provisioned Python course environment on Windows"]
    fn desktop_learning_runs_both_implementations_and_restores_evidence() {
        let runtime = IoRuntime::new(2).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let service = LearningService::new(directory.path().join("records"), runtime.clone());
        for implementation in ["manual", "langgraph"] {
            let mut workspace = runtime.block_on(service.snapshot()).unwrap().workspace;
            workspace.implementation = implementation.into();
            workspace.manual_code = reference_code("manual").into();
            workspace.langgraph_code = reference_code("langgraph").into();
            workspace.help_level = "S".into();
            workspace.prediction = "两份资料读取后形成带来源的三个字段".into();
            let (events, _) = async_channel::bounded(64);
            let report = runtime
                .block_on(service.run(workspace, Arc::new(AtomicBool::new(false)), events))
                .unwrap();
            assert!(
                report.passed,
                "{}",
                serde_json::to_string_pretty(&report.raw).unwrap()
            );
            assert_eq!(report.metrics.model_decisions, 3);
            assert_eq!(report.metrics.actual_tool_executions, 3);
        }
        let backup = directory.path().join("backup.json");
        runtime.block_on(service.export(backup.clone())).unwrap();
        let workspace = runtime.block_on(service.snapshot()).unwrap().workspace;
        runtime.block_on(service.new_round(workspace)).unwrap();
        assert!(
            runtime
                .block_on(service.snapshot())
                .unwrap()
                .history
                .is_empty()
        );
        let restored = runtime.block_on(service.restore(backup)).unwrap();
        assert_eq!(restored.history.len(), 2);
        assert!(restored.history.iter().all(|run| run.imported));
    }

    /// Runs the real desktop host and LPAC worker for the first and last new
    /// chapters. Python owns the full chapter/scenario matrix; this gate checks
    /// the Rust request, immutable submission, report and archive boundaries.
    #[test]
    #[ignore = "requires the provisioned Python course environment on Windows"]
    fn desktop_later_chapters_preserve_identity_and_restore_only_their_own_evidence() {
        let runtime = IoRuntime::new(2).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let first = LearningService::new(directory.path().join("records"), runtime.clone());
        let first_saved = runtime
            .block_on(first.save(LearningWorkspace {
                lesson_index: 5,
                notes: "first chapter must survive later experiments".into(),
                ..Default::default()
            }))
            .unwrap();
        let mut backups = Vec::new();
        for chapter in [2, 10] {
            let service = first.for_chapter(chapter).unwrap();
            for implementation in ["manual", "langgraph"] {
                let mut workspace = runtime.block_on(service.snapshot()).unwrap().workspace;
                workspace.implementation = implementation.into();
                workspace.manual_code = reference_code_for(chapter, "manual").into();
                workspace.langgraph_code = reference_code_for(chapter, "langgraph").into();
                workspace.help_level = "S".into();
                workspace.prediction =
                    format!("chapter {chapter}: use the runtime case and cite tool observations");
                let original = workspace.clone();
                let (events, _) = async_channel::bounded(64);
                let report = runtime
                    .block_on(service.run(workspace, Arc::new(AtomicBool::new(false)), events))
                    .unwrap();
                assert!(
                    report.passed,
                    "chapter {chapter} {implementation}: {}",
                    serde_json::to_string_pretty(&report.raw).unwrap()
                );
                assert_eq!(report.raw["request"]["chapter"], json!(chapter));
                assert_eq!(
                    report.raw["request"]["course_id"],
                    json!(service.course_id())
                );
                assert_eq!(report.workspace.code(), original.code());
                assert_eq!(report.workspace.prediction, original.prediction);
                assert!(report.metrics.actual_tool_executions > 0);
                assert!(report.raw["outcome"]["answer"].is_object());
            }
            // Each desktop run receives its own host-generated case. The host
            // checks each result against that case; cross-run values may differ.
            let backup = directory
                .path()
                .join(format!("chapter-{chapter}-backup.json"));
            runtime.block_on(service.export(backup.clone())).unwrap();
            let workspace = runtime.block_on(service.snapshot()).unwrap().workspace;
            runtime.block_on(service.new_round(workspace)).unwrap();
            let restored = runtime.block_on(service.restore(backup.clone())).unwrap();
            assert_eq!(restored.history.len(), 2);
            assert!(
                restored
                    .history
                    .iter()
                    .all(|run| run.imported && run.passed)
            );
            backups.push(backup);
        }
        let second = first.for_chapter(2).unwrap();
        let before = runtime.block_on(second.snapshot()).unwrap().workspace;
        assert!(
            runtime
                .block_on(second.restore(backups[1].clone()))
                .is_err()
        );
        assert_eq!(
            runtime.block_on(second.snapshot()).unwrap().workspace,
            before
        );
        assert_eq!(
            runtime.block_on(first.snapshot()).unwrap().workspace,
            first_saved
        );
    }
}
