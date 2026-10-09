//! Exercise production navigation with GPUI mouse events and temporary services.
//! No provider, library, desktop worker, or native window is started by these tests.

use super::*;
use gpui_kit::component::input::AnyInputState;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

pub(crate) const TUTORIAL_BUTTON: &str = "learning-document-../ai-agent-tutorial/README.md";
pub(crate) const FIRST_CHAPTER_BUTTON: &str = "learning-document-README.md";
pub(crate) const TOOLS_BUTTON: &str =
    "learning-document-../ai-agent-tutorial/chapters/02-tools-and-contracts.md";

pub(crate) fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
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
    init_ui_test(cx);
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
    // The bound is generous because the whole binary test suite runs in
    // parallel and that shared runtime can be busy; a load that never finishes
    // still fails.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
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

/// 学习窗口的四个输入在 0.6 里是两种状态类型；测试用 `AnyInputState`
/// 统一引用它们，再按具体变体调用 `set_value`/`focus`。
pub(crate) fn learning_input(view: &LearningWindow, index: usize) -> AnyInputState {
    match index {
        0 => AnyInputState::Editor(view.manual_input.clone()),
        1 => AnyInputState::Editor(view.langgraph_input.clone()),
        2 => AnyInputState::Textarea(view.notes_input.clone()),
        3 => AnyInputState::Textarea(view.prediction_input.clone()),
        _ => panic!("unknown learning input"),
    }
}

pub(crate) fn learning_input_entity_id(view: &LearningWindow, index: usize) -> gpui_kit::EntityId {
    match learning_input(view, index) {
        AnyInputState::Input(state) => state.entity_id(),
        AnyInputState::Textarea(state) => state.entity_id(),
        AnyInputState::Editor(state) => state.entity_id(),
        AnyInputState::Otp(state) => state.entity_id(),
    }
}

pub(crate) fn set_learning_input(
    view: &LearningWindow,
    index: usize,
    value: String,
    window: &mut Window,
    cx: &mut Context<LearningWindow>,
) {
    match learning_input(view, index) {
        AnyInputState::Input(state) => {
            state.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        AnyInputState::Textarea(state) => {
            state.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        AnyInputState::Editor(state) => {
            state.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        AnyInputState::Otp(_) => panic!("learning inputs are never OTP fields"),
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
            match learning_input(view, index) {
                AnyInputState::Input(state) => {
                    state.update(cx, |input, cx| input.focus(window, cx));
                }
                AnyInputState::Textarea(state) => {
                    state.update(cx, |input, cx| input.focus(window, cx));
                }
                AnyInputState::Editor(state) => {
                    state.update(cx, |input, cx| input.focus(window, cx));
                }
                AnyInputState::Otp(_) => panic!("learning inputs are never OTP fields"),
            }
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
        learning_input(view, index).value(cx).to_string()
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

/// 撤销/重做最多按多少次。0.6 的编辑器按输入批次记录历史，逐字符输入会产生
/// 多条记录；正常用例在近十步内耗尽，这个上限只保护死循环。
const MAX_HISTORY_STEPS: usize = 64;

/// 连按 `keys` 直到输入文本不再变化：历史耗尽后继续按不再改变文本，所以
/// 「按到稳定」等于「撤销/重做到这次编辑的尽头」，调用点可以继续断言最终
/// 文本而不是某一步的中间态。`index` 是当前聚焦的输入。
fn press_until_stable(
    learning: &Entity<LearningWindow>,
    visual: &mut VisualTestContext,
    keys: &'static str,
    index: usize,
) {
    let mut previous = input_text(learning, visual, index);
    for _ in 0..MAX_HISTORY_STEPS {
        visual.simulate_keystrokes(keys);
        redraw(visual);
        let current = input_text(learning, visual, index);
        if current == previous {
            return;
        }
        previous = current;
    }
    panic!("editor history did not settle within {MAX_HISTORY_STEPS} steps");
}

pub(crate) fn undo(
    learning: &Entity<LearningWindow>,
    visual: &mut VisualTestContext,
    index: usize,
) {
    press_until_stable(
        learning,
        visual,
        if cfg!(target_os = "macos") {
            "cmd-z"
        } else {
            "ctrl-z"
        },
        index,
    );
}

pub(crate) fn redo(
    learning: &Entity<LearningWindow>,
    visual: &mut VisualTestContext,
    index: usize,
) {
    press_until_stable(
        learning,
        visual,
        if cfg!(target_os = "macos") {
            "cmd-shift-z"
        } else {
            "ctrl-y"
        },
        index,
    );
}

mod history_test;

mod navigation_test;
