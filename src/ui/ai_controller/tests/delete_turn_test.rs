//! Deleting one stored turn through the controller and the conversation.
//!
//! The sidebar keeps showing the stored turn until the controller reports an
//! outcome, so these cover all three reload paths: a republished transcript, a
//! stale anchor (nothing written) and a storage failure.

use super::*;
use crate::ui::init_ui_test;
use gpui_kit::VisualTestContext;

struct TurnFixture {
    _temp: tempfile::TempDir,
    services: Arc<AppServices>,
    controller: Entity<AiSidebarController>,
    backend: AgentConversation,
    thread_id: String,
    first_question: String,
    sidebar: Entity<AiSidebar>,
}

fn restored(role: AiRestoredRole, content: &str, db_id: &str) -> AiRestoredMessage {
    AiRestoredMessage {
        role,
        content: content.to_string(),
        sources: Vec::new(),
        source_status: None,
        db_id: db_id.to_string(),
    }
}

fn follow_up_draft(parent: &StoredChatMessage, role: ChatRole, content: &str) -> NewChatMessage {
    NewChatMessage {
        parent_id: Some(parent.id.clone()),
        role,
        content: content.to_string(),
        model: matches!(role, ChatRole::Assistant).then(|| "chat-model".to_string()),
        citations: Vec::new(),
    }
}

/// Stores two full turns (question plus answer) in one reader session and
/// shows the same transcript in the sidebar.
fn open_turn_fixture(cx: &mut TestAppContext) -> (TurnFixture, &mut VisualTestContext) {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
    let book = library
        .create_book("Turn deletion fixture", "Author")
        .unwrap();
    drop(library);
    let services = Arc::new(AppServices::open(temp.path()).unwrap());
    let controller = AiSidebarController::new(
        Arc::clone(&services),
        ChatWindowKind::Reader,
        Some(book.id.clone()),
    )
    .unwrap();
    let backend = controller.conversation.clone();
    let mut stored = None;
    services.runtime().block_on(async {
        let repository = services.chat();
        let thread = repository
            .create_thread(NewChatThread {
                primary_book_id: Some(book.id.clone()),
                title: "Stored turn".to_string(),
                scope: ChatScope::new([book.id.clone()]).unwrap(),
                window_kind: ChatWindowKind::Reader,
            })
            .await
            .unwrap();
        let question = repository
            .append_message(
                &thread.id,
                NewChatMessage::text(ChatRole::User, "first question"),
            )
            .await
            .unwrap();
        let answer = repository
            .append_message(
                &thread.id,
                follow_up_draft(&question, ChatRole::Assistant, "first answer"),
            )
            .await
            .unwrap();
        let second = repository
            .append_message(
                &thread.id,
                follow_up_draft(&answer, ChatRole::User, "second question"),
            )
            .await
            .unwrap();
        let second_answer = repository
            .append_message(
                &thread.id,
                follow_up_draft(&second, ChatRole::Assistant, "second answer"),
            )
            .await
            .unwrap();
        backend
            .select_session(&thread.id, std::slice::from_ref(&book.id))
            .await
            .unwrap();
        stored = Some((
            thread.id,
            question.id,
            answer.id,
            second.id,
            second_answer.id,
        ));
    });
    let (thread_id, first_question, first_answer, second, second_answer) = stored.unwrap();
    init_ui_test(cx);
    let mut sidebar = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            AiSidebar::new(
                AiSidebarScope::book(
                    AiBookOption::new(&book.id, "Turn deletion fixture"),
                    Vec::new(),
                ),
                Arc::clone(&services),
                window,
                cx,
            )
        });
        sidebar = Some(view.clone());
        Root::new(view, window, cx)
    });
    let sidebar = sidebar.unwrap();
    sidebar.update(visual, |sidebar, cx| {
        sidebar.apply_session(
            Some(thread_id.clone()),
            vec![AiThreadOption::new(
                &thread_id,
                "Stored turn",
                vec![book.id.clone()],
            )],
            vec![
                restored(AiRestoredRole::User, "first question", &first_question),
                restored(AiRestoredRole::Assistant, "first answer", &first_answer),
                restored(AiRestoredRole::User, "second question", &second),
                restored(AiRestoredRole::Assistant, "second answer", &second_answer),
            ],
            cx,
        );
    });
    let controller = visual.new(|_| controller);
    (
        TurnFixture {
            _temp: temp,
            services,
            controller,
            backend,
            thread_id,
            first_question,
            sidebar,
        },
        visual,
    )
}

/// Waits for the controller to republish the transcript or report a failure.
/// Either outcome counts: the sidebar keeps the stored turn until then.
fn wait_for_outcome(
    sidebar: &Entity<AiSidebar>,
    visual: &mut VisualTestContext,
    before: &[String],
) {
    // The bound is generous because the whole binary test suite runs in
    // parallel and the shared IoRuntime can be busy; an operation that never
    // finishes still fails the test.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let done = sidebar.read_with(visual, |sidebar, _| {
            sidebar.session_error().is_some() || sidebar.transcript_contents() != before
        });
        if done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the delete operation did not finish in time"
        );
        visual.run_until_parked();
        // The product uses an independent Tokio runtime, outside GPUI's
        // deterministic executor. Give that real worker a bounded turn.
        std::thread::sleep(Duration::from_millis(1));
    }
    visual.run_until_parked();
}

fn transcript(sidebar: &Entity<AiSidebar>, visual: &VisualTestContext) -> Vec<String> {
    sidebar.read_with(visual, |sidebar, _| sidebar.transcript_contents())
}

fn session_error(sidebar: &Entity<AiSidebar>, visual: &VisualTestContext) -> Option<String> {
    sidebar.read_with(visual, |sidebar, _| {
        sidebar.session_error().map(str::to_string)
    })
}

/// The conversation layer refuses to guess a thread: with no selected session
/// there is nothing to rewind, and no stored row may be touched.
#[test]
fn conversation_level_delete_without_a_selected_session_is_a_no_op() {
    let temp = tempfile::tempdir().expect("temp dir");
    let services = Arc::new(AppServices::open(temp.path()).unwrap());
    let conversation = AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
        .expect("create conversation");
    services.runtime().block_on(async {
        assert!(
            conversation
                .delete_turn("chat-message-missing", &[])
                .await
                .unwrap()
                .is_none(),
            "a fresh session has no stored turn to rewind"
        );
    });
}

#[gpui_kit::test]
fn controller_deletes_one_stored_turn_and_republishes_the_transcript(cx: &mut TestAppContext) {
    let (fixture, visual) = open_turn_fixture(cx);
    let sidebar = fixture.sidebar.clone();
    let services = Arc::clone(&fixture.services);
    let thread_id = fixture.thread_id.clone();
    let first_question = fixture.first_question.clone();
    let before = transcript(&sidebar, visual);
    fixture.controller.update(visual, |controller, cx| {
        controller.delete_turn(first_question, sidebar.clone(), cx);
    });
    wait_for_outcome(&sidebar, visual, &before);
    assert_eq!(session_error(&sidebar, visual), None);
    assert_eq!(
        transcript(&sidebar, visual),
        ["second question", "second answer"]
    );
    // The reloaded transcript is the stored one, not a local edit.
    let stored = services
        .runtime()
        .block_on(async { services.chat().session(&thread_id).await.unwrap().unwrap() });
    assert_eq!(
        stored
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["second question", "second answer"]
    );
    assert_eq!(stored.messages[0].ordinal, 0);
    assert_eq!(stored.messages[1].ordinal, 1);
}

#[gpui_kit::test]
fn a_stale_turn_anchor_reports_without_touching_the_stored_session(cx: &mut TestAppContext) {
    let (fixture, visual) = open_turn_fixture(cx);
    let sidebar = fixture.sidebar.clone();
    let services = Arc::clone(&fixture.services);
    let thread_id = fixture.thread_id.clone();
    let before = transcript(&sidebar, visual);
    fixture.controller.update(visual, |controller, cx| {
        controller.delete_turn("chat-message-missing".to_string(), sidebar.clone(), cx);
    });
    wait_for_outcome(&sidebar, visual, &before);
    assert_eq!(
        session_error(&sidebar, visual),
        Some("这条问答已不在存储会话中，未做修改。".to_string())
    );
    assert_eq!(transcript(&sidebar, visual), before);
    let stored = services
        .runtime()
        .block_on(async { services.chat().session(&thread_id).await.unwrap().unwrap() });
    assert_eq!(stored.messages.len(), 4);
}

#[gpui_kit::test]
fn a_session_removed_from_storage_reports_and_drops_the_backend_selection(cx: &mut TestAppContext) {
    let (fixture, visual) = open_turn_fixture(cx);
    let sidebar = fixture.sidebar.clone();
    let services = Arc::clone(&fixture.services);
    let thread_id = fixture.thread_id.clone();
    let first_question = fixture.first_question.clone();
    let backend = fixture.backend.clone();
    services.runtime().block_on(async {
        assert!(
            services.chat().delete_thread(&thread_id).await.unwrap(),
            "the fixture thread must exist"
        );
    });
    let before = transcript(&sidebar, visual);
    fixture.controller.update(visual, |controller, cx| {
        controller.delete_turn(first_question, sidebar.clone(), cx);
    });
    wait_for_outcome(&sidebar, visual, &before);
    assert_eq!(
        session_error(&sidebar, visual),
        Some("这条问答已不在存储会话中，未做修改。".to_string())
    );
    assert_eq!(transcript(&sidebar, visual), before);
    assert!(
        services
            .runtime()
            .block_on(backend.selected_thread_id())
            .is_none(),
        "a conversation that no longer exists must not stay selected"
    );
}

#[gpui_kit::test]
fn a_storage_failure_reports_and_keeps_the_displayed_turn(cx: &mut TestAppContext) {
    let (fixture, visual) = open_turn_fixture(cx);
    let sidebar = fixture.sidebar.clone();
    let services = Arc::clone(&fixture.services);
    let first_question = fixture.first_question.clone();
    // Make every later connection fail: replace the database file with a
    // directory of the same name.
    let db_path = services.chat().database_path().to_path_buf();
    std::fs::remove_file(&db_path).expect("remove the chat database");
    std::fs::create_dir(&db_path).expect("replace it with a directory");
    let before = transcript(&sidebar, visual);
    fixture.controller.update(visual, |controller, cx| {
        controller.delete_turn(first_question, sidebar.clone(), cx);
    });
    wait_for_outcome(&sidebar, visual, &before);
    let error = session_error(&sidebar, visual).expect("the failure must be reported");
    assert!(error.starts_with("无法删除这条 AI 问答："), "{error}");
    assert_eq!(
        transcript(&sidebar, visual),
        before,
        "a failed delete must not change the displayed transcript"
    );
}
