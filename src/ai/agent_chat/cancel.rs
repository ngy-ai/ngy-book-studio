//! Claiming the recent session and cancelling requests.

use super::*;

impl AgentConversation {
    pub(super) async fn claim_recent_session(
        &self,
        repository: &ChatRepository,
        host_scope: &ChatScope,
    ) -> Result<Option<ChatSession>> {
        for thread in repository
            .list_threads(self.window_kind, self.primary_book_id.as_deref())
            .await?
        {
            self.validate_session_context(&thread)?;
            if !self.thread_scope_is_authorized(&thread, host_scope) {
                continue;
            }
            if !self.lease.switch_claim(&thread.id)? {
                continue;
            }
            match repository.session(&thread.id).await? {
                Some(session) => {
                    self.validate_session_context(&session.thread)?;
                    if !self.thread_scope_is_authorized(&session.thread, host_scope) {
                        self.lease.clear_claim()?;
                        continue;
                    }
                    return Ok(Some(session));
                }
                None => self.lease.clear_claim()?,
            }
        }
        Ok(None)
    }

    pub fn cancel(&self, request_id: u64) -> bool {
        let Ok(active) = self.active.lock() else {
            return false;
        };
        let Some(token) = active.get(&request_id) else {
            tracing::debug!(target: "ngy_ai", request_id, window_kind = ?self.window_kind, "AI cancellation ignored for inactive request");
            return false;
        };
        tracing::info!(target: "ngy_ai", trace_id = token.trace_id(), request_id, window_kind = ?self.window_kind, "AI conversation cancellation requested");
        token.cancel();
        true
    }

    pub fn cancel_all(&self) {
        if let Ok(active) = self.active.lock() {
            for (request_id, token) in active.iter() {
                tracing::info!(target: "ngy_ai", trace_id = token.trace_id(), request_id, window_kind = ?self.window_kind, "AI conversation closing; cancelling request");
                token.cancel();
            }
        }
    }
}
