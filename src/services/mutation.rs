//! The FIFO turnstile that serialises library mutations against readers.

use super::*;

pub(crate) struct AiServices {
    pub(crate) settings: ProviderSettings,
    pub(crate) provider: Arc<dyn OpenAiCompatibleProvider>,
    /// 翻译任务专用连接：与问答分开绑定 Endpoint，因此换问答模型不影响翻译。
    pub(crate) translation_provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) vision_provider: Arc<dyn OpenAiCompatibleProvider>,
    pub(crate) search: Arc<SearchService>,
}

/// Immutable request view, captured before asynchronous selection or history
/// preparation so saving settings cannot mix model, endpoint and search roles.
#[derive(Clone)]
pub(crate) struct AiRequestSnapshot {
    pub settings: ProviderSettings,
    pub provider: Arc<dyn OpenAiCompatibleProvider>,
    pub search: Arc<SearchService>,
}

impl std::fmt::Debug for AiServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiServices")
            .field("settings", &self.settings)
            .field("search", &self.search)
            .finish_non_exhaustive()
    }
}

/// Result of a shared library mutation together with the exact in-memory
/// projection published by that mutation.
#[derive(Debug)]
pub struct LibraryMutation<T> {
    pub value: T,
    pub snapshot: LibraryStore,
    pub generation: u64,
}

/// How long one admitted turn may hold the queue before the dispatcher warns
/// about it (`ngy_library` target). A turn is held for the whole operation —
/// including its SQLite transaction and object I/O — so a warning here is the
/// first evidence that what looks like a frozen library is one slow operation
/// rather than a stuck queue.
const HOLDING_TURN_WARNING: Duration = Duration::from_secs(5);

/// FIFO admission for every mutation of the process-level library snapshot.
///
/// `spawn_blocking` does not promise that jobs begin in submission order. A
/// plain mutex therefore lets a later UI request commit before an earlier one
/// and then be overwritten by it. Tickets are assigned and queued
/// synchronously at the API boundary. A single asynchronous dispatcher grants
/// exactly one turn at a time, so requests waiting for their turn consume
/// neither Tokio core threads nor blocking-pool threads.
#[derive(Debug)]
pub(crate) struct LibraryMutationQueue {
    pub(crate) next_ticket: AtomicU64,
    pub(crate) requests: tokio::sync::mpsc::UnboundedSender<LibraryMutationRequest>,
}

impl LibraryMutationQueue {
    pub(crate) fn new(runtime: &tokio::runtime::Handle) -> Self {
        let (requests, receiver) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(run_library_mutation_turnstile(receiver));
        Self {
            next_ticket: AtomicU64::new(0),
            requests,
        }
    }

    pub(crate) fn reserve(&self) -> LibraryMutationReservation {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let (ready, turn) = tokio::sync::oneshot::channel();
        if let Err(stopped) = self.requests.send(LibraryMutationRequest { ticket, ready }) {
            // Closing the returned sender makes `enter` report a useful error
            // instead of leaving an unresolvable reservation behind.
            drop(stopped.0.ready);
        }
        LibraryMutationReservation { ticket, turn }
    }
}

#[derive(Debug)]
pub(crate) struct LibraryMutationRequest {
    pub(crate) ticket: u64,
    pub(crate) ready: tokio::sync::oneshot::Sender<LibraryMutationTurn>,
}

#[derive(Debug)]
pub(crate) struct LibraryMutationReservation {
    pub(crate) ticket: u64,
    pub(crate) turn: tokio::sync::oneshot::Receiver<LibraryMutationTurn>,
}

impl LibraryMutationReservation {
    pub(crate) async fn enter(self) -> Result<LibraryMutationTurn> {
        self.turn.await.with_context(|| {
            format!(
                "library mutation turnstile stopped before ticket {} was admitted",
                self.ticket
            )
        })
    }
}

#[derive(Debug)]
pub(crate) struct LibraryMutationTurn {
    pub(crate) completed: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for LibraryMutationTurn {
    fn drop(&mut self) {
        if let Some(completed) = self.completed.take() {
            let _ = completed.send(());
        }
    }
}

pub(crate) async fn run_library_mutation_turnstile(
    mut requests: tokio::sync::mpsc::UnboundedReceiver<LibraryMutationRequest>,
) {
    let mut next_ticket = 0_u64;
    let mut pending = BTreeMap::new();

    while let Some(request) = requests.recv().await {
        let replaced = pending.insert(request.ticket, request.ready);
        debug_assert!(replaced.is_none(), "library mutation ticket was reused");

        while let Some(ready) = pending.remove(&next_ticket) {
            next_ticket = next_ticket.wrapping_add(1);
            let (completed, completion) = tokio::sync::oneshot::channel();
            let turn = LibraryMutationTurn {
                completed: Some(completed),
            };

            // A request may be explicitly aborted while queued. In that case
            // sending the turn returns it to us; dropping it releases the
            // dispatcher immediately and does not leave a ticket-sized hole.
            if ready.send(turn).is_ok() {
                let granted = std::time::Instant::now();
                let _ = completion.await;
                // A turn that outlives several seconds is what makes the whole
                // library look frozen: every later request is still waiting for
                // admission and none of them has reported a failure. Name the
                // ticket so the log shows which operation was holding the queue.
                let held = granted.elapsed();
                if held >= HOLDING_TURN_WARNING {
                    tracing::warn!(
                        target: "ngy_library",
                        ticket = next_ticket.wrapping_sub(1),
                        held_ms = held.as_millis() as u64,
                        "书库写入：上一次写入占用许可超过 {} 秒，后续书库操作一直在排队",
                        HOLDING_TURN_WARNING.as_secs()
                    );
                }
            }
        }
    }
}
