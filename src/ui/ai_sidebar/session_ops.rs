//! Applying, refreshing and failing persisted sessions, plus the answer
//! stream callbacks.

use super::*;

impl AiSidebar {
    /// Replaces the selected persisted conversation and its already-authorized
    /// transcript. Restoration is ignored while a request is in flight.
    pub(crate) fn apply_session(
        &mut self,
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
        messages: Vec<AiRestoredMessage>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.conversation.is_sending() {
            return false;
        }
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let requested_active = active_thread_id.clone();
        self.sessions.apply(
            active_thread_id,
            authorized_thread_options(threads, &allowed),
        );
        self.conversation.included_references.clear();
        self.conversation.messages.clear();
        let selected_active = self.sessions.active_thread_id.as_deref();
        if requested_active.as_deref() == selected_active {
            for message in messages {
                if message.content.is_empty() || message.content.len() > MAX_MESSAGE_BYTES {
                    continue;
                }
                let id = self.conversation.take_id();
                self.conversation.messages.push(AiMessage {
                    id,
                    role: match message.role {
                        AiRestoredRole::User => AiMessageRole::User,
                        AiRestoredRole::Assistant => AiMessageRole::Assistant,
                    },
                    content: message.content,
                    sources: message
                        .sources
                        .into_iter()
                        .filter(|source| allowed_source_link(&allowed, source))
                        .collect(),
                    source_status: message.source_status,
                    request_id: None,
                });
            }
        }
        cx.notify();
        self.resume_pending_scope_sync(cx);
        true
    }

    pub(crate) fn refresh_sessions(
        &mut self,
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
        cx: &mut Context<Self>,
    ) {
        let previous_active = self.sessions.active_thread_id.clone();
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        self.sessions.refresh(
            active_thread_id,
            authorized_thread_options(threads, &allowed),
        );
        if previous_active != self.sessions.active_thread_id {
            self.conversation.retain_current_request_messages();
            self.conversation.included_references.clear();
        }
        cx.notify();
        self.resume_pending_scope_sync(cx);
    }

    pub(crate) fn fail_session_operation(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.sessions.fail(error);
        cx.notify();
        self.resume_pending_scope_sync(cx);
    }

    /// Backend adapters call this for every streamed text delta. A false
    /// return means the request was cancelled or superseded.
    #[allow(dead_code)]
    pub(crate) fn append_answer_delta(
        &mut self,
        request_id: u64,
        delta: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.conversation.append_delta(request_id, delta);
        if changed {
            cx.notify();
        }
        changed
    }

    /// Removes provisional deltas after a streamed response turns into a tool
    /// call or before an uncommitted/error response is replaced.
    pub(crate) fn reset_answer(&mut self, request_id: u64, cx: &mut Context<Self>) -> bool {
        let changed = self.conversation.reset_answer(request_id);
        if changed {
            cx.notify();
        }
        changed
    }

    /// The caller supplies the already-persisted final Markdown, never a
    /// provisional stream buffer or an answer awaiting citation validation.
    pub(crate) fn finish_persisted_answer(
        &mut self,
        request_id: u64,
        markdown: String,
        sources: Vec<AiSourceLink>,
        source_status: AgentAnswerSourceStatus,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.conversation.finish(request_id, sources, source_status);
        if changed {
            if self.selection_explanation_request == Some(request_id) {
                self.selection_explanation_request = None;
            }
            cx.emit(AiAnswerCompleted {
                request_id,
                markdown,
            });
            cx.notify();
        }
        self.resume_pending_scope_sync(cx);
        changed
    }

    #[allow(dead_code)]
    pub(crate) fn fail_answer(
        &mut self,
        request_id: u64,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let error = error.into();
        let changed = self.conversation.fail(request_id, error.clone());
        if changed {
            self.fail_selection_explanation_request(request_id, error, cx);
            cx.notify();
        }
        self.resume_pending_scope_sync(cx);
        changed
    }
}
