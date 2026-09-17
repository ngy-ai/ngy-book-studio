//! Deciding whether a host answer may be trusted, and the fallback report
//! that replaces one that may not.

use super::*;

pub(crate) fn valid_exit(report: &Value, code: Option<i32>) -> bool {
    code == Some(
        if report.get("passed").and_then(Value::as_bool) == Some(true) {
            0
        } else {
            1
        },
    )
}

pub(crate) fn validate_report(
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

pub(crate) fn cleanup_profile(root: &Path, directory: &Path) -> Result<()> {
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

pub(crate) fn failed_report(
    scope: RunScope<'_>,
    workspace: &LearningWorkspace,
    status: &str,
    reason: &str,
    detail: &str,
    elapsed: Duration,
) -> Value {
    json!({"report_version":1,"run_id":scope.id,"request":{"chapter":scope.chapter,"course_id":chapter_course_id(scope.chapter),"course_version":crate::learning_records::COURSE_VERSION,"rules_version":"1.0.0","task_version":"1.0.0","implementation":workspace.implementation,"task_id":workspace.scenario,"model_mode":"scripted"},"outcome":{"status":status,"stop_reason":reason,"answer":null},"passed":false,"metrics":{"elapsed_seconds":elapsed.as_secs_f64()},"observations":{},"error":{"message":detail},"checks":[{"id":"host_completion","passed":false,"detail":"运行未形成完整的宿主检查结果"}],"learning":{"status":"pending_review"}})
}

pub(crate) fn probe_environment(root: &std::path::Path) -> LearningEnvironment {
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
