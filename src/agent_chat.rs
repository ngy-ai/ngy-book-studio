//! Persistent per-window conversations backed by the read-only agent runtime.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context as _, Result, bail, ensure};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::{
    agent::{AgentAnswer, SearchBackend, SelectionSnapshot},
    agent_runtime::{AgentCancellation, AgentQuestion, AgentRunEvent, AgentRuntime},
    ai::{ChatMessage, ChatRole},
    chat::{
        ChatRepository, ChatScope, ChatSession, ChatThread, ChatWindowKind, NewChatCitation,
        NewChatMessage, NewChatThread, StoredChatMessage,
    },
    document::{DocumentLocator, SourceLocator},
    library::LibraryStore,
    services::AppServices,
};

const HISTORY_MESSAGES: usize = 96;
const HISTORY_BYTES: usize = 384 * 1024;
const CREATE_CLAIM_ATTEMPTS: usize = 8;

static NEXT_CONVERSATION_OWNER: AtomicU64 = AtomicU64::new(1);
static THREAD_CLAIMS: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, u64>>>> = OnceLock::new();

#[derive(Debug)]
struct ConversationLease {
    database_path: PathBuf,
    owner: u64,
    thread_id: Mutex<Option<String>>,
}

#[derive(Debug)]
struct ConversationClaimReservation {
    database_path: PathBuf,
    owner: u64,
    thread_id: String,
    temporary: bool,
}

impl ConversationLease {
    fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
            owner: NEXT_CONVERSATION_OWNER.fetch_add(1, Ordering::Relaxed),
            thread_id: Mutex::new(None),
        }
    }

    /// Claims `thread_id` and releases this lease's previous claim atomically.
    ///
    /// A failed switch leaves the previous claim untouched, so a window never
    /// loses its current conversation merely because another window already
    /// owns the requested one.
    fn switch_claim(&self, thread_id: &str) -> Result<bool> {
        let mut claims = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation claim lock is poisoned"))?;
        let mut selected = self
            .thread_id
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation lease lock is poisoned"))?;
        let database_claims = claims.entry(self.database_path.clone()).or_default();
        match database_claims.get(thread_id) {
            Some(owner) if *owner != self.owner => return Ok(false),
            Some(_) => {}
            None => {
                database_claims.insert(thread_id.to_string(), self.owner);
            }
        }
        if let Some(previous) = selected.as_deref()
            && previous != thread_id
            && database_claims.get(previous) == Some(&self.owner)
        {
            database_claims.remove(previous);
        }
        *selected = Some(thread_id.to_string());
        Ok(true)
    }

    fn clear_claim(&self) -> Result<()> {
        let mut claims = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation claim lock is poisoned"))?;
        let mut selected = self
            .thread_id
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation lease lock is poisoned"))?;
        if let Some(database_claims) = claims.get_mut(&self.database_path) {
            if let Some(thread_id) = selected.as_deref()
                && database_claims.get(thread_id) == Some(&self.owner)
            {
                database_claims.remove(thread_id);
            }
            if database_claims.is_empty() {
                claims.remove(&self.database_path);
            }
        }
        *selected = None;
        Ok(())
    }

    /// Temporarily claims `thread_id` without releasing this lease's selected
    /// conversation. Callers can inspect and authorize the target while every
    /// other window is excluded, then either promote or drop the reservation.
    /// A thread already claimed by this lease needs no additional registry
    /// entry.
    fn reserve_claim(&self, thread_id: &str) -> Result<Option<ConversationClaimReservation>> {
        let mut claims = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation claim lock is poisoned"))?;
        let database_claims = claims.entry(self.database_path.clone()).or_default();
        let temporary = match database_claims.get(thread_id) {
            Some(owner) if *owner != self.owner => return Ok(None),
            Some(_) => false,
            None => {
                database_claims.insert(thread_id.to_string(), self.owner);
                true
            }
        };
        Ok(Some(ConversationClaimReservation {
            database_path: self.database_path.clone(),
            owner: self.owner,
            thread_id: thread_id.to_string(),
            temporary,
        }))
    }
}

impl ConversationClaimReservation {
    /// Promotes a temporary reservation to this lease's selected conversation.
    /// The old selected claim is released only after the caller has loaded and
    /// authorized the new thread while this reservation excludes deletion.
    fn promote_to_selection(&mut self, lease: &ConversationLease) -> Result<()> {
        ensure!(
            self.database_path == lease.database_path && self.owner == lease.owner,
            "conversation reservation belongs to another window"
        );
        ensure!(
            lease.switch_claim(&self.thread_id)?,
            "conversation reservation was lost before selection"
        );
        self.temporary = false;
        Ok(())
    }
}

impl Drop for ConversationClaimReservation {
    fn drop(&mut self) {
        if !self.temporary {
            return;
        }
        let Ok(mut claims) = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        else {
            return;
        };
        if let Some(database_claims) = claims.get_mut(&self.database_path) {
            if database_claims.get(&self.thread_id) == Some(&self.owner) {
                database_claims.remove(&self.thread_id);
            }
            if database_claims.is_empty() {
                claims.remove(&self.database_path);
            }
        }
    }
}

impl Drop for ConversationLease {
    fn drop(&mut self) {
        let Ok(thread_id) = self.thread_id.get_mut() else {
            return;
        };
        let Some(thread_id) = thread_id.take() else {
            return;
        };
        let Ok(mut claims) = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        else {
            return;
        };
        if let Some(database_claims) = claims.get_mut(&self.database_path) {
            if database_claims.get(&thread_id) == Some(&self.owner) {
                database_claims.remove(&thread_id);
            }
            if database_claims.is_empty() {
                claims.remove(&self.database_path);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConversationQuestion {
    pub request_id: u64,
    pub question: String,
    pub allowed_book_ids: Vec<String>,
    pub snapshots: Vec<SelectionSnapshot>,
}

#[derive(Clone, Debug)]
pub struct ConversationAnswer {
    pub thread_id: String,
    pub answer: AgentAnswer,
    pub stored_message: StoredChatMessage,
}

/// A host-owned request lease registered before any asynchronous preparation.
///
/// Keeping this value alive makes the request visible to [`AgentConversation::cancel`]
/// while editor references are still being frozen. Dropping it cancels and
/// unregisters only this exact request generation.
#[must_use = "a prepared AI request must be executed or explicitly dropped"]
pub struct PreparedAgentRequest {
    request_id: u64,
    cancellation: AgentCancellation,
    active: Arc<Mutex<HashMap<u64, AgentCancellation>>>,
}

impl PreparedAgentRequest {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn cancellation(&self) -> AgentCancellation {
        self.cancellation.clone()
    }
}

impl std::fmt::Debug for PreparedAgentRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedAgentRequest")
            .field("request_id", &self.request_id)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Drop for PreparedAgentRequest {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if active
            .get(&self.request_id)
            .is_some_and(|token| token.same_request(&self.cancellation))
        {
            active.remove(&self.request_id);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConversationSelection {
    /// No restoration decision has been made for this window yet.
    Uninitialized,
    /// The user explicitly requested a clean conversation.
    Fresh,
    /// This window owns the persisted thread through `ConversationLease`.
    Selected(String),
}

/// Applies the in-memory side of a deletion that SQLite has already committed.
///
/// Releasing a process-local claim can fail only when its mutex is poisoned.
/// That must remain observable, but it cannot turn an irreversible, committed
/// delete into a reported failure or leave the UI displaying the deleted
/// conversation as selected.
fn finalize_committed_delete(
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
            %error,
            "conversation was deleted but its process-local claim could not be released"
        );
    }
    None
}

struct SessionTransitionGuard {
    active: Arc<AtomicBool>,
}

impl Drop for SessionTransitionGuard {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

/// State owned by one Library, Reader or Editor window.
///
/// Clones refer to the same persisted thread and cancellation set. The host
/// computes `allowed_book_ids` from current window state on every submit.
#[derive(Clone)]
pub struct AgentConversation {
    services: Arc<AppServices>,
    window_kind: ChatWindowKind,
    primary_book_id: Option<String>,
    selection: Arc<AsyncMutex<ConversationSelection>>,
    lease: Arc<ConversationLease>,
    active: Arc<Mutex<HashMap<u64, AgentCancellation>>>,
    session_transition: Arc<AtomicBool>,
}

impl std::fmt::Debug for AgentConversation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentConversation")
            .field("window_kind", &self.window_kind)
            .field("primary_book_id", &self.primary_book_id)
            .finish_non_exhaustive()
    }
}

impl AgentConversation {
    pub fn new(
        services: Arc<AppServices>,
        window_kind: ChatWindowKind,
        primary_book_id: Option<String>,
    ) -> Result<Self> {
        match window_kind {
            ChatWindowKind::Library => ensure!(
                primary_book_id.is_none(),
                "library conversation cannot have a primary book"
            ),
            ChatWindowKind::Reader | ChatWindowKind::Editor => ensure!(
                primary_book_id
                    .as_ref()
                    .is_some_and(|id| !id.trim().is_empty()),
                "Reader/Editor conversation requires a primary book"
            ),
        }
        let lease = Arc::new(ConversationLease::new(services.database_path()));
        Ok(Self {
            services,
            window_kind,
            primary_book_id,
            selection: Arc::new(AsyncMutex::new(ConversationSelection::Uninitialized)),
            lease,
            active: Arc::new(Mutex::new(HashMap::new())),
            session_transition: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Restores the most recently used conversation for this exact window
    /// kind/current book whose persisted scope remains inside the current
    /// host-authorized scope. A missing session is normal before the first
    /// submit.
    pub async fn restore(&self, allowed_book_ids: &[String]) -> Result<Option<ChatSession>> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        match selection.clone() {
            ConversationSelection::Fresh => return Ok(None),
            ConversationSelection::Selected(thread_id) => {
                if let Some(session) = repository.session(&thread_id).await? {
                    self.validate_session_context(&session.thread)?;
                    if self.thread_scope_is_authorized(&session.thread, &host_scope) {
                        return Ok(Some(session));
                    }
                }
                self.lease.clear_claim()?;
                *selection = ConversationSelection::Uninitialized;
            }
            ConversationSelection::Uninitialized => {}
        }
        let session = self.claim_recent_session(&repository, &host_scope).await?;
        *selection = session
            .as_ref()
            .map_or(ConversationSelection::Fresh, |session| {
                ConversationSelection::Selected(session.thread.id.clone())
            });
        Ok(session)
    }

    /// Lists persisted sessions that belong to this window kind/current book
    /// and whose scope is fully contained by the current host authorization.
    pub async fn list_sessions(&self, allowed_book_ids: &[String]) -> Result<Vec<ChatThread>> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let threads = self
            .services
            .chat()
            .list_threads(self.window_kind, self.primary_book_id.as_deref())
            .await?;
        let mut authorized = Vec::with_capacity(threads.len());
        for thread in threads {
            self.validate_session_context(&thread)?;
            if self.thread_scope_is_authorized(&thread, &host_scope) {
                authorized.push(thread);
            }
        }
        Ok(authorized)
    }

    /// Selects one persisted session without releasing the current session if
    /// another live window already owns the requested thread.
    pub async fn select_session(
        &self,
        thread_id: &str,
        allowed_book_ids: &[String],
    ) -> Result<ChatSession> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        let mut reservation = self
            .lease
            .reserve_claim(thread_id)?
            .context("conversation is already open in another window")?;
        let session = repository
            .session(thread_id)
            .await?
            .context("conversation does not exist")?;
        self.validate_session_context(&session.thread)?;
        ensure!(
            self.thread_scope_is_authorized(&session.thread, &host_scope),
            "conversation scope exceeds the current host-authorized scope"
        );
        reservation.promote_to_selection(&self.lease)?;
        *selection = ConversationSelection::Selected(session.thread.id.clone());
        Ok(session)
    }

    /// Starts a clean conversation. The database thread is created lazily on
    /// first submit so repeated clicks do not leave empty persisted sessions.
    pub async fn start_new_session(&self) -> Result<()> {
        let _transition = self.begin_session_transition()?;
        let mut selection = self.selection.lock().await;
        self.lease.clear_claim()?;
        *selection = ConversationSelection::Fresh;
        Ok(())
    }

    /// Permanently deletes one authorized persisted session.
    ///
    /// Deleting the selected session releases its window claim and leaves this
    /// conversation in the same explicit fresh state as
    /// [`Self::start_new_session`]. Deleting another session preserves and
    /// returns the currently selected session so callers can keep displaying
    /// its transcript. A temporary claim closes the race between checking an
    /// inactive session and deleting it; sessions open in another window are
    /// rejected.
    pub async fn delete_session(
        &self,
        thread_id: &str,
        allowed_book_ids: &[String],
    ) -> Result<Option<ChatSession>> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        let _reservation = self
            .lease
            .reserve_claim(thread_id)?
            .context("conversation is already open in another window")?;
        let target = repository
            .session(thread_id)
            .await?
            .context("conversation does not exist")?;
        self.validate_session_context(&target.thread)?;
        ensure!(
            self.thread_scope_is_authorized(&target.thread, &host_scope),
            "conversation scope exceeds the current host-authorized scope"
        );
        let deleted_selected = matches!(
            &*selection,
            ConversationSelection::Selected(active) if active == &target.thread.id
        );

        // Resolve every fallible read and authorization decision before the
        // irreversible DELETE. The selected conversation cannot change while
        // the transition/selection guards are held, and another window cannot
        // own it because this lease retains its active claim.
        let (reset_to_fresh, preserved_session) = if deleted_selected {
            (true, None)
        } else if let ConversationSelection::Selected(active_thread_id) = selection.clone() {
            match repository.session(&active_thread_id).await? {
                Some(active_session) => {
                    self.validate_session_context(&active_session.thread)?;
                    if self.thread_scope_is_authorized(&active_session.thread, &host_scope) {
                        (false, Some(active_session))
                    } else {
                        (true, None)
                    }
                }
                None => (true, None),
            }
        } else {
            (false, None)
        };

        ensure!(
            repository.delete_thread(&target.thread.id).await?,
            "conversation no longer exists"
        );
        Ok(finalize_committed_delete(
            &mut selection,
            reset_to_fresh,
            preserved_session,
            || self.lease.clear_claim(),
        ))
    }

    /// Drops the selected conversation when its persisted authority exceeds
    /// the host's current authority. The session transition guard keeps this
    /// decision mutually exclusive with active requests and other session
    /// changes, while the selection lock makes releasing the claim and
    /// switching to a fresh transcript one state transition.
    pub async fn reconcile_scope(&self, allowed_book_ids: &[String]) -> Result<bool> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        let ConversationSelection::Selected(thread_id) = selection.clone() else {
            return Ok(false);
        };
        let session = repository.session(&thread_id).await?;
        if let Some(session) = session {
            self.validate_session_context(&session.thread)?;
            if self.thread_scope_is_authorized(&session.thread, &host_scope) {
                return Ok(false);
            }
        }

        self.lease.clear_claim()?;
        *selection = ConversationSelection::Fresh;
        Ok(true)
    }

    pub async fn selected_thread_id(&self) -> Option<String> {
        match &*self.selection.lock().await {
            ConversationSelection::Selected(thread_id) => Some(thread_id.clone()),
            ConversationSelection::Uninitialized | ConversationSelection::Fresh => None,
        }
    }

    fn begin_session_transition(&self) -> Result<SessionTransitionGuard> {
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

    fn validate_session_context(&self, thread: &ChatThread) -> Result<()> {
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

    fn host_scope(&self, allowed_book_ids: &[String]) -> Result<ChatScope> {
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

    fn thread_scope_is_authorized(&self, thread: &ChatThread, host_scope: &ChatScope) -> bool {
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
        active.insert(request_id, cancellation.clone());
        drop(active);
        Ok(PreparedAgentRequest {
            request_id,
            cancellation,
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
        ensure_request_not_cancelled(&prepared.cancellation)?;

        self.ask_inner(request, events, prepared.cancellation.clone())
            .await
    }

    async fn ask_inner(
        &self,
        request: ConversationQuestion,
        events: Option<mpsc::UnboundedSender<AgentRunEvent>>,
        cancellation: AgentCancellation,
    ) -> Result<ConversationAnswer> {
        ensure_request_not_cancelled(&cancellation)?;
        let scope = ChatScope::new(request.allowed_book_ids.iter().cloned())?;
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

        // Resolve frozen editor text against the single shared library before
        // it reaches the provider. This is the trust transition that supplies
        // current titles/revisions and issues a host citation ID; raw snapshot
        // JSON alone is never a persistable source.
        let snapshot_scope = scope.clone();
        let snapshot_cancellation = cancellation.clone();
        let snapshots = self
            .services
            .spawn_library_read(move |library| {
                ensure_request_not_cancelled(&snapshot_cancellation)?;
                authorize_selection_snapshots(library, &snapshot_scope, request.snapshots)
            })
            .await
            .context("frozen selection authorization worker stopped")??;
        ensure_request_not_cancelled(&cancellation)?;

        let repository = self.services.chat();
        let (thread_id, previous) = self
            .thread_and_history(&repository, &request.question, scope.clone())
            .await?;
        ensure_request_not_cancelled(&cancellation)?;
        let mut user_draft = NewChatMessage::text(ChatRole::User, request.question.clone());
        user_draft.parent_id = previous.last().map(|message| message.id.clone());
        ensure_request_not_cancelled(&cancellation)?;
        let user_message = repository
            .append_message(&thread_id, user_draft)
            .await
            .context("failed to persist the user question")?;
        ensure_request_not_cancelled(&cancellation)?;

        let settings = self.services.provider_settings()?;
        let provider = self.services.provider()?;
        let search = self.services.search()?;
        let search_backend: Arc<dyn SearchBackend> = search;
        let runtime = AgentRuntime::for_database(
            provider,
            search_backend,
            self.services.database_path(),
            settings.chat_model.clone(),
        )?;
        ensure_request_not_cancelled(&cancellation)?;
        let answer = runtime
            .answer(
                AgentQuestion {
                    question: request.question,
                    allowed_book_ids: scope.book_ids,
                    history: history_messages(&previous),
                    snapshots,
                },
                events,
                cancellation.clone(),
            )
            .await?;
        ensure_request_not_cancelled(&cancellation)?;

        let citations = answer
            .citations
            .iter()
            .map(|citation| NewChatCitation {
                content_unit_id: Some(citation.unit_id.clone()),
                search_chunk_id: citation
                    .citation_id
                    .strip_prefix("passage:")
                    .map(str::to_string),
                document_revision: citation.document_revision,
                unit_revision: citation.unit_revision,
                quote: citation.quote.clone(),
                locator: citation.locator.clone(),
            })
            .collect();
        let stored_message = repository
            .append_message(
                &thread_id,
                NewChatMessage {
                    parent_id: Some(user_message.id),
                    role: ChatRole::Assistant,
                    content: answer.markdown.clone(),
                    model: Some(settings.chat_model),
                    citations,
                },
            )
            .await
            .context("failed to persist the AI answer")?;

        Ok(ConversationAnswer {
            thread_id,
            answer,
            stored_message,
        })
    }

    async fn thread_and_history(
        &self,
        repository: &ChatRepository,
        question: &str,
        scope: ChatScope,
    ) -> Result<(String, Vec<StoredChatMessage>)> {
        let mut selection = self.selection.lock().await;
        let existing = match selection.clone() {
            ConversationSelection::Selected(thread_id) => {
                let session = repository.session(&thread_id).await?;
                if let Some(session) = session.as_ref() {
                    self.validate_session_context(&session.thread)?;
                } else {
                    self.lease.clear_claim()?;
                    *selection = ConversationSelection::Fresh;
                }
                session
            }
            ConversationSelection::Uninitialized => {
                let session = self.claim_recent_session(repository, &scope).await?;
                *selection = session
                    .as_ref()
                    .map_or(ConversationSelection::Fresh, |session| {
                        ConversationSelection::Selected(session.thread.id.clone())
                    });
                session
            }
            ConversationSelection::Fresh => None,
        };
        if let Some(session) = existing {
            if scope_excludes_prior_books(&session.thread.scope, &scope) {
                // Messages do not carry a per-message authority snapshot. If
                // the host removes a book, reusing this history would send
                // text derived from that now-unauthorized book back to the
                // provider even though subsequent tools are correctly scoped.
                // Preserve the old conversation as an auditable session and
                // start a clean one for the narrowed authority set.
                self.lease.clear_claim()?;
                *selection = ConversationSelection::Fresh;
            } else {
                let thread = repository
                    .update_thread(&session.thread.id, session.thread.title, scope)
                    .await?;
                *selection = ConversationSelection::Selected(thread.id.clone());
                return Ok((thread.id, session.messages));
            }
        }

        for _ in 0..CREATE_CLAIM_ATTEMPTS {
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: self.primary_book_id.clone(),
                    title: question_title(question),
                    scope: scope.clone(),
                    window_kind: self.window_kind,
                })
                .await?;
            if self.lease.switch_claim(&thread.id)? {
                *selection = ConversationSelection::Selected(thread.id.clone());
                return Ok((thread.id, Vec::new()));
            }
        }
        bail!("could not reserve an independent conversation for this window")
    }

    async fn claim_recent_session(
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
            return false;
        };
        token.cancel();
        true
    }

    pub fn cancel_all(&self) {
        if let Ok(active) = self.active.lock() {
            for token in active.values() {
                token.cancel();
            }
        }
    }
}

fn ensure_request_not_cancelled(cancellation: &AgentCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!("AI request was cancelled");
    }
    Ok(())
}

fn scope_excludes_prior_books(previous: &ChatScope, current: &ChatScope) -> bool {
    previous
        .book_ids
        .iter()
        .any(|book_id| !current.contains(book_id))
}

fn authorize_selection_snapshots(
    library: &LibraryStore,
    scope: &ChatScope,
    snapshots: Vec<SelectionSnapshot>,
) -> Result<Vec<SelectionSnapshot>> {
    snapshots
        .into_iter()
        .map(|snapshot| {
            snapshot.validate().map_err(anyhow::Error::new)?;
            ensure!(
                scope.contains(&snapshot.book_id),
                "frozen selection is outside the host-authorized scope"
            );
            let document = library
                .document(&snapshot.book_id)
                .context("frozen selection book no longer exists")?;
            let unit = document
                .find_unit(&snapshot.unit_id)
                .context("frozen selection content unit no longer exists")?;
            let locator = snapshot.locator.clone().unwrap_or_else(|| {
                let locator = DocumentLocator::unit(&document.id, &unit.id);
                match unit.source_locator.clone() {
                    Some(source) => locator.with_source(source),
                    None => locator,
                }
            });
            if let Some(source) = locator.source.as_ref() {
                ensure!(
                    !matches!(source, SourceLocator::OfficeRenderedPage { .. }),
                    "frozen selection cannot cite a preview-only Office rendered page"
                );
                ensure!(
                    unit.source_locator.as_ref() == Some(source),
                    "frozen selection source locator does not match the current content unit"
                );
            }
            document
                .validate_locator(&locator)
                .context("frozen selection locator is stale or invalid")?;
            snapshot
                .with_host_citation(
                    locator,
                    document.revision,
                    unit.revision,
                    document.title.clone(),
                    unit.title.clone(),
                )
                .map_err(anyhow::Error::new)
        })
        .collect()
}

fn question_title(question: &str) -> String {
    let title = question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect::<String>();
    if title.is_empty() {
        "新对话".to_string()
    } else {
        title
    }
}

fn history_messages(messages: &[StoredChatMessage]) -> Vec<ChatMessage> {
    let mut selected = Vec::new();
    let mut bytes = 0usize;
    for message in messages.iter().rev() {
        if selected.len() >= HISTORY_MESSAGES
            || bytes.saturating_add(message.content.len()) > HISTORY_BYTES
        {
            break;
        }
        if matches!(message.role, ChatRole::User | ChatRole::Assistant) {
            bytes += message.content.len();
            selected.push(ChatMessage::text(message.role, message.content.clone()));
        }
    }
    selected.reverse();
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_selection_is_authorized_from_current_library_metadata() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
        let record = library
            .create_book("Selection book", "Author")
            .expect("create book");
        let document = library.document(&record.id).expect("load document");
        let unit = document.units.first().expect("default chapter");
        let snapshot = SelectionSnapshot::capture(
            document.id.clone(),
            unit.id.clone(),
            23,
            "unsaved selected words",
        )
        .expect("capture selection");
        let scope = ChatScope::new([document.id.clone()]).expect("scope");

        let authorized = authorize_selection_snapshots(&library, &scope, vec![snapshot])
            .expect("authorize selection")
            .pop()
            .unwrap();
        let citation = authorized.citation().expect("selection citation");
        assert!(citation.citation_id.starts_with("selection:"));
        assert_eq!(citation.book_id, document.id);
        assert_eq!(citation.book_title, document.title);
        assert_eq!(citation.unit_id, unit.id);
        assert_eq!(citation.unit_title, unit.title);
        assert_eq!(citation.document_revision, document.revision);
        assert_eq!(citation.unit_revision, unit.revision);
        assert_eq!(citation.quote, "unsaved selected words");
        assert_eq!(citation.locator.source, unit.source_locator);
    }

    #[test]
    fn frozen_selection_authorization_rejects_out_of_scope_or_stale_units() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
        let record = library
            .create_book("Selection book", "Author")
            .expect("create book");
        let document = library.document(&record.id).expect("load document");
        let unit_id = document.units[0].id.clone();
        let snapshot = SelectionSnapshot::capture(&document.id, &unit_id, 1, "selected").unwrap();

        let out_of_scope = ChatScope::new(["some-other-book"]).unwrap();
        assert!(
            authorize_selection_snapshots(&library, &out_of_scope, vec![snapshot.clone()])
                .unwrap_err()
                .to_string()
                .contains("outside the host-authorized scope")
        );

        let scope = ChatScope::new([document.id.clone()]).unwrap();
        let stale =
            SelectionSnapshot::capture(&document.id, "missing-unit", 1, "selected").unwrap();
        assert!(
            authorize_selection_snapshots(&library, &scope, vec![stale])
                .unwrap_err()
                .to_string()
                .contains("content unit no longer exists")
        );
    }

    #[test]
    fn frozen_selection_authorization_rejects_forged_and_preview_only_sources() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
        let record = library
            .create_book("Selection book", "Author")
            .expect("create book");
        let document = library.document(&record.id).expect("load document");
        let unit = document.units.first().expect("default chapter");
        let scope = ChatScope::new([document.id.clone()]).expect("scope");

        let forged = SelectionSnapshot::capture(&document.id, &unit.id, 1, "selected")
            .unwrap()
            .with_locator(
                DocumentLocator::unit(&document.id, &unit.id)
                    .with_source(SourceLocator::pdf_page(1)),
            )
            .unwrap();
        let error = authorize_selection_snapshots(&library, &scope, vec![forged])
            .expect_err("cross-format source must be rejected");
        assert!(error.to_string().contains("does not match"));

        let preview_only = SelectionSnapshot::capture(&document.id, &unit.id, 1, "selected")
            .unwrap()
            .with_locator(
                DocumentLocator::unit(&document.id, &unit.id)
                    .with_source(SourceLocator::office_rendered_page(1)),
            )
            .unwrap();
        let error = authorize_selection_snapshots(&library, &scope, vec![preview_only])
            .expect_err("preview-only source must be rejected");
        assert!(error.to_string().contains("preview-only"));
    }

    #[test]
    fn history_is_bounded_and_keeps_chronological_order() {
        let messages = (0..120)
            .map(|index| StoredChatMessage {
                id: format!("m-{index}"),
                thread_id: "thread".into(),
                parent_id: None,
                ordinal: index,
                role: if index % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                content: format!("message {index}"),
                model: None,
                created_at: 0,
                citations: Vec::new(),
            })
            .collect::<Vec<_>>();
        let history = history_messages(&messages);
        assert_eq!(history.len(), HISTORY_MESSAGES);
        assert_eq!(
            history.first().and_then(|message| message.content.as_ref()),
            Some(&crate::ai::MessageContent::Text("message 24".into()))
        );
    }

    #[test]
    fn title_is_single_line_and_bounded() {
        let title = question_title(&format!("  first\n{}", "x".repeat(100)));
        assert!(!title.contains('\n'));
        assert_eq!(title.chars().count(), 80);
    }

    #[test]
    fn active_windows_do_not_claim_the_same_persisted_thread() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = temp.path().join("library.db");
        let first = ConversationLease::new(&database);
        let second = ConversationLease::new(&database);

        assert!(first.switch_claim("thread-1").expect("first claim"));
        assert!(!second.switch_claim("thread-1").expect("second claim"));
        assert!(second.switch_claim("thread-2").expect("independent claim"));

        drop(first);
        let reopened = ConversationLease::new(&database);
        assert!(
            reopened
                .switch_claim("thread-1")
                .expect("released claim can be restored")
        );
    }

    #[test]
    fn temporary_claim_reservation_blocks_and_then_releases_other_windows() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = temp.path().join("library.db");
        let deleting = ConversationLease::new(&database);
        let other = ConversationLease::new(&database);

        let reservation = deleting
            .reserve_claim("thread-1")
            .expect("reserve thread")
            .expect("unclaimed thread can be reserved");
        assert!(
            !other
                .switch_claim("thread-1")
                .expect("reservation blocks another window")
        );

        drop(reservation);
        assert!(
            other
                .switch_claim("thread-1")
                .expect("dropping reservation releases the thread")
        );
    }

    #[test]
    fn selection_reservation_preserves_the_old_claim_until_promotion() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = temp.path().join("library.db");
        let selecting = ConversationLease::new(&database);
        let other = ConversationLease::new(&database);

        assert!(selecting.switch_claim("thread-old").expect("old claim"));
        let mut reservation = selecting
            .reserve_claim("thread-new")
            .expect("reserve new thread")
            .expect("new thread is unclaimed");
        assert!(
            other
                .reserve_claim("thread-new")
                .expect("competing reservation")
                .is_none(),
            "a delete cannot pass the selection reservation"
        );
        assert!(
            !other
                .switch_claim("thread-old")
                .expect("old thread remains claimed"),
            "validation must not release the previously selected conversation"
        );

        reservation
            .promote_to_selection(&selecting)
            .expect("promote selection");
        drop(reservation);
        assert!(
            other
                .switch_claim("thread-old")
                .expect("promotion releases old claim")
        );
        assert!(
            other
                .reserve_claim("thread-new")
                .expect("new thread remains claimed")
                .is_none(),
            "dropping a promoted reservation must not release the selected thread"
        );
    }

    #[test]
    fn committed_delete_cleanup_error_cannot_change_success_into_failure() {
        let mut selection = ConversationSelection::Selected("deleted-thread".to_string());
        let session = finalize_committed_delete(&mut selection, true, None, || {
            Err(anyhow::anyhow!("forced claim cleanup failure"))
        });

        assert!(session.is_none());
        assert_eq!(selection, ConversationSelection::Fresh);
    }

    #[test]
    fn fresh_selection_does_not_reuse_a_recent_session() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                .expect("create conversation");

        runtime.block_on(async {
            let repository = services.chat();
            let recent = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "Recent".to_string(),
                    scope: ChatScope::default(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            assert_eq!(
                conversation.restore(&[]).await.unwrap().unwrap().thread.id,
                recent.id
            );

            conversation.start_new_session().await.unwrap();
            assert_eq!(conversation.selected_thread_id().await, None);
            assert!(conversation.restore(&[]).await.unwrap().is_none());

            let (fresh, history) = conversation
                .thread_and_history(&repository, "Start clean", ChatScope::default())
                .await
                .unwrap();
            assert_ne!(fresh, recent.id);
            assert!(history.is_empty());
            assert_eq!(conversation.selected_thread_id().await, Some(fresh));
        });
    }

    #[test]
    fn library_group_scope_filters_restore_list_select_and_reconcile() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let group_a = vec!["book-a".to_string()];
            let group_a_and_b = vec!["book-a".to_string(), "book-b".to_string()];
            let broad = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "A and B".to_string(),
                    scope: ChatScope::new(group_a_and_b.clone()).unwrap(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            let conversation =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();

            assert!(
                conversation.restore(&group_a).await.unwrap().is_none(),
                "a recent conversation outside the current group must be skipped"
            );
            assert!(
                conversation
                    .list_sessions(&group_a)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let error = conversation
                .select_session(&broad.id, &group_a)
                .await
                .expect_err("a broader conversation must not be selectable");
            assert!(error.to_string().contains("host-authorized scope"));

            conversation
                .select_session(&broad.id, &group_a_and_b)
                .await
                .unwrap();
            assert!(!conversation.reconcile_scope(&group_a_and_b).await.unwrap());
            assert!(conversation.reconcile_scope(&group_a).await.unwrap());
            assert_eq!(conversation.selected_thread_id().await, None);

            let other_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            other_window
                .select_session(&broad.id, &group_a_and_b)
                .await
                .expect("scope reconciliation must release the old claim");

            let narrow = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "A only".to_string(),
                    scope: ChatScope::new(group_a.clone()).unwrap(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            let visible = conversation.list_sessions(&group_a).await.unwrap();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].id, narrow.id);

            let restore_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            assert_eq!(
                restore_window
                    .restore(&group_a)
                    .await
                    .unwrap()
                    .unwrap()
                    .thread
                    .id,
                narrow.id
            );
        });
    }

    #[test]
    fn switching_sessions_releases_the_previous_claim() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let create = |title: &str| NewChatThread {
                primary_book_id: None,
                title: title.to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            };
            let first = repository.create_thread(create("A")).await.unwrap();
            let second = repository.create_thread(create("B")).await.unwrap();
            let first_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            let second_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();

            first_window.select_session(&first.id, &[]).await.unwrap();
            first_window.select_session(&second.id, &[]).await.unwrap();
            second_window
                .select_session(&first.id, &[])
                .await
                .expect("switching to B must release A");

            assert_eq!(
                first_window.selected_thread_id().await.as_deref(),
                Some(second.id.as_str())
            );
            assert_eq!(
                second_window.selected_thread_id().await.as_deref(),
                Some(first.id.as_str())
            );
        });
    }

    #[test]
    fn switching_sessions_restores_only_the_selected_history() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let create = |title: &str| NewChatThread {
                primary_book_id: None,
                title: title.to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            };
            let first = repository.create_thread(create("First")).await.unwrap();
            let second = repository.create_thread(create("Second")).await.unwrap();
            repository
                .append_message(
                    &first.id,
                    NewChatMessage::text(ChatRole::User, "first question"),
                )
                .await
                .unwrap();
            repository
                .append_message(
                    &second.id,
                    NewChatMessage::text(ChatRole::Assistant, "second answer"),
                )
                .await
                .unwrap();

            let conversation =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            let first_session = conversation.select_session(&first.id, &[]).await.unwrap();
            assert_eq!(first_session.messages.len(), 1);
            assert_eq!(first_session.messages[0].content, "first question");

            let second_session = conversation.select_session(&second.id, &[]).await.unwrap();
            assert_eq!(second_session.messages.len(), 1);
            assert_eq!(second_session.messages[0].content, "second answer");
            assert_eq!(conversation.list_sessions(&[]).await.unwrap().len(), 2);
        });
    }

    #[test]
    fn deleting_selected_session_starts_a_fresh_conversation() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "Delete current".to_string(),
                    scope: ChatScope::default(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            repository
                .append_message(
                    &thread.id,
                    NewChatMessage::text(ChatRole::User, "do not keep this"),
                )
                .await
                .unwrap();
            let conversation =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            conversation.select_session(&thread.id, &[]).await.unwrap();

            assert!(
                conversation
                    .delete_session(&thread.id, &[])
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(conversation.selected_thread_id().await, None);
            assert!(repository.session(&thread.id).await.unwrap().is_none());
            assert!(
                conversation.restore(&[]).await.unwrap().is_none(),
                "an explicit deletion must not silently restore another conversation"
            );
        });
    }

    #[test]
    fn deleting_inactive_session_preserves_the_selected_transcript_and_claim() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let create = |title: &str| NewChatThread {
                primary_book_id: None,
                title: title.to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            };
            let selected = repository.create_thread(create("Selected")).await.unwrap();
            let inactive = repository.create_thread(create("Inactive")).await.unwrap();
            repository
                .append_message(
                    &selected.id,
                    NewChatMessage::text(ChatRole::User, "keep this transcript"),
                )
                .await
                .unwrap();
            let conversation =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            conversation
                .select_session(&selected.id, &[])
                .await
                .unwrap();

            let still_selected = conversation
                .delete_session(&inactive.id, &[])
                .await
                .unwrap()
                .expect("the selected session remains active");
            assert_eq!(still_selected.thread.id, selected.id);
            assert_eq!(still_selected.messages.len(), 1);
            assert_eq!(still_selected.messages[0].content, "keep this transcript");
            assert_eq!(
                conversation.selected_thread_id().await.as_deref(),
                Some(selected.id.as_str())
            );
            assert!(repository.session(&inactive.id).await.unwrap().is_none());

            let other_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            assert!(
                other_window
                    .select_session(&selected.id, &[])
                    .await
                    .is_err(),
                "deleting an inactive session must not release the active claim"
            );
        });
    }

    #[test]
    fn deleting_session_claimed_by_another_window_is_rejected() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "Claimed".to_string(),
                    scope: ChatScope::default(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            let owner =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            let deleting_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            owner.select_session(&thread.id, &[]).await.unwrap();

            let error = deleting_window
                .delete_session(&thread.id, &[])
                .await
                .expect_err("another window owns this conversation");
            assert!(error.to_string().contains("another window"));
            assert!(repository.session(&thread.id).await.unwrap().is_some());
            assert_eq!(
                owner.selected_thread_id().await.as_deref(),
                Some(thread.id.as_str())
            );
            assert_eq!(deleting_window.selected_thread_id().await, None);
        });
    }

    #[test]
    fn deleting_session_outside_the_host_scope_is_rejected() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "Broad scope".to_string(),
                    scope: ChatScope::new(["book-a", "book-b"]).unwrap(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            let conversation =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();

            let error = conversation
                .delete_session(&thread.id, &["book-a".to_string()])
                .await
                .expect_err("a narrower host scope cannot delete broader history");
            assert!(error.to_string().contains("host-authorized scope"));
            assert!(repository.session(&thread.id).await.unwrap().is_some());
            assert_eq!(conversation.selected_thread_id().await, None);
        });
    }

    #[test]
    fn deleting_missing_session_is_rejected_without_changing_selection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let conversation = AgentConversation::new(services, ChatWindowKind::Library, None).unwrap();

        let error = runtime
            .block_on(conversation.delete_session("missing-thread", &[]))
            .expect_err("a missing conversation cannot be deleted");
        assert!(error.to_string().contains("does not exist"));
        assert_eq!(
            runtime.block_on(conversation.selected_thread_id()),
            None,
            "a failed deletion must not initialize or replace the selection"
        );
    }

    #[test]
    fn failed_switch_to_claimed_session_keeps_the_previous_claim() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let repository = services.chat();
            let create = |title: &str| NewChatThread {
                primary_book_id: None,
                title: title.to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
            };
            let first = repository.create_thread(create("A")).await.unwrap();
            let second = repository.create_thread(create("B")).await.unwrap();
            let first_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            let second_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();
            let third_window =
                AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                    .unwrap();

            first_window.select_session(&first.id, &[]).await.unwrap();
            second_window.select_session(&second.id, &[]).await.unwrap();
            let error = first_window
                .select_session(&second.id, &[])
                .await
                .expect_err("B is owned by the second window");
            assert!(error.to_string().contains("another window"));
            assert_eq!(
                first_window.selected_thread_id().await.as_deref(),
                Some(first.id.as_str())
            );
            assert!(
                third_window.select_session(&first.id, &[]).await.is_err(),
                "A must remain claimed after the failed switch"
            );
        });
    }

    #[test]
    fn selecting_and_deleting_reject_the_wrong_window_kind_or_primary_book() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
        let first_book = library.create_book("First", "").unwrap().id;
        let second_book = library.create_book("Second", "").unwrap().id;
        drop(library);
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let thread = services
                .chat()
                .create_thread(NewChatThread {
                    primary_book_id: Some(first_book.clone()),
                    title: "Reader chat".to_string(),
                    scope: ChatScope::new([first_book.clone()]).unwrap(),
                    window_kind: ChatWindowKind::Reader,
                })
                .await
                .unwrap();
            let first_scope = vec![first_book.clone()];
            let second_scope = vec![second_book.clone()];
            let editor = AgentConversation::new(
                Arc::clone(&services),
                ChatWindowKind::Editor,
                Some(first_book.clone()),
            )
            .unwrap();
            let other_reader = AgentConversation::new(
                Arc::clone(&services),
                ChatWindowKind::Reader,
                Some(second_book.clone()),
            )
            .unwrap();

            let error = editor
                .select_session(&thread.id, &first_scope)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("window kind"));
            let error = other_reader
                .select_session(&thread.id, &second_scope)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("primary book"));
            let error = editor
                .delete_session(&thread.id, &first_scope)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("window kind"));
            let error = other_reader
                .delete_session(&thread.id, &second_scope)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("primary book"));
            assert_eq!(editor.selected_thread_id().await, None);
            assert_eq!(other_reader.selected_thread_id().await, None);
            assert!(services.chat().session(&thread.id).await.unwrap().is_some());
        });
    }

    #[test]
    fn reader_scope_narrowing_rejects_restore_list_and_selection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
        let book_a = library.create_book("Book A", "").unwrap().id;
        let book_b = library.create_book("Book B", "").unwrap().id;
        drop(library);
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        runtime.block_on(async {
            let allowed_a = vec![book_a.clone()];
            let allowed_b = vec![book_b.clone()];
            let allowed_a_and_b = vec![book_a.clone(), book_b.clone()];
            let broad = services
                .chat()
                .create_thread(NewChatThread {
                    primary_book_id: Some(book_a.clone()),
                    title: "Reader A and B".to_string(),
                    scope: ChatScope::new(allowed_a_and_b.clone()).unwrap(),
                    window_kind: ChatWindowKind::Reader,
                })
                .await
                .unwrap();
            let conversation = AgentConversation::new(
                Arc::clone(&services),
                ChatWindowKind::Reader,
                Some(book_a.clone()),
            )
            .unwrap();

            assert!(conversation.restore(&allowed_a).await.unwrap().is_none());
            assert!(
                conversation
                    .list_sessions(&allowed_a)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let error = conversation
                .select_session(&broad.id, &allowed_a)
                .await
                .expect_err("narrowed Reader scope must reject A/B history");
            assert!(error.to_string().contains("host-authorized scope"));

            for error in [
                conversation.restore(&allowed_b).await.unwrap_err(),
                conversation.list_sessions(&allowed_b).await.unwrap_err(),
                conversation
                    .select_session(&broad.id, &allowed_b)
                    .await
                    .unwrap_err(),
            ] {
                assert!(error.to_string().contains("required current book"));
            }

            let selected = conversation
                .select_session(&broad.id, &allowed_a_and_b)
                .await
                .unwrap();
            assert_eq!(selected.thread.id, broad.id);
        });
    }

    #[test]
    fn active_request_blocks_session_changes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                .expect("create conversation");

        runtime.block_on(async {
            let thread = services
                .chat()
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "Existing".to_string(),
                    scope: ChatScope::default(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            conversation.select_session(&thread.id, &[]).await.unwrap();
            let prepared = conversation.prepare_request(97).unwrap();

            assert!(conversation.start_new_session().await.is_err());
            assert!(conversation.select_session(&thread.id, &[]).await.is_err());
            assert!(conversation.delete_session(&thread.id, &[]).await.is_err());
            assert!(conversation.reconcile_scope(&[]).await.is_err());
            assert!(
                services.chat().session(&thread.id).await.unwrap().is_some(),
                "a blocked deletion must leave the persisted session intact"
            );
            assert_eq!(
                conversation.selected_thread_id().await.as_deref(),
                Some(thread.id.as_str())
            );

            drop(prepared);
            conversation.start_new_session().await.unwrap();
            assert_eq!(conversation.selected_thread_id().await, None);
        });
    }

    #[test]
    fn prepared_request_can_be_cancelled_before_ask_without_persistence() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                .expect("create conversation");
        let prepared = conversation.prepare_request(41).expect("prepare request");
        let cancellation = prepared.cancellation();

        assert_eq!(prepared.request_id(), 41);
        assert!(conversation.cancel(41));
        assert!(cancellation.is_cancelled());

        let error = runtime
            .block_on(conversation.ask_prepared(
                ConversationQuestion {
                    request_id: 41,
                    question: "must never be persisted or sent".to_string(),
                    allowed_book_ids: Vec::new(),
                    snapshots: Vec::new(),
                },
                None,
                prepared,
            ))
            .expect_err("cancelled preparation must not run");
        assert!(error.to_string().contains("cancelled"));
        assert!(!conversation.cancel(41), "request lease must be removed");

        let threads = runtime
            .block_on(services.chat().list_threads(ChatWindowKind::Library, None))
            .expect("list conversations");
        assert!(
            threads.is_empty(),
            "a cancelled prepared request must not create persisted chat state"
        );
    }

    #[test]
    fn duplicate_prepare_does_not_replace_the_original_cancellation_token() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let conversation = AgentConversation::new(services, ChatWindowKind::Library, None)
            .expect("create conversation");
        let prepared = conversation.prepare_request(73).expect("prepare request");
        let cancellation = prepared.cancellation();

        assert!(conversation.prepare_request(73).is_err());
        assert!(conversation.cancel(73));
        assert!(
            cancellation.is_cancelled(),
            "duplicate registration must leave the original request cancellable"
        );
        drop(prepared);
        assert!(!conversation.cancel(73));
    }

    #[test]
    fn narrowing_scope_starts_a_clean_persisted_conversation() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let conversation = AgentConversation::new(services, ChatWindowKind::Library, None)
            .expect("create conversation");

        runtime.block_on(async {
            let repository = conversation.services.chat();
            let broad_scope = ChatScope::new(["book-a", "book-b"]).unwrap();
            let (broad_thread, history) = conversation
                .thread_and_history(&repository, "Broad question", broad_scope.clone())
                .await
                .unwrap();
            assert!(history.is_empty());
            repository
                .append_message(
                    &broad_thread,
                    NewChatMessage::text(ChatRole::Assistant, "secret from book-b"),
                )
                .await
                .unwrap();

            let narrow_scope = ChatScope::new(["book-a"]).unwrap();
            let (narrow_thread, history) = conversation
                .thread_and_history(&repository, "Narrow question", narrow_scope.clone())
                .await
                .unwrap();
            assert_ne!(narrow_thread, broad_thread);
            assert!(
                history.is_empty(),
                "removed-book history must not be reused"
            );
            assert_eq!(
                repository
                    .thread(&broad_thread)
                    .await
                    .unwrap()
                    .unwrap()
                    .scope,
                broad_scope
            );
            assert_eq!(
                repository
                    .thread(&narrow_thread)
                    .await
                    .unwrap()
                    .unwrap()
                    .scope,
                narrow_scope
            );
        });
    }
}
