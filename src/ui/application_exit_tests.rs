//! Exercise exit prompts and multi-window close barriers on GPUI's test
//! platform. Child completion removes synthetic windows directly; the fake
//! main close callback only records its invocation. Native HWND/WebView
//! teardown and Esc/titlebar cancellation still require Windows GUI testing.

use super::*;
use gpui::{TestAppContext, VisualTestContext};
use std::cell::RefCell;

type CloseEvents = Rc<RefCell<Vec<AnyWindowHandle>>>;

struct TrackedWindow;

impl Render for TrackedWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn main_window(cx: &mut TestAppContext, closed: &CloseEvents) -> AnyWindowHandle {
    let closed = Rc::clone(closed);
    let close: WindowCloseHandler = Rc::new(move |window, _| {
        closed.borrow_mut().push(Window::window_handle(window));
    });
    cx.add_window(move |window, cx| {
        window.on_window_should_close(cx, move |window, cx| {
            request_application_exit(window, cx, Rc::clone(&close));
            false
        });
        TrackedWindow
    })
    .into()
}

fn waiting_child(cx: &mut TestAppContext, closed: &CloseEvents) -> AnyWindowHandle {
    let closed = Rc::clone(closed);
    cx.add_window(move |window, cx| {
        on_window_close(window, cx, move |window, _| {
            closed.borrow_mut().push(Window::window_handle(window));
            // Represents a window still saving progress/drafts or releasing
            // its WebView. Completion is explicitly released by each test.
            false
        });
        TrackedWindow
    })
    .into()
}

fn request_close(cx: &mut TestAppContext, main: AnyWindowHandle) {
    let mut visual = VisualTestContext::from_window(main, cx);
    assert!(!visual.simulate_close());
    visual.run_until_parked();
}

fn answer(cx: &mut TestAppContext, response: &str) {
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer(response);
    cx.run_until_parked();
}

fn complete_child(cx: &mut TestAppContext, child: AnyWindowHandle) {
    cx.update_window(child, |_, window, _| window.remove_window())
        .expect("等待关闭的测试窗口仍然存在");
    cx.run_until_parked();
}

fn assert_idle(cx: &TestAppContext) {
    cx.read(|cx| {
        let lifecycle = cx.global::<ApplicationWindowLifecycle>();
        assert!(!lifecycle.prompt_open);
        assert!(lifecycle.exit.is_none());
    });
}

#[gpui::test]
fn continue_running_preserves_all_windows_and_repeated_closes_share_one_prompt(
    cx: &mut TestAppContext,
) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    waiting_child(cx, &closed);
    waiting_child(cx, &closed);
    waiting_child(cx, &closed);
    let open = cx.windows();

    request_close(cx, main);
    request_close(cx, main);
    request_close(cx, main);
    assert!(cx.windows() == open);
    assert!(closed.borrow().is_empty());

    answer(cx, "继续运行");
    // Answering once must consume the only prompt, including when Windows
    // sends several titlebar close messages while that prompt is open.
    assert!(!cx.has_pending_prompt());
    assert!(cx.windows() == open);
    assert!(closed.borrow().is_empty());
    assert_idle(cx);

    request_close(cx, main);
    answer(cx, "继续运行");
    assert!(!cx.has_pending_prompt());
    assert!(cx.windows() == open);
    assert_idle(cx);
}

#[gpui::test]
fn dropped_prompt_response_preserves_windows_and_allows_another_exit_attempt(
    cx: &mut TestAppContext,
) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    waiting_child(cx, &closed);
    let open = cx.windows();
    cx.update(|cx| cx.set_prompt_builder(gpui::fallback_prompt_renderer));
    request_close(cx, main);

    // Replacing a rendered prompt drops its response sender. This exercises
    // the same canceled Receiver result as a platform dialog that disappears
    // without reporting a selected button; no native dialog is constructed.
    let _replacement_response = cx
        .update_window(main, |_, window, cx| {
            window.prompt(
                gpui::PromptLevel::Info,
                "测试替换提示",
                None,
                &[gpui::PromptButton::cancel("关闭测试提示")],
                cx,
            )
        })
        .unwrap();
    cx.run_until_parked();
    assert_idle(cx);
    assert!(cx.windows() == open);
    assert!(closed.borrow().is_empty());

    cx.update(App::reset_prompt_builder);
    request_close(cx, main);
    answer(cx, "继续运行");
    assert_idle(cx);
    assert!(cx.windows() == open);
}

#[gpui::test]
fn confirmed_exit_waits_for_each_child_before_requesting_main_close_once(cx: &mut TestAppContext) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    let first = waiting_child(cx, &closed);
    let second = waiting_child(cx, &closed);
    request_close(cx, main);
    answer(cx, "退出软件");

    assert_eq!(closed.borrow().len(), 1);
    let requested_first = closed.borrow()[0];
    assert!([first, second].contains(&requested_first));
    assert_eq!(cx.windows().len(), 3);
    request_close(cx, main);
    cx.update(advance_application_exit);
    assert!(!cx.has_pending_prompt());
    assert!(*closed.borrow() == vec![requested_first]);

    complete_child(cx, requested_first);
    assert_eq!(closed.borrow().len(), 2);
    let requested_second = closed.borrow()[1];
    assert!([first, second].contains(&requested_second));
    assert!(requested_first != requested_second);
    assert_eq!(cx.windows().len(), 2);
    assert!(!closed.borrow().contains(&main));

    complete_child(cx, requested_second);
    assert!(*closed.borrow() == vec![requested_first, requested_second, main]);
    assert!(cx.windows() == vec![main]);
    // Main persistence can also be asynchronous. Further close requests and
    // notifications must not restart it while the first invocation is pending.
    request_close(cx, main);
    cx.update(advance_application_exit);
    cx.update(advance_application_exit);
    assert!(*closed.borrow() == vec![requested_first, requested_second, main]);
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn a_child_opened_while_exit_waits_is_closed_before_the_main_window(cx: &mut TestAppContext) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    let original = waiting_child(cx, &closed);
    request_close(cx, main);
    answer(cx, "退出软件");
    assert!(*closed.borrow() == vec![original]);

    let late = waiting_child(cx, &closed);
    cx.run_until_parked();
    assert!(*closed.borrow() == vec![original]);
    complete_child(cx, original);
    assert!(*closed.borrow() == vec![original, late]);
    assert!(cx.windows().contains(&main));
    complete_child(cx, late);
    assert!(*closed.borrow() == vec![original, late, main]);
}

#[gpui::test]
fn main_persistence_completion_waits_for_a_child_opened_during_its_save(cx: &mut TestAppContext) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    request_close(cx, main);
    answer(cx, "退出软件");
    assert!(*closed.borrow() == vec![main]);

    // A previously accepted open finishes while the main window is saving.
    let late = waiting_child(cx, &closed);
    cx.run_until_parked();
    assert!(*closed.borrow() == vec![main, late]);
    cx.update_window(main, |_, window, cx| {
        finish_library_window_close(window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let exit = cx
            .global::<ApplicationWindowLifecycle>()
            .exit
            .as_ref()
            .unwrap();
        assert!(exit.main_ready);
        assert!(exit.waiting_for == Some(late));
        assert!(!exit.removal_scheduled);
    });
    assert!(cx.windows().contains(&main));
    assert!(cx.windows().contains(&late));
    assert!(*closed.borrow() == vec![main, late]);

    // Leave the native removal to GUI acceptance and release the synthetic
    // child only after canceling this test's pending exit attempt.
    cx.update(cancel_application_exit);
    complete_child(cx, late);
    assert!(cx.windows() == vec![main]);
    assert_idle(cx);
}

#[gpui::test]
fn child_veto_cancels_the_exit_attempt_and_keeps_the_main_window_open(cx: &mut TestAppContext) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    let veto_events = Rc::clone(&closed);
    let child: AnyWindowHandle = cx
        .add_window(move |window, cx| {
            on_window_close(window, cx, move |window, cx| {
                veto_events.borrow_mut().push(Window::window_handle(window));
                cancel_application_exit(cx);
                false
            });
            TrackedWindow
        })
        .into();
    let open = cx.windows();
    request_close(cx, main);
    answer(cx, "退出软件");
    assert!(*closed.borrow() == vec![child]);
    assert!(cx.windows() == open);
    assert_idle(cx);

    request_close(cx, main);
    answer(cx, "继续运行");
    assert!(*closed.borrow() == vec![child]);
    assert!(cx.windows() == open);
    assert_idle(cx);
}

#[gpui::test]
fn canceled_exit_ignores_late_child_completion_and_can_be_confirmed_again(cx: &mut TestAppContext) {
    let closed = CloseEvents::default();
    let main = main_window(cx, &closed);
    let child = waiting_child(cx, &closed);
    request_close(cx, main);
    answer(cx, "退出软件");
    assert!(*closed.borrow() == vec![child]);

    // A persistence failure or a later editor cancel abandons the whole exit.
    cx.update(cancel_application_exit);
    complete_child(cx, child);
    cx.update(advance_application_exit);
    assert!(*closed.borrow() == vec![child]);
    assert!(cx.windows() == vec![main]);
    assert_idle(cx);

    request_close(cx, main);
    answer(cx, "退出软件");
    assert!(*closed.borrow() == vec![child, main]);
    cx.update(advance_application_exit);
    assert!(*closed.borrow() == vec![child, main]);
}
