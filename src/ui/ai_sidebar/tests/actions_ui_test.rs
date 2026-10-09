//! GUI regression for the per-message action row: the buttons must render
//! under their own bubble, editing must move the question back into the input
//! box, regenerating must resubmit that bubble's question with the exact
//! persisted anchor, and deleting must leave the stored turn to the backend.

use super::*;
use gpui_kit::component::Root;
use gpui_kit::{Modifiers, Subscription, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    cx.run_until_parked();
}

#[derive(Default)]
struct EventLog {
    submits: Vec<AiQuestionRequest>,
    deleted_turns: Vec<String>,
}

type EventLogHandle = Rc<RefCell<EventLog>>;

fn open_sidebar(
    cx: &mut TestAppContext,
) -> (
    Entity<AiSidebar>,
    EventLogHandle,
    Subscription,
    &mut VisualTestContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(temp.path()).unwrap());
    init_ui_test(cx);
    let mut sidebar = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(book("book"), Vec::new()),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        sidebar = Some(view.clone());
        Root::new(view, window, cx)
    });
    let sidebar = sidebar.expect("the root must contain the sidebar");
    // The test window is narrower than the sidebar's collapse threshold; a
    // user would see the same layout after expanding the sidebar manually.
    sidebar.update(visual, |sidebar, cx| {
        sidebar.layout.collapsed = false;
        sidebar.layout.manual_override = true;
        cx.notify();
    });
    let events = Rc::new(RefCell::new(EventLog::default()));
    let recorded = Rc::clone(&events);
    let subscription = sidebar.update(visual, |_, cx| {
        cx.subscribe(
            &sidebar,
            move |_, _, event: &AiSidebarEvent, _| match event {
                AiSidebarEvent::Submit(request) => {
                    recorded.borrow_mut().submits.push(request.clone())
                }
                AiSidebarEvent::DeleteTurn { user_db_id } => {
                    recorded.borrow_mut().deleted_turns.push(user_db_id.clone());
                }
                _ => {}
            },
        )
    });
    (sidebar, events, subscription, visual)
}

fn restore_one_turn(sidebar: &Entity<AiSidebar>, cx: &mut VisualTestContext) {
    sidebar.update(cx, |sidebar, cx| {
        sidebar.apply_session(
            Some("thread".to_string()),
            vec![AiThreadOption::new(
                "thread",
                "Thread",
                vec!["book".to_string()],
            )],
            vec![
                AiRestoredMessage {
                    role: AiRestoredRole::User,
                    content: "first question".to_string(),
                    sources: Vec::new(),
                    source_status: None,
                    db_id: "stored-user".to_string(),
                },
                AiRestoredMessage {
                    role: AiRestoredRole::Assistant,
                    content: "first answer".to_string(),
                    sources: Vec::new(),
                    source_status: Some(AgentAnswerSourceStatus::NoVerifiedSources),
                    db_id: "stored-answer".to_string(),
                },
            ],
            cx,
        );
    });
    redraw(cx);
}

#[gpui_kit::test]
fn clicking_edit_moves_the_question_back_into_the_input_box(cx: &mut TestAppContext) {
    let (sidebar, _, _subscription, visual) = open_sidebar(cx);
    restore_one_turn(&sidebar, visual);

    let edit = visual
        .debug_bounds("ai-message-edit-1")
        .expect("the question must render its edit action");
    assert!(
        visual.debug_bounds("ai-message-regenerate-1").is_none(),
        "a question offers no regeneration"
    );
    assert!(
        visual.debug_bounds("ai-message-delete-2").is_none(),
        "an answer offers no deletion of its own"
    );
    visual.simulate_click(edit.center(), Modifiers::none());
    redraw(visual);

    sidebar.read_with(visual, |sidebar, cx| {
        assert_eq!(
            sidebar.input.read(cx).value(),
            "first question",
            "editing must hand the question back for rewriting"
        );
        assert!(
            sidebar.conversation.messages.is_empty(),
            "the edited question and the answer after it leave the transcript"
        );
        assert_eq!(
            sidebar.conversation.pending_truncate_from.as_deref(),
            Some("stored-user"),
            "the stored turn is rewound when the edit is submitted"
        );
    });
}

#[gpui_kit::test]
fn clicking_regenerate_resubmits_the_questions_own_turn(cx: &mut TestAppContext) {
    let (sidebar, events, _subscription, visual) = open_sidebar(cx);
    restore_one_turn(&sidebar, visual);

    let regenerate = visual
        .debug_bounds("ai-message-regenerate-2")
        .expect("the answer must render its regenerate action");
    visual.simulate_click(regenerate.center(), Modifiers::none());
    redraw(visual);

    let emitted = events.borrow();
    assert_eq!(
        emitted.submits.len(),
        1,
        "regeneration submits exactly one request"
    );
    assert_eq!(emitted.submits[0].question, "first question");
    assert_eq!(
        emitted.submits[0].truncate_from.as_deref(),
        Some("stored-user")
    );
    drop(emitted);

    sidebar.read_with(visual, |sidebar, _| {
        assert_eq!(sidebar.conversation.messages.len(), 1);
        assert_eq!(sidebar.conversation.messages[0].content, "first question");
        assert!(sidebar.conversation.is_sending());
    });
}

#[gpui_kit::test]
fn clicking_delete_waits_for_the_stored_turn_to_be_removed(cx: &mut TestAppContext) {
    let (sidebar, events, _subscription, visual) = open_sidebar(cx);
    restore_one_turn(&sidebar, visual);

    let delete = visual
        .debug_bounds("ai-message-delete-1")
        .expect("the question must render its delete action");
    visual.simulate_click(delete.center(), Modifiers::none());
    redraw(visual);

    assert_eq!(
        events.borrow().deleted_turns,
        vec!["stored-user".to_string()],
        "the stored question's own ID is the deletion anchor"
    );
    sidebar.read_with(visual, |sidebar, _| {
        assert_eq!(
            sidebar.conversation.messages.len(),
            2,
            "a stored turn stays visible until the backend confirms the deletion"
        );
        assert!(
            sidebar.sessions.busy,
            "the sidebar waits for the reloaded transcript"
        );
    });
}

#[gpui_kit::test]
fn deleting_an_unpersisted_question_only_changes_the_transcript(cx: &mut TestAppContext) {
    let (sidebar, events, _subscription, visual) = open_sidebar(cx);
    sidebar.update(visual, |sidebar, cx| {
        sidebar.conversation.messages.push(AiMessage {
            id: 7,
            role: AiMessageRole::User,
            content: "unpersisted question".to_string(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(3),
            db_id: None,
        });
        cx.notify();
    });
    redraw(visual);

    let delete = visual
        .debug_bounds("ai-message-delete-7")
        .expect("the unpersisted question must still offer deletion");
    visual.simulate_click(delete.center(), Modifiers::none());
    redraw(visual);

    assert!(
        events.borrow().deleted_turns.is_empty(),
        "nothing is sent to the backend without a stored anchor"
    );
    sidebar.read_with(visual, |sidebar, _| {
        assert!(
            sidebar.conversation.messages.is_empty(),
            "the local transcript drops the turn immediately"
        );
        assert!(!sidebar.sessions.busy);
    });
}
