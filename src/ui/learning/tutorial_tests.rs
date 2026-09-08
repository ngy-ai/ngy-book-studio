//! Exercise production navigation with GPUI mouse events and temporary services.
//! No provider, library, desktop worker, or native window is started by these tests.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};

const TUTORIAL_BUTTON: &str = "learning-document-../ai-agent-tutorial/README.md";
const FIRST_CHAPTER_BUTTON: &str = "learning-document-README.md";
const TOOLS_BUTTON: &str =
    "learning-document-../ai-agent-tutorial/chapters/02-tools-and-contracts.md";

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    // GPUI 0.2.2 retains debug_bounds entries across frames. Use these bounds
    // to dispatch clicks and assert the resulting workspace, never to prove
    // an old control has disappeared with debug_bounds(...).is_none().
    let bounds = cx
        .debug_bounds(selector)
        .expect("navigation control must be rendered");
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    cx.simulate_click(bounds.center(), Modifiers::none());
    redraw(cx);
}

fn open_learning(
    cx: &mut TestAppContext,
    workspace: LearningWorkspace,
) -> (
    tempfile::TempDir,
    Entity<LearningWindow>,
    &mut VisualTestContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(LearningService::new(
        directory.path().join("learning"),
        moye_epub_editor::runtime::IoRuntime::new(1).unwrap(),
    ));
    cx.update(gpui_component::init);
    let mut learning = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = LearningWindow::new(service, window, cx);
            view.apply_snapshot(
                LearningSnapshot {
                    lessons: moye_epub_editor::learning_records::lessons(),
                    workspace,
                    history: vec![],
                    environment: LearningEnvironment {
                        ready: true,
                        message: "测试快照，无外部运行".into(),
                        install_url: None,
                    },
                },
                window,
                cx,
            );
            view
        });
        learning = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1360.), px(860.)));
    redraw(visual);
    (directory, learning.unwrap(), visual)
}

fn assert_workspace(
    learning: &Entity<LearningWindow>,
    cx: &VisualTestContext,
    expected: &LearningWorkspace,
    pane: StudyPane,
) {
    learning.read_with(cx, |view, cx| {
        assert_eq!(view.study_pane, pane);
        assert_eq!(&view.entered_workspace(cx).unwrap(), expected);
    });
}

fn wait_for_chapter_load(learning: &Entity<LearningWindow>, cx: &mut VisualTestContext) {
    // Persistence runs on the real, independent IoRuntime, so the GPUI test
    // executor becoming parked alone does not mean the disk operation finished.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        redraw(cx);
        if learning.read_with(cx, |view, _| view.operation.is_none()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "chapter load did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn learning_input(view: &LearningWindow, index: usize) -> Entity<InputState> {
    match index {
        0 => view.manual_input.clone(),
        1 => view.langgraph_input.clone(),
        2 => view.notes_input.clone(),
        3 => view.prediction_input.clone(),
        _ => panic!("unknown learning input"),
    }
}

fn focus_learning_input(
    learning: &Entity<LearningWindow>,
    visual: &mut VisualTestContext,
    index: usize,
) {
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.study_pane = if index < 2 {
                StudyPane::Code
            } else {
                StudyPane::Notes
            };
            if index < 2 {
                view.implementation = if index == 0 { "manual" } else { "langgraph" }.into();
            }
            learning_input(view, index).update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        });
    });
    redraw(visual);
}

fn input_text(
    learning: &Entity<LearningWindow>,
    visual: &VisualTestContext,
    index: usize,
) -> String {
    learning.read_with(visual, |view, cx| {
        learning_input(view, index).read(cx).value().to_string()
    })
}

fn edit_learning_input(
    learning: &Entity<LearningWindow>,
    visual: &mut VisualTestContext,
    index: usize,
    text: &str,
) {
    focus_learning_input(learning, visual, index);
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    visual.simulate_input(text);
    redraw(visual);
    assert_eq!(input_text(learning, visual, index), text);
}

fn undo(visual: &mut VisualTestContext) {
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    redraw(visual);
}

fn redo(visual: &mut VisualTestContext) {
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-y"
    });
    redraw(visual);
}

#[gpui::test]
fn chapter_changes_do_not_apply_another_chapters_undo_history(cx: &mut TestAppContext) {
    let original = LearningWorkspace {
        manual_code: "original_manual".into(),
        langgraph_code: "original_library".into(),
        notes: "original_notes".into(),
        prediction: "original_prediction".into(),
        ..Default::default()
    };
    let (_directory, learning, visual) = open_learning(cx, original);
    let previous = learning.read_with(visual, |view, _| {
        std::array::from_fn::<_, 4, _>(|index| learning_input(view, index))
    });
    for index in 0..4 {
        edit_learning_input(&learning, visual, index, &format!("edited_{index}"));
    }
    // Verify that keyboard Undo really reaches the focused GPUI editor.
    focus_learning_input(&learning, visual, 0);
    undo(visual);
    assert_eq!(input_text(&learning, visual, 0), "original_manual");
    redo(visual);
    assert_eq!(input_text(&learning, visual, 0), "edited_0");
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    for (index, previous) in previous.iter().enumerate() {
        learning.read_with(visual, |view, _| {
            assert_ne!(
                learning_input(view, index).entity_id(),
                previous.entity_id()
            );
        });
        let expected = input_text(&learning, visual, index);
        focus_learning_input(&learning, visual, index);
        undo(visual);
        assert_eq!(input_text(&learning, visual, index), expected);
        redo(visual);
        assert_eq!(input_text(&learning, visual, index), expected);
    }
}

#[gpui::test]
fn save_preserves_undo_but_new_round_and_restore_reset_each_editors_history(
    cx: &mut TestAppContext,
) {
    let original = LearningWorkspace {
        manual_code: "original_0".into(),
        langgraph_code: "original_1".into(),
        notes: "original_2".into(),
        prediction: "original_3".into(),
        ..Default::default()
    };
    let (directory, learning, visual) = open_learning(cx, original);
    let previous = learning.read_with(visual, |view, _| {
        std::array::from_fn::<_, 4, _>(|index| learning_input(view, index))
    });
    for index in 0..4 {
        edit_learning_input(&learning, visual, index, &format!("saved_{index}"));
    }
    visual.update(|window, cx| learning.update(cx, |view, cx| view.save(window, cx)));
    wait_for_chapter_load(&learning, visual);
    for (index, previous) in previous.iter().enumerate() {
        learning.read_with(visual, |view, _| {
            assert_eq!(
                learning_input(view, index).entity_id(),
                previous.entity_id()
            );
        });
        focus_learning_input(&learning, visual, index);
        undo(visual);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("original_{index}")
        );
        redo(visual);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
    }
    visual.update(|window, cx| learning.update(cx, |view, cx| view.new_round(window, cx)));
    wait_for_chapter_load(&learning, visual);
    for index in 0..4 {
        focus_learning_input(&learning, visual, index);
        undo(visual);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
    }
    let backup = directory.path().join("undo-restore-backup.json");
    moye_epub_editor::learning_records::LearningStore::new(directory.path().join("learning"))
        .export(&backup)
        .unwrap();
    for index in 0..4 {
        edit_learning_input(&learning, visual, index, &format!("discarded_{index}"));
    }
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            let service = Arc::clone(&view.service);
            let workspace = view.entered_workspace(cx).unwrap();
            view.operation = Some(Operation::Restoring);
            cx.spawn_in(window, async move |view, cx| {
                let saved = service.save(workspace).await.unwrap();
                let outcome = service.restore(backup).await.map(Some);
                let _ = view.update_in(cx, |view, window, cx| {
                    view.finish_restore(Some(saved), outcome, true, window, cx);
                });
            })
            .detach();
        });
    });
    wait_for_chapter_load(&learning, visual);
    for index in 0..4 {
        focus_learning_input(&learning, visual, index);
        undo(visual);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
        redo(visual);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
    }
}

#[gpui::test]
fn entering_chapter_saves_previous_inputs_and_restores_separate_workspaces(
    cx: &mut TestAppContext,
) {
    let original = LearningWorkspace::default();
    let (directory, learning, visual) = open_learning(cx, original);
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.manual_input.update(cx, |input, cx| {
                input.set_value("# first chapter code", window, cx)
            });
            view.notes_input.update(cx, |input, cx| {
                input.set_value("第一章未保存笔记", window, cx)
            });
            view.selected_lesson = 4;
            view.help_level = "H2".into();
        });
    });
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 2);
        assert_eq!(view.study_pane, StudyPane::Lesson);
        assert_eq!(view.selected_lesson, 0);
        assert_eq!(view.lessons.len(), 1);
        assert!(view.notes_input.read(cx).value().is_empty());
        assert_eq!(view.help_level, "H0");
        assert!(view.report.is_none() && view.trace.is_empty() && view.history.is_empty());
    });
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.manual_input.update(cx, |input, cx| {
                input.set_value("# chapter two code", window, cx)
            });
            view.prediction_input.update(cx, |input, cx| {
                input.set_value("第二章首次预测", window, cx)
            });
            view.scenario = "fault".into();
            view.implementation = "langgraph".into();
            view.help_level = "S".into();
        });
    });
    click(visual, FIRST_CHAPTER_BUTTON);
    click(visual, "learning-start-first-chapter");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 1);
        assert_eq!(view.selected_lesson, 4);
        assert_eq!(
            view.manual_input.read(cx).value().as_ref(),
            "# first chapter code"
        );
        assert_eq!(
            view.notes_input.read(cx).value().as_ref(),
            "第一章未保存笔记"
        );
        assert_eq!(view.help_level, "H2");
    });
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 2);
        assert_eq!(
            view.manual_input.read(cx).value().as_ref(),
            "# chapter two code"
        );
        assert_eq!(
            view.prediction_input.read(cx).value().as_ref(),
            "第二章首次预测"
        );
        assert_eq!(view.scenario, "fault");
        assert_eq!(view.implementation, "langgraph");
        assert_eq!(view.help_level, "S");
    });
    assert!(directory.path().join("learning/chapter-01.json").exists());
    assert!(directory.path().join("learning/chapter-02.json").exists());
}

#[gpui::test]
fn failed_chapter_load_opens_its_recovery_and_preserves_previous_inputs(cx: &mut TestAppContext) {
    let (directory, learning, visual) = open_learning(cx, LearningWorkspace::default());
    let data = directory.path().join("learning");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("chapter-02.json"), b"corrupt target chapter").unwrap();
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.notes_input.update(cx, |input, cx| {
                input.set_value("切换失败也保留我", window, cx)
            });
        });
    });
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 2);
        assert!(view.saved_workspace.is_none());
        assert!(view.notes_input.read(cx).value().is_empty());
        assert!(view.manual_input.read(cx).value().is_empty());
        assert!(view.notice.as_ref().unwrap().error);
        assert!(view.notice.as_ref().unwrap().text.contains("恢复本章备份"));
        assert!(view.inputs_disabled());
        assert!(!view.chapter_entry_disabled());
    });
    assert_eq!(
        moye_epub_editor::learning_records::LearningStore::new(data.clone())
            .workspace()
            .unwrap()
            .notes,
        "切换失败也保留我"
    );
    click(visual, FIRST_CHAPTER_BUTTON);
    click(visual, "learning-start-first-chapter");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 1);
        assert_eq!(
            view.notes_input.read(cx).value().as_ref(),
            "切换失败也保留我"
        );
    });
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.running = true;
            view.enter_chapter(3, window, cx);
            assert_eq!(view.service.chapter(), 1);
            assert!(view.operation.is_none());
            view.running = false;
            view.operation = Some(Operation::Saving);
            view.enter_chapter(3, window, cx);
            assert_eq!(view.service.chapter(), 1);
            view.operation = None;
            view.reload_required = true;
            view.enter_chapter(3, window, cx);
            assert_eq!(view.service.chapter(), 1);
            assert!(view.operation.is_none());
            assert_eq!(
                view.notes_input.read(cx).value().as_ref(),
                "切换失败也保留我"
            );
            view.reload_required = false;
        });
    });
    assert!(!data.join("chapter-03.json").exists());
    assert_eq!(
        std::fs::read(data.join("chapter-02.json")).unwrap(),
        b"corrupt target chapter"
    );
}

#[gpui::test]
fn previous_chapter_save_failure_keeps_its_unsaved_inputs(cx: &mut TestAppContext) {
    let (directory, learning, visual) = open_learning(cx, LearningWorkspace::default());
    let store =
        moye_epub_editor::learning_records::LearningStore::new(directory.path().join("learning"));
    // Simulate an independently advanced revision; switching must not conceal
    // the original editor or bind it to the target chapter after save fails.
    store.save(LearningWorkspace::default()).unwrap();
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.notes_input.update(cx, |input, cx| {
                input.set_value("尚未保存的原章作答", window, cx)
            });
        });
    });
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 1);
        assert_eq!(
            view.notes_input.read(cx).value().as_ref(),
            "尚未保存的原章作答"
        );
        assert!(view.saved_workspace.is_some());
        assert!(view.notice.as_ref().unwrap().text.contains("保存失败"));
    });
    assert!(!directory.path().join("learning/chapter-02.json").exists());
}

#[gpui::test]
fn unloaded_corrupt_chapters_allow_leaving_and_restoring_their_own_backup(cx: &mut TestAppContext) {
    let (directory, learning, visual) = open_learning(cx, LearningWorkspace::default());
    let data = directory.path().join("learning");
    let second =
        moye_epub_editor::learning_records::LearningStore::for_chapter(data.clone(), 2).unwrap();
    second
        .save(LearningWorkspace {
            notes: "第二章备份里的作答".into(),
            ..LearningWorkspace::for_chapter(2).unwrap()
        })
        .unwrap();
    let backup = directory.path().join("chapter-two-backup.json");
    second.export(&backup).unwrap();
    std::fs::write(data.join("chapter-01.json"), b"corrupt first chapter").unwrap();
    std::fs::write(data.join("chapter-02.json"), b"corrupt second chapter").unwrap();
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.clear_unloaded_chapter(window, cx);
            view.load(window, cx);
        });
    });
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, _| {
        assert_eq!(view.service.chapter(), 1);
        assert!(view.saved_workspace.is_none());
        assert!(view.notice.as_ref().unwrap().error);
    });
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, _| {
        assert_eq!(view.service.chapter(), 2);
        assert!(view.saved_workspace.is_none());
        assert!(!view.chapter_entry_disabled());
    });
    // Bypass only the native picker. Use the visible chapter's actual service
    // and the production restore completion path against temporary files.
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            let service = Arc::clone(&view.service);
            view.operation = Some(Operation::Restoring);
            cx.spawn_in(window, async move |view, cx| {
                let result = service.restore(backup).await.map(Some);
                let _ = view.update_in(cx, |view, window, cx| {
                    view.finish_restore(None, result, true, window, cx);
                });
            })
            .detach();
        });
    });
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.service.chapter(), 2);
        assert_eq!(
            view.notes_input.read(cx).value().as_ref(),
            "第二章备份里的作答"
        );
        assert!(!view.inputs_disabled());
        assert!(!view.notice.as_ref().unwrap().error);
    });
    assert_eq!(
        std::fs::read(data.join("chapter-01.json")).unwrap(),
        b"corrupt first chapter"
    );
    assert!(
        std::fs::read_dir(data.join("archives"))
            .unwrap()
            .any(|entry| {
                std::fs::read(entry.unwrap().path()).unwrap() == b"corrupt second chapter"
            })
    );
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| view.enter_chapter(3, window, cx));
    });
    wait_for_chapter_load(&learning, visual);
    learning.read_with(visual, |view, _| {
        assert_eq!(view.service.chapter(), 3);
        assert!(!view.inputs_disabled());
    });
}

#[gpui::test]
fn chapter_buttons_preserve_unsaved_work_and_resume_the_sixth_lesson(cx: &mut TestAppContext) {
    let original = LearningWorkspace::default();
    let (directory, learning, visual) = open_learning(cx, original.clone());
    assert_workspace(
        &learning,
        visual,
        &original,
        StudyPane::Document(CourseDocument::Tutorial),
    );
    click(visual, "learning-start-first-chapter");
    assert_workspace(&learning, visual, &original, StudyPane::Lesson);

    // Change the live inputs after loading the snapshot: comparing only the
    // saved fixture would miss navigation that restores stale saved contents.
    let mut edited = LearningWorkspace {
        manual_code: "# my unfinished manual agent\n".into(),
        langgraph_code: "# my unfinished graph\n".into(),
        notes: "保留我的第一次判断".into(),
        prediction: "工具失败后应先看能否重试".into(),
        ..original
    };
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            for (input, value) in [
                (&view.manual_input, &edited.manual_code),
                (&view.langgraph_input, &edited.langgraph_code),
                (&view.notes_input, &edited.notes),
                (&view.prediction_input, &edited.prediction),
            ] {
                input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
            }
            cx.notify();
        });
    });
    redraw(visual);
    click(visual, TUTORIAL_BUTTON);
    assert!(
        visual
            .debug_bounds("learning-continue-first-chapter")
            .is_some()
    );
    click(visual, "learning-continue-first-chapter");
    assert_workspace(&learning, visual, &edited, StudyPane::Lesson);

    for index in 1..6 {
        click(visual, "learning-next-lesson");
        edited.lesson_index = index;
        assert_workspace(&learning, visual, &edited, StudyPane::Lesson);
    }
    click(visual, "learning-previous-lesson");
    edited.lesson_index = 4;
    assert_workspace(&learning, visual, &edited, StudyPane::Lesson);
    click(visual, "learning-next-lesson");
    edited.lesson_index = 5;
    click(visual, "learning-next-lesson");
    assert_workspace(
        &learning,
        visual,
        &edited,
        StudyPane::Document(CourseDocument::Tools),
    );

    // Use the actual chapter buttons to traverse every chapter boundary.
    for chapter in CourseDocument::CHAPTERS.into_iter().skip(2) {
        click(visual, "learning-next-chapter");
        assert_workspace(&learning, visual, &edited, StudyPane::Document(chapter));
    }
    for chapter in CourseDocument::CHAPTERS.into_iter().take(9).rev() {
        click(visual, "learning-previous-chapter");
        assert_workspace(&learning, visual, &edited, StudyPane::Document(chapter));
    }
    click(visual, "learning-back-to-guide");
    assert_workspace(
        &learning,
        visual,
        &edited,
        StudyPane::Document(CourseDocument::Tutorial),
    );
    click(visual, "learning-continue-first-chapter");
    assert_workspace(&learning, visual, &edited, StudyPane::Lesson);
    assert!(
        !directory.path().join("learning").exists(),
        "browsing must not save a record"
    );
}

#[gpui::test]
fn active_run_blocks_document_navigation_and_keeps_cancel_clickable(cx: &mut TestAppContext) {
    let workspace = LearningWorkspace {
        revision: 7,
        lesson_index: 5,
        ..Default::default()
    };
    let (directory, learning, visual) = open_learning(cx, workspace.clone());
    click(visual, "learning-continue-first-chapter");
    let cancellation = Arc::new(AtomicBool::new(false));
    learning.update(visual, |view, cx| {
        // Model the UI state of an active controlled run without starting one.
        view.running = true;
        view.cancellation = Some(Arc::clone(&cancellation));
        cx.notify();
    });
    redraw(visual);
    for selector in [
        TUTORIAL_BUTTON,
        TOOLS_BUTTON,
        "learning-previous-lesson",
        "learning-next-lesson",
    ] {
        click(visual, selector);
        assert_workspace(&learning, visual, &workspace, StudyPane::Lesson);
        assert!(visual.debug_bounds("learning-cancel").is_some());
        assert!(!cancellation.load(Ordering::Acquire));
    }
    click(visual, "learning-cancel");
    assert!(
        cancellation.load(Ordering::Acquire),
        "the visible control must dispatch cancellation"
    );
    learning.update(visual, |view, cx| {
        view.closing = true;
        cx.notify();
    });
    redraw(visual);
    for selector in [TUTORIAL_BUTTON, TOOLS_BUTTON, "learning-next-lesson"] {
        click(visual, selector);
        assert_workspace(&learning, visual, &workspace, StudyPane::Lesson);
        assert!(visual.debug_bounds("learning-cancel").is_some());
    }
    assert!(!directory.path().join("learning").exists());
}

#[gpui::test]
fn closing_document_disables_chapter_and_continue_buttons(cx: &mut TestAppContext) {
    let workspace = LearningWorkspace {
        revision: 2,
        lesson_index: 5,
        ..Default::default()
    };
    let (_directory, learning, visual) = open_learning(cx, workspace.clone());
    click(visual, FIRST_CHAPTER_BUTTON);
    click(visual, "learning-next-chapter");
    assert_workspace(
        &learning,
        visual,
        &workspace,
        StudyPane::Document(CourseDocument::Tools),
    );
    learning.update(visual, |view, cx| {
        view.closing = true;
        cx.notify();
    });
    redraw(visual);
    for selector in [
        "learning-previous-chapter",
        "learning-next-chapter",
        "learning-back-to-guide",
        TUTORIAL_BUTTON,
    ] {
        click(visual, selector);
        assert_workspace(
            &learning,
            visual,
            &workspace,
            StudyPane::Document(CourseDocument::Tools),
        );
    }
    learning.update(visual, |view, cx| {
        view.closing = false;
        cx.notify();
    });
    redraw(visual);
    click(visual, "learning-previous-chapter");
    learning.update(visual, |view, cx| {
        view.closing = true;
        cx.notify();
    });
    redraw(visual);
    click(visual, "learning-continue-first-chapter");
    assert_workspace(
        &learning,
        visual,
        &workspace,
        StudyPane::Document(CourseDocument::Overview),
    );
}

#[gpui::test]
fn restore_dialog_error_and_cancel_keep_unsaved_inputs_and_controls_usable(
    cx: &mut TestAppContext,
) {
    let original = LearningWorkspace::default();
    let (directory, learning, visual) = open_learning(cx, original.clone());
    let edited = LearningWorkspace {
        manual_code: "# keep this unsaved implementation\n".into(),
        notes: "文件选择失败也不能丢失当前判断".into(),
        ..original.clone()
    };
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.manual_input.update(cx, |input, cx| {
                input.set_value(edited.manual_code.clone(), window, cx);
            });
            view.notes_input.update(cx, |input, cx| {
                input.set_value(edited.notes.clone(), window, cx);
            });
            view.operation = Some(Operation::Restoring);
            // Exercise the production completion path without showing a native
            // picker. It failed before either save or restore was attempted.
            view.finish_restore(
                None,
                Err(anyhow::anyhow!(
                    "无法打开恢复对话框：SHCreateItemFromParsingName"
                )),
                false,
                window,
                cx,
            );
        });
    });
    redraw(visual);
    assert_workspace(
        &learning,
        visual,
        &edited,
        StudyPane::Document(CourseDocument::Tutorial),
    );
    learning.read_with(visual, |view, cx| {
        assert_eq!(view.saved_workspace.as_ref(), Some(&original));
        assert!(view.operation.is_none() && !view.reload_required && !view.inputs_disabled());
        assert!(view.dirty(cx));
        assert!(view.notice.as_ref().unwrap().error);
    });
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.operation = Some(Operation::Restoring);
            view.finish_restore(None, Ok(None), false, window, cx);
        });
    });
    redraw(visual);
    assert_workspace(
        &learning,
        visual,
        &edited,
        StudyPane::Document(CourseDocument::Tutorial),
    );
    learning.read_with(visual, |view, _| {
        assert!(view.operation.is_none() && !view.reload_required && !view.inputs_disabled());
    });
    click(visual, "learning-continue-first-chapter");
    assert_workspace(&learning, visual, &edited, StudyPane::Lesson);
    assert!(
        !directory.path().join("learning").exists(),
        "dialog outcomes must not write records"
    );
}

#[gpui::test]
fn restore_record_failure_still_requires_reload_and_dialog_error_does_not_clear_it(
    cx: &mut TestAppContext,
) {
    let original = LearningWorkspace::default();
    let (_directory, learning, visual) = open_learning(cx, original.clone());
    visual.update(|window, cx| {
        learning.update(cx, |view, cx| {
            view.operation = Some(Operation::Restoring);
            view.finish_restore(
                None,
                Err(anyhow::anyhow!("restore failed after save")),
                true,
                window,
                cx,
            );
            assert!(view.reload_required && view.inputs_disabled());
            view.operation = Some(Operation::Restoring);
            view.finish_restore(
                None,
                Err(anyhow::anyhow!("picker unavailable")),
                false,
                window,
                cx,
            );
            assert!(view.reload_required && view.inputs_disabled());
        });
    });
    assert_workspace(
        &learning,
        visual,
        &original,
        StudyPane::Document(CourseDocument::Tutorial),
    );
}
