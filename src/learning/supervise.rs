//! Reading the host protocol: deadlines, cancellation and the pipe events.

use super::*;

pub(crate) fn supervise(
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
pub(crate) struct SupervisorTiming {
    pub(crate) host_deadline: Duration,
    pub(crate) cancel_grace: Duration,
}

pub(crate) fn supervise_command(
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
