//! Tests for the learning service, the host protocol and its supervisor.

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
            "course" => forged["request"]["course_id"] = json!(crate::learning_records::COURSE_ID),
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
            "code" => forged["code_snapshot"]["desktop_submission/manual.py"] = json!("other code"),
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
