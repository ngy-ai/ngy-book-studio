//! The session picker and the authorization filter on stored threads.

use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionPickerState {
    pub(crate) threads: Vec<AiThreadOption>,
    pub(crate) active_thread_id: Option<String>,
    pub(crate) open: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    pub(crate) pending_delete: Option<PendingSessionDeletion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSessionDeletion {
    pub(crate) thread_id: String,
    pub(crate) title: String,
}

impl SessionPickerState {
    pub(super) fn active_title(&self) -> &str {
        self.active_thread_id
            .as_deref()
            .and_then(|active| {
                self.threads
                    .iter()
                    .find(|thread| thread.id == active)
                    .map(|thread| thread.title.as_str())
            })
            .unwrap_or("新对话")
    }

    pub(super) fn begin(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.open = false;
        self.error = None;
        self.pending_delete = None;
        true
    }

    pub(super) fn apply(&mut self, active_thread_id: Option<String>, threads: Vec<AiThreadOption>) {
        self.threads = normalized_thread_options(threads);
        self.active_thread_id = active_thread_id.filter(|active| {
            self.threads
                .iter()
                .any(|thread| thread.id == active.as_str())
        });
        self.busy = false;
        self.open = false;
        self.error = None;
        self.pending_delete = None;
    }

    pub(super) fn refresh(
        &mut self,
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
    ) {
        let was_open = self.open;
        self.apply(active_thread_id, threads);
        self.open = was_open;
    }

    pub(super) fn fail(&mut self, error: impl Into<String>) {
        self.busy = false;
        self.error = Some(error.into());
    }
}

pub(crate) fn normalized_thread_options(threads: Vec<AiThreadOption>) -> Vec<AiThreadOption> {
    let mut seen = HashSet::new();
    threads
        .into_iter()
        .filter(|thread| !thread.id.trim().is_empty() && seen.insert(thread.id.clone()))
        .collect()
}

pub(crate) fn thread_scope_is_authorized(
    thread: &AiThreadOption,
    allowed: &HashSet<String>,
) -> bool {
    thread
        .book_ids
        .iter()
        .all(|book_id| allowed.contains(book_id))
}

pub(crate) fn authorized_thread_options(
    threads: Vec<AiThreadOption>,
    allowed: &HashSet<String>,
) -> Vec<AiThreadOption> {
    normalized_thread_options(threads)
        .into_iter()
        .filter(|thread| thread_scope_is_authorized(thread, allowed))
        .collect()
}
