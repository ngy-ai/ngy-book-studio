//! Cancelling an in-flight request and the error that reports it.

pub(super) use super::*;

#[derive(Clone, Default)]
pub struct AgentCancellation {
    pub(crate) inner: Arc<CancellationState>,
}

/// Host cancellation is a distinct outcome even after a completed request's
/// token is invalidated during cleanup. Keep its established display text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentRequestCancelled;

impl std::fmt::Display for AgentRequestCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AI request was cancelled")
    }
}

impl std::error::Error for AgentRequestCancelled {}

pub(crate) struct CancellationState {
    pub(crate) trace_id: u64,
    pub(crate) cancelled: AtomicBool,
    pub(crate) notify: Notify,
}

impl Default for CancellationState {
    fn default() -> Self {
        static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            trace_id: NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed),
            cancelled: AtomicBool::default(),
            notify: Notify::default(),
        }
    }
}

impl std::fmt::Debug for AgentCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl AgentCancellation {
    /// Process-local diagnostic identity, shared across preparation, execution
    /// and cancellation. Unlike UI request IDs it is unique across windows.
    pub fn trace_id(&self) -> u64 {
        self.inner.trace_id
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            tracing::debug!(target: "ngy_ai", trace_id = self.trace_id(), "AI request token invalidated");
            // A question has at most one active cancellation waiter. notify_one
            // also stores a permit when cancellation wins the tiny window
            // between the atomic check and polling `notified()`.
            self.inner.notify.notify_one();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(super) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}
