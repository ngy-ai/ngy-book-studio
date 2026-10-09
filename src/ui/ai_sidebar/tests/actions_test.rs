//! Tests for the per-message actions: editing, regeneration and the
//! persisted anchors they rewind the stored thread to.

use super::*;

fn finished_turn(
    state: &mut ConversationState,
    question: &str,
    answer: &str,
    user_db_id: Option<&str>,
    assistant_db_id: Option<&str>,
) {
    let request = state.begin(question).unwrap();
    assert!(state.append_delta(request.request_id, answer));
    assert!(state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));
    state.set_persisted_ids(
        request.request_id,
        user_db_id.map(str::to_string),
        assistant_db_id.map(str::to_string),
    );
}

#[test]
fn editing_a_question_rewinds_the_transcript_and_anchors_the_stored_turn() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "first question",
        "first answer",
        Some("user-1"),
        Some("answer-1"),
    );
    finished_turn(
        &mut state,
        "second question",
        "second answer",
        Some("user-2"),
        Some("answer-2"),
    );
    assert_eq!(state.messages.len(), 4);

    let text = state.begin_edit(0).unwrap();
    assert_eq!(text, "first question");
    assert!(
        state.messages.is_empty(),
        "the edited question and everything after it must leave the transcript"
    );
    assert_eq!(state.pending_truncate_from.as_deref(), Some("user-1"));

    let request = state.begin("edited question").unwrap();
    assert_eq!(request.truncate_from.as_deref(), Some("user-1"));
    assert!(
        state.pending_truncate_from.is_none(),
        "the anchor is consumed by the submission that used it"
    );
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].content, "edited question");
}

#[test]
fn editing_only_applies_to_user_messages_and_only_when_idle() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "question",
        "answer",
        Some("user-1"),
        Some("answer-1"),
    );

    assert!(
        state.begin_edit(1).is_none(),
        "an assistant answer is not editable"
    );
    assert_eq!(state.messages.len(), 2);
    assert!(state.pending_truncate_from.is_none());
    assert!(state.begin_edit(9).is_none());

    let request = state.begin("running").unwrap();
    assert!(state.append_delta(request.request_id, "streaming"));
    assert!(state.begin_edit(0).is_none());
    assert!(state.begin_regenerate(1).is_none());
    assert_eq!(state.messages.len(), 4);
    assert!(state.pending_truncate_from.is_none());
}

#[test]
fn regenerating_an_answer_rewinds_from_its_own_question() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "first question",
        "first answer",
        Some("user-1"),
        Some("answer-1"),
    );
    finished_turn(
        &mut state,
        "second question",
        "second answer",
        Some("user-2"),
        Some("answer-2"),
    );

    // Regenerating the first answer rewrites the branch from its question:
    // everything from that question on is replaced, including the later turn
    // that was built on the answer being replaced.
    let question = state.begin_regenerate(1).unwrap();
    assert_eq!(question, "first question");
    assert!(state.messages.is_empty());
    assert_eq!(state.pending_truncate_from.as_deref(), Some("user-1"));

    let request = state.begin(&question).unwrap();
    assert_eq!(request.truncate_from.as_deref(), Some("user-1"));
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].content, "first question");
}

#[test]
fn regenerating_rejects_user_messages_and_questions_without_a_preceding_prompt() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let request = state.begin("question").unwrap();
    assert!(state.append_delta(request.request_id, "answer"));
    assert!(state.finish(
        request.request_id,
        Vec::new(),
        AgentAnswerSourceStatus::NoVerifiedSources,
    ));

    assert!(
        state.begin_regenerate(0).is_none(),
        "a question is not an answer"
    );
    assert!(state.begin_regenerate(7).is_none());
    assert_eq!(state.messages.len(), 2);

    // A restored transcript can contain an answer without its question after
    // the user's earlier scope change; it must not be regenerated blindly.
    state.messages.remove(0);
    assert!(state.begin_regenerate(0).is_none());
    assert_eq!(state.messages.len(), 1);
}

#[test]
fn an_edit_without_a_persisted_anchor_never_truncates_the_stored_thread() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(&mut state, "question", "answer", None, None);

    let text = state.begin_edit(0).unwrap();
    assert_eq!(text, "question");
    assert!(
        state.pending_truncate_from.is_none(),
        "without a stored anchor the rewrite stays local"
    );
    let request = state.begin("edited question").unwrap();
    assert!(request.truncate_from.is_none());
}

#[test]
fn switching_away_drops_a_pending_anchor() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "question",
        "answer",
        Some("user-1"),
        Some("answer-1"),
    );
    assert_eq!(state.begin_edit(0).unwrap(), "question");
    assert!(state.pending_truncate_from.is_some());

    state.retain_current_request_messages();
    assert!(state.pending_truncate_from.is_none());
    let request = state.begin("edited in another session").unwrap();
    assert!(
        request.truncate_from.is_none(),
        "an abandoned edit must not rewind the session that replaced it"
    );
}

#[test]
fn deleting_a_turn_removes_the_question_and_the_answer_right_after_it() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "first question",
        "first answer",
        Some("user-1"),
        Some("answer-1"),
    );
    finished_turn(
        &mut state,
        "second question",
        "second answer",
        Some("user-2"),
        Some("answer-2"),
    );

    assert_eq!(
        state.turn_deletion(0),
        Some(TurnDeletion::Stored("user-1".to_string()))
    );
    assert!(state.discard_turn(0));
    assert_eq!(
        state
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["second question", "second answer"],
        "the later turn survives"
    );
}

#[test]
fn deleting_a_live_turn_also_removes_its_failure_notice() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    let first = state.begin("first question").unwrap();
    assert!(state.fail(first.request_id, "provider failed"));

    assert_eq!(state.turn_deletion(0), Some(TurnDeletion::LocalOnly));
    assert!(state.discard_turn(0));
    assert!(state.messages.is_empty(), "the notice belongs to that turn");
}

#[test]
fn only_questions_offer_a_deletion_while_idle() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "question",
        "answer",
        Some("user-1"),
        Some("answer-1"),
    );

    assert!(
        state.turn_deletion(1).is_none(),
        "an answer is not deletable"
    );
    assert!(state.turn_deletion(9).is_none());
    assert_eq!(
        state.turn_deletion(0),
        Some(TurnDeletion::Stored("user-1".to_string()))
    );

    let running = state.begin("running question").unwrap();
    assert!(state.append_delta(running.request_id, "streaming"));
    assert!(state.turn_deletion(0).is_none());
    assert!(!state.discard_turn(0));
    assert_eq!(state.messages.len(), 4);
}

#[test]
fn persisted_ids_land_on_the_exact_question_and_answer_of_their_request() {
    let mut state = ConversationState::new(AiSidebarScope::library("All", vec![book("a")]));
    finished_turn(
        &mut state,
        "first",
        "first answer",
        Some("user-1"),
        Some("answer-1"),
    );
    finished_turn(
        &mut state,
        "second",
        "second answer",
        Some("user-2"),
        Some("answer-2"),
    );

    assert_eq!(state.messages[0].db_id.as_deref(), Some("user-1"));
    assert_eq!(state.messages[1].db_id.as_deref(), Some("answer-1"));
    assert_eq!(state.messages[2].db_id.as_deref(), Some("user-2"));
    assert_eq!(state.messages[3].db_id.as_deref(), Some("answer-2"));

    // Status and error notices never carry an anchor of their own.
    let failed = state.begin("third").unwrap();
    assert!(state.fail(failed.request_id, "fixture failure"));
    assert!(
        state
            .messages
            .iter()
            .filter(|message| message.request_id == Some(failed.request_id))
            .all(|message| message.db_id.is_none())
    );
}
