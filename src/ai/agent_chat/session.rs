//! Finishing a committed delete and guarding a session transition.

use super::*;

/// Applies the in-memory side of a deletion that SQLite has already committed.
///
/// Releasing a process-local claim can fail only when its mutex is poisoned.
/// That must remain observable, but it cannot turn an irreversible, committed
/// delete into a reported failure or leave the UI displaying the deleted
/// conversation as selected.
pub(crate) fn finalize_committed_delete(
    selection: &mut ConversationSelection,
    reset_to_fresh: bool,
    preserved_session: Option<ChatSession>,
    release_claim: impl FnOnce() -> Result<()>,
) -> Option<ChatSession> {
    if !reset_to_fresh {
        return preserved_session;
    }

    *selection = ConversationSelection::Fresh;
    if let Err(error) = release_claim() {
        tracing::error!(
            target: "ngy_ai",
            stage = "release_deleted_session",
            error_kind = error_kind(&error),
            "conversation was deleted but its process-local claim could not be released"
        );
    }
    None
}

pub(crate) struct SessionTransitionGuard {
    pub(crate) active: Arc<AtomicBool>,
}

impl Drop for SessionTransitionGuard {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}
