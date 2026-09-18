//! Exercise production navigation with GPUI mouse events and temporary services.
//! No provider, library, desktop worker, or native window is started by these tests.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};

pub(crate) const TUTORIAL_BUTTON: &str = "learning-document-../ai-agent-tutorial/README.md";
pub(crate) const FIRST_CHAPTER_BUTTON: &str = "learning-document-README.md";
pub(crate) const TOOLS_BUTTON: &str =
    "learning-document-../ai-agent-tutorial/chapters/02-tools-and-contracts.md";

pub(crate) fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

pub(crate) fn click(cx: &mut VisualTestContext, selector: &'static str) {
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

pub(crate) fn open_learning(
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
        ngy_book_studio::runtime::IoRuntime::new(1).unwrap(),
    ));
    cx.update(gpui_component::init);
    let mut learning = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = LearningWindow::new(service, window, cx);
            view.apply_snapshot(
                LearningSnapshot {
                    lessons: ngy_book_studio::learning_records::lessons(),
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

pub(crate) fn assert_workspace(
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

pub(crate) fn wait_for_chapter_load(learning: &Entity<LearningWindow>, cx: &mut VisualTestContext) {
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

pub(crate) fn learning_input(view: &LearningWindow, index: usize) -> Entity<InputState> {
    match index {
        0 => view.manual_input.clone(),
        1 => view.langgraph_input.clone(),
        2 => view.notes_input.clone(),
        3 => view.prediction_input.clone(),
        _ => panic!("unknown learning input"),
    }
}

pub(crate) fn focus_learning_input(
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

pub(crate) fn input_text(
    learning: &Entity<LearningWindow>,
    visual: &VisualTestContext,
    index: usize,
) -> String {
    learning.read_with(visual, |view, cx| {
        learning_input(view, index).read(cx).value().to_string()
    })
}

pub(crate) fn edit_learning_input(
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

pub(crate) fn undo(visual: &mut VisualTestContext) {
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    redraw(visual);
}

pub(crate) fn redo(visual: &mut VisualTestContext) {
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-y"
    });
    redraw(visual);
}

mod history_test;

mod navigation_test;
