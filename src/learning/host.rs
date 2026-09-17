//! Launching the course host process and the two pipes it talks over.

use super::*;

pub(crate) const MAX_PROTOCOL_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) const HOST_DEADLINE: Duration = Duration::from_secs(185);

pub(crate) const CANCEL_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy)]
pub(crate) struct RunScope<'a> {
    pub(crate) chapter: u8,
    pub(crate) id: &'a str,
}

pub(crate) struct ChildGuard(pub(crate) Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn host_command(root: &Path) -> Command {
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
pub(crate) fn execute(
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

pub(crate) enum PipeEvent {
    Line(Vec<u8>),
    End,
    Error(String),
}
