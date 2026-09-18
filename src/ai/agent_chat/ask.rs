//! Asking a question and preparing a request.

use super::*;

impl AgentConversation {
    pub async fn selected_thread_id(&self) -> Option<String> {
        match &*self.selection.lock().await {
            ConversationSelection::Selected(thread_id) => Some(thread_id.clone()),
            ConversationSelection::Uninitialized | ConversationSelection::Fresh => None,
        }
    }

    pub(super) fn begin_session_transition(&self) -> Result<SessionTransitionGuard> {
        ensure!(
            self.session_transition
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "conversation session is already changing"
        );
        let guard = SessionTransitionGuard {
            active: Arc::clone(&self.session_transition),
        };
        let active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation cancellation lock is poisoned"))?;
        ensure!(
            active.is_empty(),
            "cannot change conversation while an AI request is active"
        );
        drop(active);
        Ok(guard)
    }

    pub(super) fn validate_session_context(&self, thread: &ChatThread) -> Result<()> {
        ensure!(
            thread.window_kind == self.window_kind,
            "conversation belongs to another window kind"
        );
        ensure!(
            thread.primary_book_id == self.primary_book_id,
            "conversation belongs to another primary book"
        );
        Ok(())
    }

    pub(super) fn host_scope(&self, allowed_book_ids: &[String]) -> Result<ChatScope> {
        let scope = ChatScope::new(allowed_book_ids.iter().cloned())?;
        match self.window_kind {
            ChatWindowKind::Library => {}
            ChatWindowKind::Reader | ChatWindowKind::Editor => {
                let primary = self
                    .primary_book_id
                    .as_deref()
                    .context("conversation primary book is missing")?;
                ensure!(
                    scope.contains(primary),
                    "host scope does not contain the required current book"
                );
            }
        }
        Ok(scope)
    }

    pub(super) fn thread_scope_is_authorized(
        &self,
        thread: &ChatThread,
        host_scope: &ChatScope,
    ) -> bool {
        !scope_excludes_prior_books(&thread.scope, host_scope)
    }

    pub async fn ask(
        &self,
        request: ConversationQuestion,
        events: Option<mpsc::UnboundedSender<AgentRunEvent>>,
    ) -> Result<ConversationAnswer> {
        let prepared = self.prepare_request(request.request_id)?;
        self.ask_prepared(request, events, prepared).await
    }

    /// Registers cancellation synchronously so callers can safely perform
    /// asynchronous snapshot preparation before invoking the agent.
    pub fn prepare_request(&self, request_id: u64) -> Result<PreparedAgentRequest> {
        let cancellation = AgentCancellation::default();
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation cancellation lock is poisoned"))?;
        ensure!(
            !self.session_transition.load(Ordering::Acquire),
            "conversation session is changing"
        );
        ensure!(
            !active.contains_key(&request_id),
            "request ID is already active"
        );
        let ai_snapshot = self.services.ai_request_snapshot()?;
        active.insert(request_id, cancellation.clone());
        drop(active);
        tracing::debug!(
            target: "ngy_ai",
            trace_id = cancellation.trace_id(),
            request_id,
            window_kind = ?self.window_kind,
            stage = "prepare_request",
            "AI request registered before reference preparation"
        );
        Ok(PreparedAgentRequest {
            request_id,
            cancellation,
            ai_snapshot,
            active: Arc::clone(&self.active),
        })
    }

    /// Executes an already registered request. The same lease must remain in
    /// the originating conversation, preventing a cancelled preparation from
    /// being re-registered after it finishes.
    pub async fn ask_prepared(
        &self,
        request: ConversationQuestion,
        events: Option<mpsc::UnboundedSender<AgentRunEvent>>,
        prepared: PreparedAgentRequest,
    ) -> Result<ConversationAnswer> {
        let span = tracing::info_span!(
            target: "ngy_ai",
            "ai_conversation",
            trace_id = prepared.cancellation.trace_id(),
            request_id = request.request_id,
            window_kind = ?self.window_kind,
            allowed_books = request.allowed_book_ids.len(),
            snapshots = request.snapshots.len(),
            question_bytes = request.question.len()
        );
        conversation_stage("conversation_request", async {
            conversation_stage("validate_prepared_request", async {
                ensure!(
                    !request.question.trim().is_empty(),
                    "question must not be empty"
                );
                ensure!(
                    request.request_id == prepared.request_id,
                    "prepared request ID does not match the question"
                );
                ensure!(
                    Arc::ptr_eq(&prepared.active, &self.active),
                    "prepared request belongs to another conversation"
                );
                let registered = self
                    .active
                    .lock()
                    .map_err(|_| anyhow::anyhow!("conversation cancellation lock is poisoned"))?
                    .get(&prepared.request_id)
                    .is_some_and(|token| token.same_request(&prepared.cancellation));
                ensure!(registered, "prepared request is no longer active");
                ensure_request_not_cancelled(&prepared.cancellation)
            })
            .await?;
            self.ask_inner(
                request,
                events,
                prepared.cancellation.clone(),
                prepared.ai_snapshot.clone(),
            )
            .await
        })
        .instrument(span)
        .await
    }
}
