//! Tests for chapter history and unsaved work.

use super::*;

#[gpui_kit::test]
pub(crate) fn chapter_changes_do_not_apply_another_chapters_undo_history(cx: &mut TestAppContext) {
    let original = LearningWorkspace {
        manual_code: "original_manual".into(),
        langgraph_code: "original_library".into(),
        notes: "original_notes".into(),
        prediction: "original_prediction".into(),
        ..Default::default()
    };
    let (_directory, learning, visual) = open_learning(cx, original);
    let previous = learning.read_with(visual, |view, _| {
        std::array::from_fn::<_, 4, _>(|index| learning_input_entity_id(view, index))
    });
    for index in 0..4 {
        edit_learning_input(&learning, visual, index, &format!("edited_{index}"));
    }
    // Verify that keyboard Undo really reaches the focused GPUI editor.
    focus_learning_input(&learning, visual, 0);
    undo(&learning, visual, 0);
    assert_eq!(input_text(&learning, visual, 0), "original_manual");
    redo(&learning, visual, 0);
    assert_eq!(input_text(&learning, visual, 0), "edited_0");
    click(visual, TOOLS_BUTTON);
    click(visual, "learning-enter-chapter-2");
    wait_for_chapter_load(&learning, visual);
    for (index, previous) in previous.iter().enumerate() {
        learning.read_with(visual, |view, _| {
            assert_ne!(learning_input_entity_id(view, index), *previous);
        });
        let expected = input_text(&learning, visual, index);
        focus_learning_input(&learning, visual, index);
        undo(&learning, visual, index);
        assert_eq!(input_text(&learning, visual, index), expected);
        redo(&learning, visual, index);
        assert_eq!(input_text(&learning, visual, index), expected);
    }
}

#[gpui_kit::test]
pub(crate) fn save_preserves_undo_but_new_round_and_restore_reset_each_editors_history(
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
        std::array::from_fn::<_, 4, _>(|index| learning_input_entity_id(view, index))
    });
    for index in 0..4 {
        edit_learning_input(&learning, visual, index, &format!("saved_{index}"));
    }
    visual.update(|window, cx| learning.update(cx, |view, cx| view.save(window, cx)));
    wait_for_chapter_load(&learning, visual);
    for (index, previous) in previous.iter().enumerate() {
        learning.read_with(visual, |view, _| {
            assert_eq!(learning_input_entity_id(view, index), *previous);
        });
        focus_learning_input(&learning, visual, index);
        undo(&learning, visual, index);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("original_{index}")
        );
        redo(&learning, visual, index);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
    }
    visual.update(|window, cx| learning.update(cx, |view, cx| view.new_round(window, cx)));
    wait_for_chapter_load(&learning, visual);
    for index in 0..4 {
        focus_learning_input(&learning, visual, index);
        undo(&learning, visual, index);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
    }
    let backup = directory.path().join("undo-restore-backup.json");
    ngy_book_studio::learning_records::LearningStore::new(directory.path().join("learning"))
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
        undo(&learning, visual, index);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
        redo(&learning, visual, index);
        assert_eq!(
            input_text(&learning, visual, index),
            format!("saved_{index}")
        );
    }
}

#[gpui_kit::test]
pub(crate) fn entering_chapter_saves_previous_inputs_and_restores_separate_workspaces(
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

#[gpui_kit::test]
pub(crate) fn failed_chapter_load_opens_its_recovery_and_preserves_previous_inputs(
    cx: &mut TestAppContext,
) {
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
        ngy_book_studio::learning_records::LearningStore::new(data.clone())
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

#[gpui_kit::test]
pub(crate) fn previous_chapter_save_failure_keeps_its_unsaved_inputs(cx: &mut TestAppContext) {
    let (directory, learning, visual) = open_learning(cx, LearningWorkspace::default());
    let store =
        ngy_book_studio::learning_records::LearningStore::new(directory.path().join("learning"));
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

#[gpui_kit::test]
pub(crate) fn unloaded_corrupt_chapters_allow_leaving_and_restoring_their_own_backup(
    cx: &mut TestAppContext,
) {
    let (directory, learning, visual) = open_learning(cx, LearningWorkspace::default());
    let data = directory.path().join("learning");
    let second =
        ngy_book_studio::learning_records::LearningStore::for_chapter(data.clone(), 2).unwrap();
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
