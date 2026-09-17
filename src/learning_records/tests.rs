//! Tests for the versioned chapter records.

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
