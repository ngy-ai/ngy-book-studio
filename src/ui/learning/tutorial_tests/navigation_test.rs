//! Tests for navigation, runs and restore failures.

use super::*;

#[gpui_kit::test]
pub(crate) fn chapter_buttons_preserve_unsaved_work_and_resume_the_sixth_lesson(
    cx: &mut TestAppContext,
) {
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
            set_learning_input(view, 0, edited.manual_code.clone(), window, cx);
            set_learning_input(view, 1, edited.langgraph_code.clone(), window, cx);
            set_learning_input(view, 2, edited.notes.clone(), window, cx);
            set_learning_input(view, 3, edited.prediction.clone(), window, cx);
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

#[gpui_kit::test]
pub(crate) fn active_run_blocks_document_navigation_and_keeps_cancel_clickable(
    cx: &mut TestAppContext,
) {
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

#[gpui_kit::test]
pub(crate) fn closing_document_disables_chapter_and_continue_buttons(cx: &mut TestAppContext) {
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

#[gpui_kit::test]
pub(crate) fn restore_dialog_error_and_cancel_keep_unsaved_inputs_and_controls_usable(
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

#[gpui_kit::test]
pub(crate) fn restore_record_failure_still_requires_reload_and_dialog_error_does_not_clear_it(
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
