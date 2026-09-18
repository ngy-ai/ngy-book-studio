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

#[derive(Clone, Debug)]
pub(crate) struct ConversationState {
    pub(crate) scope: AiSidebarScope,
    pub(crate) reference_hints: Vec<AiReferenceHint>,
    pub(crate) automatic_reference: Option<AutomaticReferenceState>,
    pub(crate) included_references: HashSet<AiReferenceKey>,
    pub(crate) messages: Vec<AiMessage>,
    pub(crate) request: RequestState,
    pub(crate) next_id: u64,
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
        self.messages.push(AiMessage {
            id: message_id,
            role: AiMessageRole::User,
            content: question.to_string(),
            sources: Vec::new(),
            source_status: None,
            request_id: Some(request_id),
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
        })
    }

    /// Called only after the controller has successfully started a fresh
    /// backend conversation. Never inherit manual references or an excluded
    /// automatic selection from the previous conversation.
    pub(super) fn begin_selection_explanation(
        &mut self,
        reference: AiReferenceHint,
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
        let question = selection_explanation_question(&reference);
        self.messages.clear();
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
    }

    pub(super) fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}
