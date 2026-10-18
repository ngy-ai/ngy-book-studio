//! The conversation state machine: messages, automatic references and the
//! in-flight request.

use super::*;

pub(crate) type AiReferenceKey = (String, String, Option<String>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutomaticReferenceState {
    pub(crate) key: AiReferenceKey,
    pub(crate) frozen_text: String,
    pub(crate) revision: Option<u64>,
    pub(crate) included: bool,
}

impl AutomaticReferenceState {
    pub(super) fn from_reference(reference: &AiReferenceHint) -> Option<Self> {
        let frozen_text = reference.frozen_text.as_ref()?;
        if frozen_text.trim().is_empty() {
            return None;
        }
        Some(Self {
            key: reference_key(reference),
            frozen_text: frozen_text.clone(),
            revision: reference.revision,
            included: true,
        })
    }

    pub(super) fn same_selection(&self, other: &Self) -> bool {
        self.key == other.key
            && self.frozen_text == other.frozen_text
            && self.revision == other.revision
    }
}

/// What one user message's delete action can do with the stored thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnDeletion {
    /// The question never reached persistence (a failed or cancelled request,
    /// or a stored ID that was never reported): only the local transcript
    /// changes.
    LocalOnly,
    /// The stored thread still holds this question and must drop it together
    /// with its answer.
    Stored(String),
}

#[derive(Clone, Debug)]
pub(crate) struct ConversationState {
    pub(crate) scope: AiSidebarScope,
    pub(crate) reference_hints: Vec<AiReferenceHint>,
    pub(crate) automatic_reference: Option<AutomaticReferenceState>,
    pub(crate) included_references: HashSet<AiReferenceKey>,
    pub(crate) messages: Vec<AiMessage>,
    pub(crate) request: RequestState,
    pub(crate) next_id: u64,
    /// Persisted message ID the next question rewinds the stored thread to.
    ///
    /// Set by `begin_edit`/`begin_regenerate`, consumed by `begin`, and
    /// cleared whenever the local transcript is replaced by another session,
    /// so an abandoned edit can never truncate an unrelated conversation.
    pub(crate) pending_truncate_from: Option<String>,
}

impl ConversationState {
    pub(super) fn new(scope: AiSidebarScope) -> Self {
        Self {
            scope,
            reference_hints: Vec::new(),
            automatic_reference: None,
            included_references: HashSet::new(),
            messages: Vec::new(),
            request: RequestState::Idle,
            next_id: 1,
            pending_truncate_from: None,
        }
    }

    pub(super) fn set_scope(&mut self, mut scope: AiSidebarScope) -> bool {
        // Reader/editor refreshes keep explicit additions that are still
        // offered by the host. The fixed current book can never be removed.
        if let (
            AiSidebarScope::Book {
                current: old_current,
                additional_ids: old_additional,
                ..
            },
            AiSidebarScope::Book {
                current,
                available,
                additional_ids,
            },
        ) = (&self.scope, &mut scope)
            && old_current.id == current.id
        {
            *additional_ids = old_additional
                .iter()
                .filter(|id| available.iter().any(|book| book.id == id.as_str()))
                .cloned()
                .collect();
        }
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.prune_references_to_scope();
        true
    }

    pub(super) fn set_reference_hints(&mut self, references: Vec<AiReferenceHint>) {
        let allowed = self.scope.book_ids().into_iter().collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        self.reference_hints = references
            .into_iter()
            .filter(|reference| allowed.contains(&reference.book_id))
            .filter(valid_reference_hint_locator)
            .filter(|reference| seen.insert(reference_key(reference)))
            .collect();
        let available = self
            .reference_hints
            .iter()
            .map(reference_key)
            .collect::<HashSet<_>>();
        self.included_references
            .retain(|reference| available.contains(reference));

        let previous = self.automatic_reference.take();
        self.automatic_reference = self
            .reference_hints
            .iter()
            .find_map(AutomaticReferenceState::from_reference)
            .map(|mut automatic| {
                if let Some(previous) = previous.as_ref()
                    && previous.same_selection(&automatic)
                {
                    // A routine Reader/Editor refresh must not re-enable the
                    // same selection after the user explicitly excluded it.
                    automatic.included = previous.included;
                }
                if automatic.included
                    && !self.included_references.contains(&automatic.key)
                    && self.included_references.len() >= MAX_SELECTED_REFERENCES
                {
                    automatic.included = false;
                }
                automatic
            });
    }

    pub(super) fn prune_references_to_scope(&mut self) {
        let allowed = self.scope.book_ids().into_iter().collect::<HashSet<_>>();
        self.reference_hints.retain(|reference| {
            allowed.contains(&reference.book_id) && valid_reference_hint_locator(reference)
        });
        let available = self
            .reference_hints
            .iter()
            .map(reference_key)
            .collect::<HashSet<_>>();
        self.included_references
            .retain(|reference| available.contains(reference));
        if self
            .automatic_reference
            .as_ref()
            .is_some_and(|automatic| !available.contains(&automatic.key))
        {
            self.automatic_reference = None;
        }
        for message in &mut self.messages {
            message
                .sources
                .retain(|source| allowed_source_link(&allowed, source));
        }
    }

    pub(super) fn toggle_reference(&mut self, reference: &AiReferenceHint) -> bool {
        let key = reference_key(reference);
        if self
            .automatic_reference
            .as_ref()
            .is_some_and(|automatic| automatic.key == key)
        {
            if self.reference_is_included_by_key(&key) {
                // One row represents the deduplicated effective reference. A
                // click removes both ways that exact row could be included;
                // unrelated manual references remain untouched.
                self.included_references.remove(&key);
                if let Some(automatic) = self.automatic_reference.as_mut() {
                    automatic.included = false;
                }
                return true;
            }
            if self.selected_reference_count() >= MAX_SELECTED_REFERENCES {
                return false;
            }
            if let Some(automatic) = self.automatic_reference.as_mut() {
                automatic.included = true;
            }
            return true;
        }
        if self.included_references.remove(&key) {
            return true;
        }
        if self.selected_reference_count() >= MAX_SELECTED_REFERENCES
            || !self
                .reference_hints
                .iter()
                .any(|available| reference_key(available) == key)
        {
            return false;
        }
        self.included_references.insert(key)
    }

    pub(super) fn reference_is_included(&self, reference: &AiReferenceHint) -> bool {
        self.reference_is_included_by_key(&reference_key(reference))
    }

    pub(super) fn reference_is_included_by_key(&self, key: &AiReferenceKey) -> bool {
        self.included_references.contains(key)
            || self
                .automatic_reference
                .as_ref()
                .is_some_and(|automatic| automatic.included && automatic.key == *key)
    }

    pub(super) fn selected_reference_count(&self) -> usize {
        self.included_references.len()
            + usize::from(self.automatic_reference.as_ref().is_some_and(|automatic| {
                automatic.included && !self.included_references.contains(&automatic.key)
            }))
    }

    pub(super) fn begin(&mut self, question: &str) -> Option<AiQuestionRequest> {
        if !matches!(self.request, RequestState::Idle) {
            return None;
        }
        let question = question.trim();
        if question.is_empty() || question.len() > MAX_QUESTION_BYTES {
            return None;
        }
        let request_id = self.take_id();
        let message_id = self.take_id();
        // Consumed only once the question is accepted: a rejected submission
        // (empty or oversized text) must keep the anchor for the next attempt.
        let truncate_from = self.pending_truncate_from.take();
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::User,
            content: question.to_string(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
            db_id: None,
        });
        self.request = RequestState::Sending { request_id };
        let reference_hints = self
            .reference_hints
            .iter()
            .filter(|reference| self.reference_is_included(reference))
            .cloned()
            .collect::<Vec<_>>();
        Some(AiQuestionRequest {
            request_id,
            question: question.to_string(),
            book_ids: self.scope.book_ids(),
            book_titles: self
                .scope
                .selected_books()
                .into_iter()
                .map(|book| (book.id, book.title))
                .collect(),
            // Editor currently freezes its one selected chapter through this
            // field after its exact href/revision/request-id acknowledgement.
            reference: reference_hints.first().cloned(),
            reference_hints,
            truncate_from,
        })
    }

    /// Starts an edit of one user message: the message and everything after it
    /// disappear from the local transcript, its text is handed back for the
    /// input box, and the stored thread rewinds to the same message once the
    /// edit is submitted.
    ///
    /// Returns `None` while a request is running, for non-user messages, and
    /// for out-of-range indices.
    pub(super) fn begin_edit(&mut self, index: usize) -> Option<String> {
        if !matches!(self.request, RequestState::Idle) {
            return None;
        }
        let message = self.messages.get(index)?;
        if message.role != AiMessageRole::User {
            return None;
        }
        let content = message.content.clone();
        self.pending_truncate_from = message.db_id.clone();
        if self.pending_truncate_from.is_none() {
            // The question never reached persistence (or its stored ID was
            // never reported): the local rewrite stays local, because guessing
            // an anchor would cut an unrelated part of the stored thread.
            tracing::debug!(
                target: "ngy_ai",
                stage = "edit_message",
                "edited question has no persisted anchor; stored thread stays untouched"
            );
        }
        self.messages.truncate(index);
        Some(content)
    }

    /// Prepares a regeneration of one answer: every message from the question
    /// that produced it on is removed locally and the same question is handed
    /// back for immediate resubmission. The stored thread rewinds to that
    /// question, so the new answer replaces the old turn instead of stacking
    /// behind it.
    ///
    /// Returns `None` while a request is running, for non-assistant messages,
    /// and when no preceding question exists.
    pub(super) fn begin_regenerate(&mut self, index: usize) -> Option<String> {
        if !matches!(self.request, RequestState::Idle) {
            return None;
        }
        let message = self.messages.get(index)?;
        if message.role != AiMessageRole::Assistant {
            return None;
        }
        let question_index = self.messages[..index]
            .iter()
            .rposition(|message| message.role == AiMessageRole::User)?;
        let question = self.messages[question_index].content.clone();
        self.pending_truncate_from = self.messages[question_index].db_id.clone();
        if self.pending_truncate_from.is_none() {
            tracing::debug!(
                target: "ngy_ai",
                stage = "regenerate_message",
                "regenerated question has no persisted anchor; stored thread stays untouched"
            );
        }
        self.messages.truncate(question_index);
        Some(question)
    }

    /// Decides what deleting one message can do: only user questions are
    /// deletable, and only while no answer is being written.
    pub(super) fn turn_deletion(&self, index: usize) -> Option<TurnDeletion> {
        if !matches!(self.request, RequestState::Idle) {
            return None;
        }
        let message = self.messages.get(index)?;
        if message.role != AiMessageRole::User {
            return None;
        }
        Some(match message.db_id.clone() {
            Some(db_id) => TurnDeletion::Stored(db_id),
            None => TurnDeletion::LocalOnly,
        })
    }

    /// Removes one question and the answer (or failure notice) right after it
    /// from the local transcript, keeping the later turns.
    pub(super) fn discard_turn(&mut self, index: usize) -> bool {
        if !matches!(self.request, RequestState::Idle) {
            return false;
        }
        let Some(message) = self.messages.get(index) else {
            return false;
        };
        if message.role != AiMessageRole::User {
            return false;
        }
        // A live turn is one user question plus its answer or failure notice,
        // both carrying that question's request generation. A restored
        // transcript has no request generation, so the answer is simply the
        // assistant message right after the question.
        let request_id = message.request_id;
        let next = self
            .messages
            .get(index + 1)
            .map(|next| (next.request_id, next.role));
        let answer_follows = next.is_some_and(|(next_id, role)| match (request_id, next_id) {
            (Some(request_id), Some(next_id)) => {
                next_id == request_id && role != AiMessageRole::User
            }
            _ => role == AiMessageRole::Assistant,
        });
        let removed = if answer_follows { 2 } else { 1 };
        self.messages.drain(index..index + removed);
        true
    }

    /// Records the persisted IDs the backend reported for one finished answer.
    ///
    /// The question and the answer of a live request are stored by the
    /// backend, not by the sidebar, so the sidebar can only anchor a later
    /// edit/regeneration after this callback. Cancelled, failed or locally
    /// reset messages keep `db_id = None` and stay out of the stored thread's
    /// rewrite path.
    pub(super) fn set_persisted_ids(
        &mut self,
        request_id: u64,
        user_db_id: Option<String>,
        assistant_db_id: Option<String>,
    ) {
        for message in &mut self.messages {
            if message.request_id != Some(request_id) {
                continue;
            }
            match message.role {
                AiMessageRole::User => message.db_id = user_db_id.clone(),
                AiMessageRole::Assistant => message.db_id = assistant_db_id.clone(),
                AiMessageRole::Status | AiMessageRole::Error => {}
            }
        }
    }

    /// Called only after the controller has successfully started a fresh
    /// backend conversation. Never inherit manual references or an excluded
    /// automatic selection from the previous conversation.
    pub(super) fn begin_selection_explanation(
        &mut self,
        reference: AiReferenceHint,
        prompt: &str,
    ) -> Option<AiQuestionRequest> {
        if self.is_sending()
            || !self.scope.book_ids().contains(&reference.book_id)
            || !valid_reference_hint_locator(&reference)
            || reference
                .frozen_text
                .as_ref()
                .is_none_or(|text| text.trim().is_empty())
        {
            return None;
        }
        let question = selection_explanation_question(&reference, prompt);
        self.messages.clear();
        // The explanation replaces the transcript entirely; an anchor left
        // over from an abandoned edit belongs to the discarded conversation.
        self.pending_truncate_from = None;
        self.included_references.clear();
        self.automatic_reference = None;
        self.set_reference_hints(vec![reference]);
        self.begin(&question)
    }

    pub(super) fn cancel(&mut self) -> Option<u64> {
        let RequestState::Sending { request_id } = self.request else {
            return None;
        };
        self.discard_answer_message(request_id);
        self.request = RequestState::Idle;
        let message_id = self.take_id();
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::Status,
            content: "已取消本次回答。".to_string(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
            db_id: None,
        });
        Some(request_id)
    }

    #[allow(dead_code)]
    pub(super) fn append_delta(&mut self, request_id: u64, delta: &str) -> bool {
        if !self.is_current(request_id) || delta.is_empty() {
            return false;
        }
        if let Some(message) = self.messages.iter_mut().rev().find(|message| {
            message.request_id == Some(request_id) && message.role == AiMessageRole::Assistant
        }) {
            if message.content.len().saturating_add(delta.len()) > MAX_MESSAGE_BYTES {
                return false;
            }
            message.content.push_str(delta);
        } else {
            if delta.len() > MAX_MESSAGE_BYTES {
                return false;
            }
            let message_id = self.take_id();
            self.messages.push(AiMessage {
                id: message_id,
                role: AiMessageRole::Assistant,
                content: delta.to_string(),
                sources: Vec::new(),
                source_status: None,
                request_id: Some(request_id),
                db_id: None,
            });
        }
        true
    }

    #[allow(dead_code)]
    pub(super) fn reset_answer(&mut self, request_id: u64) -> bool {
        if !self.is_current(request_id) {
            return false;
        }
        self.discard_answer_message(request_id)
    }

    #[allow(dead_code)]
    pub(super) fn finish(
        &mut self,
        request_id: u64,
        sources: Vec<AiSourceLink>,
        source_status: AgentAnswerSourceStatus,
    ) -> bool {
        if !self.is_current(request_id) {
            return false;
        }
        let allowed = self.scope.book_ids().into_iter().collect::<HashSet<_>>();
        let sources = sources
            .into_iter()
            .filter(|source| allowed_source_link(&allowed, source))
            .collect();
        if let Some(message) = self.messages.iter_mut().rev().find(|message| {
            message.request_id == Some(request_id) && message.role == AiMessageRole::Assistant
        }) {
            message.sources = sources;
            message.source_status = Some(source_status);
        } else {
            let message_id = self.take_id();
            self.messages.push(AiMessage {
                id: message_id,
                role: AiMessageRole::Assistant,
                content: "模型没有返回文本。".to_string(),
                sources,
                source_status: Some(source_status),
                request_id: Some(request_id),
                db_id: None,
            });
        }
        self.request = RequestState::Idle;
        true
    }

    #[allow(dead_code)]
    pub(super) fn fail(&mut self, request_id: u64, message: impl Into<String>) -> bool {
        if !self.is_current(request_id) {
            return false;
        }
        self.discard_answer_message(request_id);
        self.request = RequestState::Idle;
        let message_id = self.take_id();
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::Error,
            content: message.into(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
            db_id: None,
        });
        true
    }

    pub(super) fn discard_answer_message(&mut self, request_id: u64) -> bool {
        let before = self.messages.len();
        self.messages.retain(|message| {
            message.request_id != Some(request_id) || message.role != AiMessageRole::Assistant
        });
        self.messages.len() != before
    }

    #[allow(dead_code)]
    pub(super) fn is_current(&self, request_id: u64) -> bool {
        self.request == RequestState::Sending { request_id }
    }

    pub(super) fn is_sending(&self) -> bool {
        matches!(self.request, RequestState::Sending { .. })
    }

    pub(super) fn retain_current_request_messages(&mut self) {
        match self.request {
            RequestState::Sending { request_id } => self
                .messages
                .retain(|message| message.request_id == Some(request_id)),
            RequestState::Idle => self.messages.clear(),
        }
        if !self.is_sending() {
            // Another transcript is being loaded; an anchor from the discarded
            // one must not rewind the conversation that replaces it.
            self.pending_truncate_from = None;
        }
    }

    pub(super) fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}
