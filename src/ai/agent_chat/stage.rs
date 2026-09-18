//! Staging a conversation step and the cancellation/host-scope guards.

use super::*;

pub(crate) async fn conversation_stage<T>(
    stage: &'static str,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    let started = Instant::now();
    tracing::debug!(target: "ngy_ai", stage, "AI conversation stage started");
    let result = future.await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => {
            tracing::debug!(target: "ngy_ai", stage, elapsed_ms, "AI conversation stage completed")
        }
        Err(error) if error.is::<AgentRequestCancelled>() => {
            tracing::debug!(target: "ngy_ai", stage, elapsed_ms, error_kind = "cancelled", "AI conversation stage cancelled")
        }
        Err(error) => {
            tracing::warn!(target: "ngy_ai", stage, elapsed_ms, error_kind = error_kind(error), "AI conversation stage failed")
        }
    }
    result
}

pub(crate) fn ensure_request_not_cancelled(cancellation: &AgentCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!(AgentRequestCancelled);
    }
    Ok(())
}

pub(crate) fn scope_excludes_prior_books(previous: &ChatScope, current: &ChatScope) -> bool {
    previous
        .book_ids
        .iter()
        .any(|book_id| !current.contains(book_id))
}
